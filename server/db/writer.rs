// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Writing recordings and deleting old ones.

use crate::db::{self, CompositeId};
use crate::dir;
use crate::recording::{self, MAX_RECORDING_WALL_DURATION};
use base::clock::{self, Clocks};
use base::shutdown::ShutdownError;
use base::{bail, err, Error, ErrorKind, FastHashMap, Mutex};
use bytes::Bytes;
use std::cmp::{self, Ordering};
use std::convert::TryFrom;
use std::future::Future;
use std::mem;
use std::sync::Arc;
use tracing::{debug, info, info_span, trace, warn, Instrument as _};

/// Trait to allow mocking out [crate::dir::SampleFileDir] in syncer tests.
/// This is public because it's exposed in the [SyncerChannel] type parameters,
/// not because it's of direct use outside this module.
pub trait DirWriter: 'static + Send + Sync {
    type File: FileWriter;

    fn create_file(
        &self,
        id: CompositeId,
    ) -> impl Future<Output = Result<Self::File, Error>> + Send;
    fn sync(&self) -> impl Future<Output = Result<(), base::Error>> + Send;
    fn collect_garbage(
        &self,
        to_unlink: Vec<CompositeId>,
    ) -> impl Future<Output = Result<Vec<CompositeId>, Error>> + Send;
}

/// Trait to allow mocking out [std::fs::File] in syncer tests.
/// This is public because it's exposed in the [SyncerChannel] type parameters,
/// not because it's of direct use outside this module.
pub trait FileWriter: 'static + Send {
    /// As in `std::fs::File::sync_all`.
    fn sync_all(&mut self) -> impl Future<Output = Result<(), Error>> + Send;

    /// Writes some or all of `data` to the file; advances it accordingly.
    fn write(&mut self, data: &mut Bytes) -> impl Future<Output = Result<(), Error>> + Send;
}

impl DirWriter for dir::Pool {
    type File = dir::writer::WriteStream;

    async fn create_file(&self, id: CompositeId) -> Result<Self::File, Error> {
        dir::Pool::create_file(self, id).await
    }
    async fn sync(&self) -> Result<(), base::Error> {
        self.run("sync", |ctx| ctx.sync()).await
    }
    async fn collect_garbage(
        &self,
        to_unlink: Vec<CompositeId>,
    ) -> Result<Vec<CompositeId>, Error> {
        self.collect_garbage(to_unlink).await
    }
}

impl FileWriter for dir::writer::WriteStream {
    async fn sync_all(&mut self) -> Result<(), Error> {
        self.sync_all().await
    }
    async fn write(&mut self, data: &mut Bytes) -> Result<(), Error> {
        self.write(data).await
    }
}

/// A command sent to a [Syncer].
enum SyncerCommand<F> {
    /// Command sent by [SyncerChannel::async_save_recording].
    AsyncSaveRecording(CompositeId, recording::Duration, F),

    /// Command sent by [SyncerChannel::flush].
    Flush(tokio::sync::oneshot::Sender<std::convert::Infallible>),
}

/// A channel which can be used to send commands to the syncer.
/// Can be cloned to allow multiple threads to send commands.
pub struct SyncerChannel<F>(tokio::sync::mpsc::Sender<SyncerCommand<F>>);

impl<F> ::std::clone::Clone for SyncerChannel<F> {
    fn clone(&self) -> Self {
        SyncerChannel(self.0.clone())
    }
}

/// State of the worker thread created by [start_syncer].
struct Syncer<C: Clocks, D: DirWriter> {
    dir_id: i32,
    dir: D,
    db: Arc<db::Database<C>>,
    planned_flushes: std::collections::BinaryHeap<PlannedFlush>,
    db_flush: tokio::sync::watch::Receiver<u64>,
    shutdown_rx: base::shutdown::Receiver,
}

/// A plan to flush at a given instant due to a recently-saved recording's `flush_if_sec` parameter.
struct PlannedFlush {
    /// Monotonic time at which this flush should happen.
    when: base::clock::Instant,

    /// Recording which prompts this flush. If this recording is already flushed at the planned
    /// time, it can be skipped.
    recording: CompositeId,

    /// A human-readable reason for the flush, for logs.
    reason: String,

    /// Senders to drop when this time is reached. This is for test instrumentation; see
    /// [SyncerChannel::flush].
    senders: Vec<tokio::sync::oneshot::Sender<std::convert::Infallible>>,
}

// PlannedFlush is meant for placement in a max-heap which should return the soonest flush. This
// PlannedFlush is greater than other if its when is _less_ than the other's.
impl Ord for PlannedFlush {
    fn cmp(&self, other: &Self) -> Ordering {
        other.when.cmp(&self.when)
    }
}

impl PartialOrd for PlannedFlush {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for PlannedFlush {
    fn eq(&self, other: &Self) -> bool {
        self.when == other.when
    }
}

impl Eq for PlannedFlush {}

/// Starts a syncer for the given sample file directory.
///
/// The lock must not be held on `db` when this is called.
///
/// There should be only one syncer per directory, or 0 if operating in read-only mode.
/// This function will perform the initial rotation synchronously, so that it is finished before
/// file writing starts. Afterward the syncing happens in a background thread.
///
/// Returns a `SyncerChannel` which can be used to send commands (and can be cloned freely) and
/// a `JoinHandle` for the syncer thread. Commands sent on the channel will be executed or retried
/// forever. (TODO: provide some manner of pushback during retry.) At program shutdown, all
/// `SyncerChannel` clones should be dropped and then the handle joined to allow all recordings to
/// be persisted.
pub async fn start_syncer<C: Clocks + Clone>(
    db: Arc<db::Database<C>>,
    shutdown_rx: base::shutdown::Receiver,
    dir_id: i32,
) -> Result<
    (
        SyncerChannel<dir::writer::WriteStream>,
        tokio::task::JoinHandle<()>,
    ),
    Error,
> {
    let mut syncer = Syncer::new(shutdown_rx, db, dir_id).await?;
    syncer.initial_rotation().await?;
    let (snd, mut rcv) = tokio::sync::mpsc::channel(16);
    let span = info_span!("syncer", path = %syncer.path().display());
    Ok((
        SyncerChannel(snd),
        tokio::task::Builder::new()
            .name(&format!("syncer-{path}", path = syncer.path().display()))
            .spawn(
                async move {
                    info!("starting");
                    while syncer.iter(&mut rcv).await {}
                    info!("ending");
                }
                .instrument(span),
            )
            .unwrap(),
    ))
}

/// A new retention limit for use in [lower_retention].
pub struct NewLimit {
    pub stream_id: i32,
    pub limit: i64,
}

/// Immediately deletes recordings if necessary to fit within the given new `retain_bytes` limit.
/// Note this doesn't change the limit in the database; it only deletes files.
/// Pass a limit of 0 to delete all recordings associated with a camera.
///
/// This is expected to be performed from `moonfire-nvr config` when no syncer is running.
/// It potentially flushes the database twice (before and after the actual deletion).
pub async fn lower_retention(
    db: Arc<db::Database>,
    dir_id: i32,
    limits: &[NewLimit],
) -> Result<(), Error> {
    let (_tx, rx) = base::shutdown::channel();
    let mut syncer = Syncer::new(rx, db, dir_id).await?;
    syncer
        .do_rotation(|db| {
            for l in limits {
                let (fs_bytes_before, extra);
                {
                    let Some(stream) = db.streams_by_id().get(&l.stream_id) else {
                        bail!(NotFound, msg("no such stream {}", l.stream_id));
                    };
                    if stream.sample_file_dir_id != Some(dir_id) {
                        bail!(
                            InvalidArgument,
                            msg("stream {} not in dir {}", l.stream_id, dir_id)
                        );
                    }
                    fs_bytes_before =
                        stream.fs_bytes + stream.fs_bytes_to_add - stream.fs_bytes_to_delete;
                    extra = stream.config.retain_bytes - l.limit;
                }
                if l.limit >= fs_bytes_before {
                    continue;
                }
                delete_recordings(db, l.stream_id, extra)?;
            }
            Ok(())
        })
        .await
}

/// Enqueues deletion of recordings to bring a stream's disk usage within bounds.
/// The next flush will mark the recordings as garbage in the SQLite database, and then they can
/// be deleted from disk.
fn delete_recordings(
    db: &mut db::LockedDatabase,
    stream_id: i32,
    extra_bytes_needed: i64,
) -> Result<(), Error> {
    let fs_bytes_needed = {
        let stream = match db.streams_by_id().get(&stream_id) {
            None => bail!(NotFound, msg("no stream {stream_id}")),
            Some(s) => s,
        };
        stream.fs_bytes + stream.fs_bytes_to_add - stream.fs_bytes_to_delete + extra_bytes_needed
            - stream.config.retain_bytes
    };
    let mut fs_bytes_to_delete = 0;
    if fs_bytes_needed <= 0 {
        debug!(
            "{}: have remaining quota of {}",
            stream_id,
            base::strutil::encode_size(fs_bytes_needed)
        );
        return Ok(());
    }
    let mut n = 0;
    db.delete_oldest_recordings(stream_id, &mut |row| {
        if fs_bytes_needed >= fs_bytes_to_delete {
            fs_bytes_to_delete += db::round_up(i64::from(row.sample_file_bytes));
            n += 1;
            return true;
        }
        false
    })?;
    Ok(())
}

impl<F: FileWriter> SyncerChannel<F> {
    /// Asynchronously syncs the given writer, closes it, records it into the database, and
    /// starts rotation.
    fn async_save_recording(&self, id: CompositeId, wall_duration: recording::Duration, f: F) {
        // TODO: don't unwrap.
        self.0
            .try_send(SyncerCommand::AsyncSaveRecording(id, wall_duration, f))
            .unwrap();
    }

    /// For testing: flushes the syncer, waiting for all currently-queued commands to complete,
    /// including the next scheduled database flush (if any). Note this doesn't wait for any
    /// post-database flush garbage collection.
    pub async fn flush(&self) {
        let (snd, rcv) = tokio::sync::oneshot::channel();
        self.0.send(SyncerCommand::Flush(snd)).await.unwrap();
        rcv.await.unwrap_err(); // syncer should just drop the channel, closing it.
    }
}

impl<C: Clocks + Clone> Syncer<C, dir::Pool> {
    async fn new(
        shutdown_rx: base::shutdown::Receiver,
        db: Arc<db::Database<C>>,
        dir_id: i32,
    ) -> Result<Self, Error> {
        let streams_to_next: FastHashMap<_, _>;
        let (pool, db_flush);
        {
            let l = db.lock();
            let d = l
                .sample_file_dirs_by_id()
                .get(&dir_id)
                .ok_or_else(|| err!(NotFound, msg("no dir {dir_id}")))?;
            pool = d.pool().clone();

            // Abandon files.
            // First, get a list of the streams in question.
            streams_to_next = l
                .streams_by_id()
                .iter()
                .filter_map(|(&k, v)| {
                    if v.sample_file_dir_id == Some(dir_id) {
                        Some((k, v.cum_recordings))
                    } else {
                        None
                    }
                })
                .collect();
            db_flush = l.on_flush();
        }
        let undeletable = pool.run("abandon", move |ctx| {
            let mut dir = ctx.iterator()?;
            let mut undeletable = 0;
            while let Some(e) = dir.next() {
                let e = e?;
                let Ok(id) = e.recording_id() else {
                    continue;
                };
                let Some(next) = streams_to_next.get(&id.stream()) else {
                    continue; // unknown stream.
                };
                if id.recording() >= *next {
                    match ctx.unlink(id) {
                        Err(e) if e.kind() == ErrorKind::NotFound => {}
                        Ok(()) => {}
                        Err(e) => {
                            warn!(err = %e.chain(), "dir: unable to unlink abandoned recording");
                            undeletable += 1;
                        }
                    }
                }
            }
            Ok(undeletable)
        }).await?;
        if undeletable > 0 {
            bail!(
                Unknown,
                msg("unable to delete {undeletable} abandoned recordings; see logs")
            );
        }

        Ok(Self {
            dir_id,
            shutdown_rx,
            dir: pool,
            db,
            db_flush,
            planned_flushes: std::collections::BinaryHeap::new(),
        })
    }

    /// Rotates files for all streams and deletes stale files from previous runs.
    /// Called from main thread.
    async fn initial_rotation(&mut self) -> Result<(), Error> {
        let dir_id = self.dir_id;
        self.do_rotation(|db| {
            let streams: Vec<i32> = db
                .streams_by_id()
                .iter()
                .filter_map(|(&id, s)| (s.sample_file_dir_id == Some(dir_id)).then_some(id))
                .collect();
            for &stream_id in &streams {
                delete_recordings(db, stream_id, 0)?;
            }
            Ok(())
        })
        .await
    }

    /// Helper to do initial or retention-lowering rotation. Called from main thread.
    async fn do_rotation<F>(&mut self, delete_recordings: F) -> Result<(), Error>
    where
        F: Fn(&mut db::LockedDatabase) -> Result<(), Error>,
    {
        {
            let mut db = self.db.lock();
            delete_recordings(&mut db)?;
            db.flush("synchronous deletion")?;
        }
        let garbage: Vec<_> = {
            let l = self.db.lock();
            let d = l.sample_file_dirs_by_id().get(&self.dir_id).unwrap();
            d.garbage_needs_unlink.iter().copied().collect()
        };
        if !garbage.is_empty() {
            let mut unlinked = self.dir.collect_garbage(garbage).await?;
            self.db.lock().mark_unlinked(self.dir_id, &mut unlinked)?;
            self.db.lock().flush("synchronous garbage collection")?;
        }
        Ok(())
    }

    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }
}

impl<C: Clocks + Clone, D: DirWriter> Syncer<C, D> {
    /// Processes a single command or timeout.
    ///
    /// Returns true iff the loop should continue.
    async fn iter(
        &mut self,
        cmds: &mut tokio::sync::mpsc::Receiver<SyncerCommand<D::File>>,
    ) -> bool {
        // Set up a future that evaluates on the next planned flush, or never.
        let clocks = self.db.clocks();
        let next_planned_flush = match self.planned_flushes.peek() {
            Some(f) => {
                let now = self.db.clocks().monotonic();

                // Calculate the timeout to use, mapping negative durations to 0.
                let timeout = f.when.saturating_sub(&now);
                futures::future::Either::Left(clocks.sleep(timeout))
            }
            None => futures::future::Either::Right(futures::future::pending()),
        };

        // Wait for a command, the next flush timeout (if specified), or channel disconnect.
        let cmd = tokio::select! {
            // The tests expect that if there is data available, the
            // simulated clock will not be polled.
            biased;

            cmd = cmds.recv() => match cmd {
                Some(cmd) => cmd,
                None => return false, // cmd senders gone.
            },

            _ = self.db_flush.changed() => {
                // The database has been flushed; garbage collection should be attempted.
                if self.collect_garbage().await.is_err() {
                    return false;
                }
                return true;
            },

            _ = next_planned_flush => {
                self.flush();
                return true;
            }
        };

        // Have a command; handle it.
        match cmd {
            SyncerCommand::AsyncSaveRecording(id, wall_dur, f) => {
                if self.save(id, wall_dur, f).await.is_err() {
                    return false;
                }
            }
            SyncerCommand::Flush(flush) => {
                // The sender is waiting for the supplied writer to be dropped. If there's no
                // timeout, do so immediately; otherwise wait for that timeout then drop it.
                if let Some(mut f) = self.planned_flushes.peek_mut() {
                    f.senders.push(flush);
                }
            }
        };

        true
    }

    /// Collects garbage (without forcing a database flush). Called from worker task.
    async fn collect_garbage(&mut self) -> Result<(), ShutdownError> {
        let garbage: Vec<_> = {
            let l = self.db.lock();
            let d = l.sample_file_dirs_by_id().get(&self.dir_id).unwrap();
            d.garbage_needs_unlink.iter().copied().collect()
        };
        if garbage.is_empty() {
            return Ok(());
        }
        let mut unlinked = self.dir.collect_garbage(garbage).await.unwrap_or_else(|e| {
            tracing::error!(err = %e.chain(), "failed to collect garbage");
            Vec::new()
        });
        let db = self.db.clone();
        let dir_id = self.dir_id;
        db.lock().mark_unlinked(dir_id, &mut unlinked).expect("XXX");
        Ok(())
    }

    /// Saves the given recording and prompts rotation. Called from worker task.
    /// Note that this doesn't flush immediately; SQLite transactions are batched to lower SSD
    /// wear. On the next flush, the old recordings will actually be marked as garbage in the
    /// database, and shortly afterward actually deleted from disk.
    async fn save(
        &mut self,
        id: CompositeId,
        wall_duration: recording::Duration,
        mut f: D::File,
    ) -> Result<(), ShutdownError> {
        trace!("Processing save for {}", id);
        let stream_id = id.stream();

        // Free up a like number of bytes.
        while let Err(e) = f.sync_all().await {
            clock::retry_wait(&self.db.clocks(), &self.shutdown_rx, e).await?;
        }
        while let Err(e) = self.dir.sync().await {
            clock::retry_wait(&self.db.clocks(), &self.shutdown_rx, e).await?;
        }
        let mut db = self.db.lock();
        db.mark_synced(id).unwrap();
        delete_recordings(&mut db, stream_id, 0).unwrap();
        let s = db.streams_by_id().get(&stream_id).unwrap();
        let c = db.cameras_by_id().get(&s.camera_id).unwrap();

        // Schedule a flush.
        let how_soon = base::clock::Duration::from_secs(u64::from(s.config.flush_if_sec))
            .saturating_sub(
                base::clock::Duration::try_from(wall_duration)
                    .expect("wall_duration is non-negative"),
            );
        let now = self.db.clocks().monotonic();
        let when = now + how_soon;
        let reason = format!(
            "{} sec after start of {} {}-{} recording {}",
            s.config.flush_if_sec,
            wall_duration,
            c.short_name,
            s.type_.as_str(),
            id
        );
        trace!("scheduling flush in {:?} because {}", how_soon, &reason);
        self.planned_flushes.push(PlannedFlush {
            when,
            reason,
            recording: id,
            senders: Vec::new(),
        });
        Ok(())
    }

    /// Flushes the database if necessary to honor `flush_if_sec` for some recording.
    /// Called from worker thread when one of the `planned_flushes` arrives.
    fn flush(&mut self) {
        trace!("Flushing");
        let mut l = self.db.lock();

        // Look through the planned flushes and see if any are still relevant. It's possible
        // they're not because something else (e.g., a syncer for a different sample file dir)
        // has flushed the database in the meantime.
        use std::collections::binary_heap::PeekMut;
        while let Some(f) = self.planned_flushes.peek_mut() {
            let s = match l.streams_by_id().get(&f.recording.stream()) {
                Some(s) => s,
                None => {
                    // Removing streams while running hasn't been implemented yet, so this should
                    // be impossible.
                    warn!(
                        "bug: no stream for {} which was scheduled to be flushed",
                        f.recording
                    );
                    PeekMut::pop(f);
                    continue;
                }
            };

            if s.cum_recordings <= f.recording.recording() {
                // not yet committed.
                break;
            }

            trace!("planned flush ({}) no longer needed", &f.reason);
            PeekMut::pop(f);
        }

        // If there's anything left to do now, try to flush.
        let f = match self.planned_flushes.peek() {
            None => return,
            Some(f) => f,
        };
        let now = self.db.clocks().monotonic();
        if f.when > now {
            return;
        }
        if let Err(e) = l.flush(&f.reason) {
            let d = base::clock::Duration::from_secs(60);
            warn!(
                "flush failure on save for reason {}; will retry after {:?}: {:?}",
                f.reason, d, e
            );
            self.planned_flushes
                .peek_mut()
                .expect("planned_flushes is non-empty")
                .when = self.db.clocks().monotonic() + base::clock::Duration::from_secs(60);
            return;
        }

        // A successful flush should take care of everything planned.
        self.planned_flushes.clear();
    }
}

/// Struct for writing a single run (of potentially several recordings) to disk and committing its
/// metadata to the database. `Writer` hands off each recording's state to the syncer when done. It
/// saves the recording to the database (if I/O errors do not prevent this), retries forever,
/// or panics (if further writing on this stream is impossible).
pub struct Writer<'a, C: Clocks + Clone, D: DirWriter> {
    dir: &'a D,
    db: &'a db::Database<C>,
    channel: &'a SyncerChannel<D::File>,
    stream_id: i32,
    state: WriterState<D::File>,
}

// clippy points out that the `Open` variant is significantly larger and
// suggests boxing it. There's no benefit to this given that we don't have a lot
// of `WriterState`s active at once, and they should cycle between `Open` and
// `Closed`.
#[allow(clippy::large_enum_variant)]
enum WriterState<F: FileWriter> {
    Unopened,
    Open(InnerWriter<F>),
    Closed(PreviousWriter),
}

/// State for writing a single recording, used within [Writer].
///
/// Note that the recording created by every `InnerWriter` must be written to the [SyncerChannel]
/// with at least one sample. The sample may have zero duration.
struct InnerWriter<F: FileWriter> {
    f: F,
    r: Arc<Mutex<db::RecordingToInsert>>,
    e: recording::SampleIndexEncoder,
    id: CompositeId,
    video_sample_entry_id: i32,

    hasher: blake3::Hasher,

    /// The start time of this recording, based solely on examining the local clock after frames in
    /// this recording were received. Frames can suffer from various kinds of delay (initial
    /// buffering, encoding, and network transmission), so this time is set to far in the future on
    /// construction, given a real value on the first packet, and decreased as less-delayed packets
    /// are discovered. See design/time.md for details.
    local_start: recording::Time,

    /// A sample which has been written to disk but not added to `index`. Index writes are one
    /// sample behind disk writes because the duration of a sample is the difference between its
    /// pts and the next sample's pts. A sample is flushed when the next sample is written, when
    /// the writer is closed cleanly (the caller supplies the next pts), or when the writer is
    /// closed uncleanly (with a zero duration, which the `.mp4` format allows only at the end).
    ///
    /// `unindexed_sample` should always be `Some`, except when a `write` call has aborted on
    /// shutdown. In that case, the close will be unable to write the full segment.
    unindexed_sample: Option<UnindexedSample>,
}

/// A sample which has been written to disk but not included in the index yet.
/// The index includes the sample's duration, which is calculated from the
/// _following_ sample's pts, so the most recent sample is always unindexed.
#[derive(Copy, Clone)]
struct UnindexedSample {
    local_time: recording::Time,
    pts_90k: i64, // relative to the start of the run, not a single recording.
    len: i32,
    is_key: bool,
}

/// State associated with a run's previous recording; used within [Writer].
#[derive(Copy, Clone)]
struct PreviousWriter {
    end: recording::Time,
    run_offset: i32,
}

impl<'a, C: Clocks + Clone, D: DirWriter> Writer<'a, C, D> {
    /// `db` must not be locked.
    pub fn new(
        dir: &'a D,
        db: &'a db::Database<C>,
        channel: &'a SyncerChannel<D::File>,
        stream_id: i32,
    ) -> Self {
        Writer {
            dir,
            db,
            channel,
            stream_id,
            state: WriterState::Unopened,
        }
    }

    /// Opens a new recording if not already open.
    ///
    /// On successful return, `self.state` will be `WriterState::Open(w)` with `w` violating the
    /// invariant that `unindexed_sample` is `Some`. The caller (`write`) is responsible for
    /// correcting this.
    async fn open(
        &mut self,
        shutdown_rx: &mut base::shutdown::Receiver,
        video_sample_entry_id: i32,
    ) -> Result<(), Error> {
        let prev = match self.state {
            WriterState::Unopened => None,
            WriterState::Open(ref o) => {
                if o.video_sample_entry_id != video_sample_entry_id {
                    bail!(Internal, msg("inconsistent video_sample_entry_id"));
                }
                return Ok(());
            }
            WriterState::Closed(prev) => Some(prev),
        };
        let (id, r) = self.db.lock().add_recording(
            self.stream_id,
            db::RecordingToInsert {
                run_offset: prev.map(|p| p.run_offset + 1).unwrap_or(0),
                start: prev.map(|p| p.end).unwrap_or(recording::Time::MAX),
                video_sample_entry_id,
                flags: db::RecordingFlags::Growing as i32,
                ..Default::default()
            },
        )?;
        let f = loop {
            match self.dir.create_file(id).await {
                Ok(f) => break f,
                Err(e) => {
                    warn!(
                        "failed to create recording file for stream {}: {}; retrying",
                        self.stream_id, e
                    );
                    if let Err(e2) = clock::retry_wait(&self.db.clocks(), shutdown_rx, e).await {
                        bail!(Cancelled, source(e2));
                    }
                }
            }
        };

        self.state = WriterState::Open(InnerWriter {
            f,
            r,
            e: recording::SampleIndexEncoder::default(),
            id,
            hasher: blake3::Hasher::new(),
            local_start: recording::Time::MAX,
            unindexed_sample: None,
            video_sample_entry_id,
        });
        Ok(())
    }

    pub fn previously_opened(&self) -> Result<bool, Error> {
        Ok(match self.state {
            WriterState::Unopened => false,
            WriterState::Closed(_) => true,
            WriterState::Open(_) => bail!(Internal, msg("open!")),
        })
    }

    /// Writes a new frame to this recording.
    /// `local_time` should be the local clock's time as of when this packet was received.
    pub async fn write(
        &mut self,
        shutdown_rx: &mut base::shutdown::Receiver,
        frame: Bytes,
        local_time: recording::Time,
        pts_90k: i64,
        is_key: bool,
        video_sample_entry_id: i32,
    ) -> Result<(), Error> {
        self.open(shutdown_rx, video_sample_entry_id).await?;
        let w = match self.state {
            WriterState::Open(ref mut w) => w,
            _ => unreachable!(),
        };

        // Note w's invariant that `unindexed_sample` is `None` may currently be violated.
        // We must restore it on all success or error paths.

        if let Some(unindexed) = w.unindexed_sample.take() {
            let duration = pts_90k - unindexed.pts_90k;
            if duration <= 0 {
                w.unindexed_sample = Some(unindexed); // restore invariant.
                bail!(
                    InvalidArgument,
                    msg(
                        "pts not monotonically increasing; got {} then {}",
                        unindexed.pts_90k,
                        pts_90k,
                    ),
                );
            }
            let duration = match i32::try_from(duration) {
                Ok(d) => d,
                Err(_) => {
                    w.unindexed_sample = Some(unindexed); // restore invariant.
                    bail!(
                        InvalidArgument,
                        msg(
                            "excessive pts jump from {} to {}",
                            unindexed.pts_90k,
                            pts_90k,
                        ),
                    )
                }
            };
            if let Err(e) = w.add_sample(
                duration,
                unindexed.len,
                unindexed.is_key,
                unindexed.local_time,
                self.db,
                self.stream_id,
            ) {
                w.unindexed_sample = Some(unindexed); // restore invariant.
                return Err(e);
            }
        }
        let len = i32::try_from(frame.len()).unwrap();
        w.hasher.update(&frame);
        let mut remaining = frame;
        while !remaining.is_empty() {
            if let Err(e) = w.f.write(&mut remaining).await {
                if let Err(e2) = clock::retry_wait(&self.db.clocks(), shutdown_rx, e).await {
                    tracing::warn!(
                        "abandoning incompletely written recording {} on shutdown",
                        w.id
                    );
                    bail!(Cancelled, source(e2));
                }
            }
        }
        w.unindexed_sample = Some(UnindexedSample {
            local_time,
            pts_90k,
            len,
            is_key,
        });
        Ok(())
    }

    /// Cleanly closes a single recording within this writer, using a supplied
    /// pts of the next sample for the last sample's duration (if known).
    ///
    /// The `Writer` may be used again, causing another recording to be created
    /// within the same run.
    ///
    /// If the `Writer` is dropped without `close`, the `Drop` trait impl will
    /// close, swallowing errors and using a zero duration for the last sample.
    pub fn close(&mut self, next_pts: Option<i64>, reason: Option<String>) -> Result<(), Error> {
        self.state = match mem::replace(&mut self.state, WriterState::Unopened) {
            WriterState::Open(w) => {
                let prev = w.close(self.channel, next_pts, self.db, self.stream_id, reason)?;
                WriterState::Closed(prev)
            }
            s => s,
        };
        Ok(())
    }
}

fn clamp(v: i64, min: i64, max: i64) -> i64 {
    std::cmp::min(std::cmp::max(v, min), max)
}

impl<F: FileWriter> InnerWriter<F> {
    fn add_sample<C: Clocks + Clone>(
        &mut self,
        duration_90k: i32,
        bytes: i32,
        is_key: bool,
        pkt_local_time: recording::Time,
        db: &db::Database<C>,
        stream_id: i32,
    ) -> Result<(), Error> {
        let mut l = self.r.lock();

        // design/time.md explains these time manipulations in detail.
        let prev_media_duration_90k = l.media_duration_90k;
        let media_duration_90k = l.media_duration_90k + duration_90k;
        let local_start = cmp::min(
            self.local_start,
            pkt_local_time - recording::Duration(i64::from(media_duration_90k)),
        );
        let limit = i64::from(media_duration_90k / 2000); // 1/2000th, aka 500 ppm.
        let start = if l.run_offset == 0 {
            // Start time isn't anchored to previous recording's end; adjust.
            local_start
        } else {
            l.start
        };
        let wall_duration_90k = media_duration_90k
            + i32::try_from(clamp(local_start.0 - start.0, -limit, limit)).unwrap();
        if wall_duration_90k > i32::try_from(MAX_RECORDING_WALL_DURATION).unwrap() {
            bail!(
                OutOfRange,
                msg("Duration {wall_duration_90k} exceeds maximum {MAX_RECORDING_WALL_DURATION}"),
            );
        }
        l.wall_duration_90k = wall_duration_90k;
        l.start = start;
        self.local_start = local_start;
        self.e.add_sample(duration_90k, bytes, is_key, &mut l);
        drop(l);
        db.lock()
            .send_live_segment(
                stream_id,
                db::LiveFrame {
                    recording: self.id.recording(),
                    is_key,
                    media_off_90k: prev_media_duration_90k..media_duration_90k,
                },
            )
            .unwrap();
        Ok(())
    }

    fn close<C: Clocks + Clone>(
        mut self,
        channel: &SyncerChannel<F>,
        next_pts: Option<i64>,
        db: &db::Database<C>,
        stream_id: i32,
        reason: Option<String>,
    ) -> Result<PreviousWriter, Error> {
        let unindexed = self.unindexed_sample.take().ok_or_else(|| {
            err!(
                FailedPrecondition,
                msg(
                    "unable to add recording {} to database due to aborted write",
                    self.id,
                ),
            )
        })?;
        let (last_sample_duration, flags) = match next_pts {
            None => (0, db::RecordingFlags::TrailingZero as i32),
            Some(p) => (
                i32::try_from(p - unindexed.pts_90k).map_err(|_| {
                    err!(
                        OutOfRange,
                        msg(
                            "pts {} following {} creates invalid duration",
                            p,
                            unindexed.pts_90k
                        )
                    )
                })?,
                0,
            ),
        };
        let blake3 = self.hasher.finalize();
        let (run_offset, end);
        self.add_sample(
            last_sample_duration,
            unindexed.len,
            unindexed.is_key,
            unindexed.local_time,
            db,
            stream_id,
        )?;

        // This always ends a live segment.
        let wall_duration;
        {
            let mut l = self.r.lock();
            l.flags = flags;
            l.local_time_delta = self.local_start - l.start;
            l.sample_file_blake3 = Some(*blake3.as_bytes());
            l.end_reason = reason;
            wall_duration = recording::Duration(i64::from(l.wall_duration_90k));
            run_offset = l.run_offset;
            end = l.start + wall_duration;
        }
        drop(self.r);
        channel.async_save_recording(self.id, wall_duration, self.f);
        Ok(PreviousWriter { end, run_offset })
    }
}

impl<C: Clocks + Clone, D: DirWriter> Drop for Writer<'_, C, D> {
    fn drop(&mut self) {
        if ::std::thread::panicking() {
            // This will probably panic again. Don't do it.
            return;
        }
        if let WriterState::Open(w) = mem::replace(&mut self.state, WriterState::Unopened) {
            // Swallow any error. The caller should only drop the Writer without calling close()
            // if there's already been an error. The caller should report that. No point in
            // complaining again.
            if let Err(e) = w.close(
                self.channel,
                None,
                self.db,
                self.stream_id,
                Some("drop".to_owned()),
            ) {
                warn!(err = %e.chain(), "error closing recording on drop");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Writer;
    use crate::db::{self, CompositeId, VideoSampleEntryToInsert};
    use crate::recording;
    use crate::testutil;
    use base::clock::{Clocks, SimulatedClocks};
    use base::Mutex;
    use base::{bail, Error};
    use bytes::{Buf as _, Bytes};
    use std::collections::VecDeque;
    use std::sync::Arc;

    #[derive(Clone)]
    struct MockDir(Arc<Mutex<VecDeque<MockDirAction>>>);

    enum MockDirAction {
        Create(
            CompositeId,
            Box<dyn Fn(CompositeId) -> Result<MockFile, Error> + Send>,
        ),
        Sync(Box<dyn Fn() -> Result<(), Error> + Send>),
        CollectGarbage(
            Vec<CompositeId>,
            Box<dyn Fn(Vec<CompositeId>) -> Vec<CompositeId> + Send>,
        ),
    }

    impl std::fmt::Debug for MockDirAction {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Create(id, _f) => f.debug_tuple("Create").field(id).finish_non_exhaustive(),
                Self::Sync(_f) => f.debug_tuple("Sync").finish_non_exhaustive(),
                Self::CollectGarbage(ids, _f) => f
                    .debug_tuple("CollectGarbage")
                    .field(ids)
                    .finish_non_exhaustive(),
            }
        }
    }

    impl MockDir {
        fn new() -> Self {
            MockDir(Arc::new(Mutex::new(VecDeque::new())))
        }
        fn expect(&self, action: MockDirAction) {
            self.0.lock().push_back(action);
        }
        fn ensure_done(&self) {
            assert_eq!(self.0.lock().len(), 0);
        }
    }

    impl super::DirWriter for MockDir {
        type File = MockFile;

        async fn create_file(&self, id: CompositeId) -> Result<Self::File, Error> {
            match self
                .0
                .lock()
                .pop_front()
                .expect("got create_file with no expectation")
            {
                MockDirAction::Create(expected_id, ref f) => {
                    assert_eq!(id, expected_id);
                    f(id)
                }
                _ => panic!("got create_file({id}), expected something else"),
            }
        }
        async fn sync(&self) -> Result<(), Error> {
            match self
                .0
                .lock()
                .pop_front()
                .expect("got sync with no expectation")
            {
                MockDirAction::Sync(f) => f(),
                _ => panic!("got sync, expected something else"),
            }
        }
        async fn collect_garbage(&self, ids: Vec<CompositeId>) -> Result<Vec<CompositeId>, Error> {
            match self
                .0
                .lock()
                .pop_front()
                .expect("got collect_garbage with no expectation")
            {
                MockDirAction::CollectGarbage(expected_ids, f) => {
                    assert_eq!(ids, expected_ids);
                    Ok(f(ids))
                }
                o => panic!("got collect_garbage({ids:?}), expected {o:#?}"),
            }
        }
    }

    impl Drop for MockDir {
        fn drop(&mut self) {
            if !::std::thread::panicking() {
                assert_eq!(self.0.lock().len(), 0);
            }
        }
    }

    #[derive(Clone)]
    struct MockFile(Arc<Mutex<VecDeque<MockFileAction>>>);

    enum MockFileAction {
        SyncAll(Box<dyn Fn() -> Result<(), Error> + Send>),
        #[allow(clippy::type_complexity)]
        Write(Box<dyn Fn(&mut Bytes) -> Result<(), Error> + Send>),
    }

    impl MockFile {
        fn new() -> Self {
            MockFile(Arc::new(Mutex::new(VecDeque::new())))
        }
        fn expect(&self, action: MockFileAction) {
            self.0.lock().push_back(action);
        }
        fn ensure_done(&self) {
            assert_eq!(self.0.lock().len(), 0);
        }
    }

    impl super::FileWriter for MockFile {
        async fn sync_all(&mut self) -> Result<(), Error> {
            match self
                .0
                .lock()
                .pop_front()
                .expect("got sync_all with no expectation")
            {
                MockFileAction::SyncAll(f) => f(),
                _ => panic!("got sync_all, expected something else"),
            }
        }
        async fn write(&mut self, buf: &mut Bytes) -> Result<(), Error> {
            match self
                .0
                .lock()
                .pop_front()
                .expect("got write with no expectation")
            {
                MockFileAction::Write(f) => f(buf),
                _ => panic!("got write({buf:?}), expected something else"),
            }
        }
    }

    struct Harness {
        db: Arc<db::Database<SimulatedClocks>>,
        dir_id: i32,
        _tmpdir: ::tempfile::TempDir,
        dir: MockDir,
        channel: super::SyncerChannel<MockFile>,
        _shutdown_tx: base::shutdown::Sender,
        shutdown_rx: base::shutdown::Receiver,
        syncer: super::Syncer<SimulatedClocks, MockDir>,
        syncer_rx: tokio::sync::mpsc::Receiver<super::SyncerCommand<MockFile>>,
    }

    async fn new_harness(flush_if_sec: u32) -> Harness {
        let clocks = SimulatedClocks::new(base::clock::SystemTime::new(0, 0));
        let tdb = testutil::TestDb::new_with_flush_if_sec(clocks, flush_if_sec).await;
        let dir_id = {
            *tdb.db
                .lock()
                .sample_file_dirs_by_id()
                .keys()
                .next()
                .unwrap()
        };

        // This starts a real fs-backed syncer. Get rid of it.
        drop(tdb.syncer_channel);
        tdb.syncer_join.await.unwrap();

        // Start a mock syncer.
        let dir = MockDir::new();
        let (shutdown_tx, shutdown_rx) = base::shutdown::channel();
        let syncer = {
            let l = tdb.db.lock();
            super::Syncer {
                dir_id: *l.sample_file_dirs_by_id().keys().next().unwrap(),
                dir: dir.clone(),
                db: tdb.db.clone(),
                db_flush: l.on_flush(),
                planned_flushes: std::collections::BinaryHeap::new(),
                shutdown_rx: shutdown_rx.clone(),
            }
        };
        let (syncer_tx, syncer_rx) = tokio::sync::mpsc::channel(16);
        Harness {
            dir_id,
            dir,
            db: tdb.db,
            _tmpdir: tdb.tmpdir,
            channel: super::SyncerChannel(syncer_tx),
            _shutdown_tx: shutdown_tx,
            shutdown_rx,
            syncer,
            syncer_rx,
        }
    }

    #[tokio::test]
    async fn excessive_pts_jump() {
        testutil::init();
        let mut h = new_harness(0).await;
        let video_sample_entry_id =
            h.db.lock()
                .insert_video_sample_entry(VideoSampleEntryToInsert {
                    width: 1920,
                    height: 1080,
                    pasp_h_spacing: 1,
                    pasp_v_spacing: 1,
                    data: [0u8; 100].to_vec(),
                    rfc6381_codec: "avc1.000000".to_owned(),
                })
                .unwrap();
        let mut w = Writer::new(&h.dir, &h.db, &h.channel, testutil::TEST_STREAM_ID);
        h.dir.expect(MockDirAction::Create(
            CompositeId::new(1, 0),
            Box::new(|_id| bail!(Unknown, msg("unknown"))),
        ));
        let f = MockFile::new();
        h.dir.expect(MockDirAction::Create(
            CompositeId::new(1, 0),
            Box::new({
                let f = f.clone();
                move |_id| Ok(f.clone())
            }),
        ));
        f.expect(MockFileAction::Write(Box::new(|data| {
            data.advance(1);
            Ok(())
        })));
        f.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            Bytes::from_static(b"1"),
            recording::Time(1),
            0,
            true,
            video_sample_entry_id,
        )
        .await
        .unwrap();

        let e = w
            .write(
                &mut h.shutdown_rx,
                Bytes::from_static(b"2"),
                recording::Time(2),
                i64::from(i32::MAX) + 1,
                true,
                video_sample_entry_id,
            )
            .await
            .unwrap_err();
        assert!(e.to_string().contains("excessive pts jump"));
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));
        drop(w);
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // planned flush
        assert_eq!(h.syncer.planned_flushes.len(), 0);
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // DatabaseFlushed
        f.ensure_done();
        h.dir.ensure_done();
    }

    /// Tests the database flushing while a syncer is still processing a previous flush event.
    #[tokio::test]
    async fn double_flush() {
        testutil::init();
        let mut h = new_harness(0).await;
        h.db.lock()
            .update_retention(&[db::RetentionChange {
                stream_id: testutil::TEST_STREAM_ID,
                new_record: true,
                new_limit: 0,
            }])
            .unwrap();

        // Setup: add a 3-byte recording.
        let video_sample_entry_id =
            h.db.lock()
                .insert_video_sample_entry(VideoSampleEntryToInsert {
                    width: 1920,
                    height: 1080,
                    pasp_h_spacing: 1,
                    pasp_v_spacing: 1,
                    data: [0u8; 100].to_vec(),
                    rfc6381_codec: "avc1.000000".to_owned(),
                })
                .unwrap();
        let mut w = Writer::new(&h.dir, &h.db, &h.channel, testutil::TEST_STREAM_ID);
        let f = MockFile::new();
        h.dir.expect(MockDirAction::Create(
            CompositeId::new(1, 0),
            Box::new({
                let f = f.clone();
                move |_id| Ok(f.clone())
            }),
        ));
        f.expect(MockFileAction::Write(Box::new(|buf| {
            assert_eq!(&buf[..], b"123");
            buf.advance(3);
            Ok(())
        })));
        f.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            Bytes::from_static(b"123"),
            recording::Time(2),
            0,
            true,
            video_sample_entry_id,
        )
        .await
        .unwrap();
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));
        w.close(Some(1), None).unwrap();
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // planned flush
        assert_eq!(h.syncer.planned_flushes.len(), 0);
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // DatabaseFlushed
        f.ensure_done();
        h.dir.ensure_done();

        // Then a 1-byte recording.
        let f = MockFile::new();
        h.dir.expect(MockDirAction::Create(
            CompositeId::new(1, 1),
            Box::new({
                let f = f.clone();
                move |_id| Ok(f.clone())
            }),
        ));
        f.expect(MockFileAction::Write(Box::new(|buf| {
            assert_eq!(&buf[..], b"4");
            buf.advance(1);
            Ok(())
        })));
        f.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            Bytes::from_static(b"4"),
            recording::Time(3),
            1,
            true,
            video_sample_entry_id,
        )
        .await
        .unwrap();
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));
        h.dir.expect(MockDirAction::CollectGarbage(
            vec![CompositeId::new(1, 0)],
            Box::new({
                let db = h.db.clone();
                move |ids| {
                    // The drop(w) below should cause the old recording to be deleted (moved to
                    // garbage). When the database is flushed, the syncer forces garbage collection
                    // including this unlink.

                    // Do another database flush here, as if from another syncer.
                    db.lock().flush("another syncer running").unwrap();
                    ids
                }
            }),
        ));
        drop(w);

        assert!(h.syncer.iter(&mut h.syncer_rx).await); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // planned flush
        assert_eq!(h.syncer.planned_flushes.len(), 0);
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // DatabaseFlushed
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // DatabaseFlushed again
        f.ensure_done();
        h.dir.ensure_done();

        // Garbage should be marked collected on the next database flush.
        {
            let mut l = h.db.lock();
            let dir = l.sample_file_dirs_by_id().get(&h.dir_id).unwrap();
            assert!(dir.garbage_needs_unlink.is_empty());
            assert!(!dir.garbage_unlinked.is_empty());
            l.flush("forced gc").unwrap();
            let dir = l.sample_file_dirs_by_id().get(&h.dir_id).unwrap();
            assert!(dir.garbage_needs_unlink.is_empty());
            assert!(dir.garbage_unlinked.is_empty());
        }

        assert_eq!(h.syncer.planned_flushes.len(), 0);
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // DatabaseFlushed

        // The syncer should shut down cleanly.
        drop(h.channel);
        assert_eq!(
            h.syncer_rx.try_recv().err(),
            Some(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        );
        assert!(h.syncer.planned_flushes.is_empty());
    }

    #[tokio::test]
    async fn write_path_retries() {
        testutil::init();
        let mut h = new_harness(0).await;
        let video_sample_entry_id =
            h.db.lock()
                .insert_video_sample_entry(VideoSampleEntryToInsert {
                    width: 1920,
                    height: 1080,
                    pasp_h_spacing: 1,
                    pasp_v_spacing: 1,
                    data: [0u8; 100].to_vec(),
                    rfc6381_codec: "avc1.000000".to_owned(),
                })
                .unwrap();
        let mut w = Writer::new(&h.dir, &h.db, &h.channel, testutil::TEST_STREAM_ID);
        h.dir.expect(MockDirAction::Create(
            CompositeId::new(1, 0),
            Box::new(|_id| bail!(Unknown, msg("create error"))),
        ));
        let f = MockFile::new();
        h.dir.expect(MockDirAction::Create(
            CompositeId::new(1, 0),
            Box::new({
                let f = f.clone();
                move |_id| Ok(f.clone())
            }),
        ));
        f.expect(MockFileAction::Write(Box::new(|buf| {
            assert_eq!(&buf[..], b"1234");
            bail!(Unknown, msg("write error"))
        })));
        f.expect(MockFileAction::Write(Box::new(|buf| {
            assert_eq!(&buf[..], b"1234");
            buf.advance(1);
            Ok(())
        })));
        f.expect(MockFileAction::Write(Box::new(|buf| {
            assert_eq!(&buf[..], b"234");
            bail!(Unknown, msg("write error"))
        })));
        f.expect(MockFileAction::Write(Box::new(|buf| {
            assert_eq!(&buf[..], b"234");
            buf.advance(3);
            Ok(())
        })));
        f.expect(MockFileAction::SyncAll(Box::new(|| {
            bail!(Unknown, msg("sync_all error"))
        })));
        f.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            Bytes::from_static(b"1234"),
            recording::Time(1),
            0,
            true,
            video_sample_entry_id,
        )
        .await
        .unwrap();
        h.dir.expect(MockDirAction::Sync(Box::new(|| {
            bail!(Unknown, msg("sync error"))
        })));
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));
        drop(w);
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // planned flush
        assert_eq!(h.syncer.planned_flushes.len(), 0);
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // DatabaseFlushed
        f.ensure_done();
        h.dir.ensure_done();

        {
            let l = h.db.lock();
            let s = l.streams_by_id().get(&testutil::TEST_STREAM_ID).unwrap();
            assert_eq!(s.bytes_to_add, 0);
            assert_eq!(s.sample_file_bytes, 4);
        }

        // The syncer should shut down cleanly.
        drop(h.channel);
        assert_eq!(
            h.syncer_rx.try_recv().err(),
            Some(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        );
        assert!(h.syncer.planned_flushes.is_empty());
    }

    #[tokio::test]
    async fn planned_flush() {
        testutil::init();
        let mut h = new_harness(60).await; // flush_if_sec=60

        // There's a database constraint forbidding a recording starting at t=0, so advance.
        h.db.clocks()
            .sleep(base::clock::Duration::from_secs(1))
            .await;

        // Setup: add a 3-byte recording.
        let video_sample_entry_id =
            h.db.lock()
                .insert_video_sample_entry(VideoSampleEntryToInsert {
                    width: 1920,
                    height: 1080,
                    pasp_h_spacing: 1,
                    pasp_v_spacing: 1,
                    data: [0u8; 100].to_vec(),
                    rfc6381_codec: "avc1.000000".to_owned(),
                })
                .unwrap();
        let mut w = Writer::new(&h.dir, &h.db, &h.channel, testutil::TEST_STREAM_ID);
        let f1 = MockFile::new();
        h.dir.expect(MockDirAction::Create(
            CompositeId::new(1, 0),
            Box::new({
                let f = f1.clone();
                move |_id| Ok(f.clone())
            }),
        ));
        f1.expect(MockFileAction::Write(Box::new(|buf| {
            assert_eq!(&buf[..], b"123");
            buf.advance(3);
            Ok(())
        })));
        f1.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            Bytes::from_static(b"123"),
            recording::Time(recording::TIME_UNITS_PER_SEC),
            0,
            true,
            video_sample_entry_id,
        )
        .await
        .unwrap();
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));
        drop(w);

        assert!(h.syncer.iter(&mut h.syncer_rx).await); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 1);

        // Flush and let 30 seconds go by.
        h.db.lock().flush("forced").unwrap();
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // DatabaseFlushed
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        h.db.clocks()
            .sleep(base::clock::Duration::from_secs(30))
            .await;

        // Then, a 1-byte recording.
        let mut w = Writer::new(&h.dir, &h.db, &h.channel, testutil::TEST_STREAM_ID);
        let f2 = MockFile::new();
        h.dir.expect(MockDirAction::Create(
            CompositeId::new(1, 1),
            Box::new({
                let f = f2.clone();
                move |_id| Ok(f.clone())
            }),
        ));
        f2.expect(MockFileAction::Write(Box::new(|buf| {
            assert_eq!(&buf[..], b"4");
            buf.advance(1);
            Ok(())
        })));
        f2.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            Bytes::from_static(b"4"),
            recording::Time(31 * recording::TIME_UNITS_PER_SEC),
            1,
            true,
            video_sample_entry_id,
        )
        .await
        .unwrap();
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));

        drop(w);

        assert!(h.syncer.iter(&mut h.syncer_rx).await); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 2);

        assert_eq!(h.syncer.planned_flushes.len(), 2);
        let db_flush_count_before = h.db.lock().flushes();
        assert_eq!(
            h.db.clocks().monotonic(),
            base::clock::Instant::from_secs(31)
        );
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // planned flush (no-op)
        assert_eq!(
            h.db.clocks().monotonic(),
            base::clock::Instant::from_secs(61)
        );
        assert_eq!(h.db.lock().flushes(), db_flush_count_before);
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // planned flush
        assert_eq!(
            h.db.clocks().monotonic(),
            base::clock::Instant::from_secs(91)
        );
        assert_eq!(h.db.lock().flushes(), db_flush_count_before + 1);
        assert_eq!(h.syncer.planned_flushes.len(), 0);
        assert!(h.syncer.iter(&mut h.syncer_rx).await); // DatabaseFlushed

        f1.ensure_done();
        f2.ensure_done();
        h.dir.ensure_done();

        // The syncer should shut down cleanly.
        drop(h.channel);
        assert_eq!(
            h.syncer_rx.try_recv().err(),
            Some(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        );
        assert!(h.syncer.planned_flushes.is_empty());
    }
}
