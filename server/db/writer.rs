// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Writing recordings and deleting old ones.

use crate::db::{self, CompositeId};
use crate::dir;
use crate::recording::{self, MAX_RECORDING_WALL_DURATION};
use base::clock::{self, Clocks};
use base::shutdown::ShutdownError;
use failure::{bail, format_err, Error};
use fnv::FnvHashMap;
use log::{debug, trace, warn};
use std::cmp::{self, Ordering};
use std::convert::TryFrom;
use std::io;
use std::mem;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration as StdDuration;
use time::{Duration, Timespec};

/// Trait to allow mocking out [crate::dir::SampleFileDir] in syncer tests.
/// This is public because it's exposed in the [SyncerChannel] type parameters,
/// not because it's of direct use outside this module.
pub trait DirWriter: 'static + Send {
    type File: FileWriter;

    fn create_file(&self, id: CompositeId) -> Result<Self::File, nix::Error>;
    fn sync(&self) -> Result<(), nix::Error>;
    fn unlink_file(&self, id: CompositeId) -> Result<(), nix::Error>;
}

/// Trait to allow mocking out [std::fs::File] in syncer tests.
/// This is public because it's exposed in the [SyncerChannel] type parameters,
/// not because it's of direct use outside this module.
pub trait FileWriter: 'static {
    /// As in `std::fs::File::sync_all`.
    fn sync_all(&self) -> Result<(), io::Error>;

    /// As in `std::io::Writer::write`.
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error>;
}

impl DirWriter for Arc<dir::SampleFileDir> {
    type File = ::std::fs::File;

    fn create_file(&self, id: CompositeId) -> Result<Self::File, nix::Error> {
        dir::SampleFileDir::create_file(self, id)
    }
    fn sync(&self) -> Result<(), nix::Error> {
        dir::SampleFileDir::sync(self)
    }
    fn unlink_file(&self, id: CompositeId) -> Result<(), nix::Error> {
        dir::SampleFileDir::unlink_file(self, id)
    }
}

impl FileWriter for ::std::fs::File {
    fn sync_all(&self) -> Result<(), io::Error> {
        self.sync_all()
    }
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        io::Write::write(self, buf)
    }
}

/// A command sent to a [Syncer].
enum SyncerCommand<F> {
    /// Command sent by [SyncerChannel::async_save_recording].
    AsyncSaveRecording(CompositeId, recording::Duration, F),

    /// Notes that the database has been flushed and garbage collection should be attempted.
    /// [start_syncer] sets up a database callback to send this command.
    DatabaseFlushed,

    /// Command sent by [SyncerChannel::flush].
    Flush(mpsc::SyncSender<()>),
}

/// A channel which can be used to send commands to the syncer.
/// Can be cloned to allow multiple threads to send commands.
pub struct SyncerChannel<F>(mpsc::Sender<SyncerCommand<F>>);

impl<F> ::std::clone::Clone for SyncerChannel<F> {
    fn clone(&self) -> Self {
        SyncerChannel(self.0.clone())
    }
}

/// State of the worker thread created by [start_syncer].
struct Syncer<C: Clocks + Clone, D: DirWriter> {
    dir_id: i32,
    dir: D,
    db: Arc<db::Database<C>>,
    planned_flushes: std::collections::BinaryHeap<PlannedFlush>,
    shutdown_rx: base::shutdown::Receiver,
}

/// A plan to flush at a given instant due to a recently-saved recording's `flush_if_sec` parameter.
struct PlannedFlush {
    /// Monotonic time at which this flush should happen.
    when: Timespec,

    /// Recording which prompts this flush. If this recording is already flushed at the planned
    /// time, it can be skipped.
    recording: CompositeId,

    /// A human-readable reason for the flush, for logs.
    reason: String,

    /// Senders to drop when this time is reached. This is for test instrumentation; see
    /// [SyncerChannel::flush].
    senders: Vec<mpsc::SyncSender<()>>,
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
///
/// Note that dropping all `SyncerChannel` clones currently includes calling
/// `LockedDatabase::clear_on_flush`, as this function installs a hook to watch database flushes.
/// TODO: add a join wrapper which arranges for the on flush hook to be removed automatically.
pub fn start_syncer<C>(
    db: Arc<db::Database<C>>,
    shutdown_rx: base::shutdown::Receiver,
    dir_id: i32,
) -> Result<(SyncerChannel<::std::fs::File>, thread::JoinHandle<()>), Error>
where
    C: Clocks + Clone,
{
    let db2 = db.clone();
    let (mut syncer, path) = Syncer::new(&db.lock(), shutdown_rx, db2, dir_id)?;
    syncer.initial_rotation()?;
    let (snd, rcv) = mpsc::channel();
    db.lock().on_flush(Box::new({
        let snd = snd.clone();
        move || {
            if let Err(e) = snd.send(SyncerCommand::DatabaseFlushed) {
                warn!("Unable to notify syncer for dir {} of flush: {}", dir_id, e);
            }
        }
    }));
    Ok((
        SyncerChannel(snd),
        thread::Builder::new()
            .name(format!("sync-{}", path.display()))
            .spawn(move || while syncer.iter(&rcv) {})
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
pub fn lower_retention(
    db: &Arc<db::Database>,
    dir_id: i32,
    limits: &[NewLimit],
) -> Result<(), Error> {
    let db2 = db.clone();
    let (_tx, rx) = base::shutdown::channel();
    let (mut syncer, _) = Syncer::new(&db.lock(), rx, db2, dir_id)?;
    syncer.do_rotation(|db| {
        for l in limits {
            let (fs_bytes_before, extra);
            {
                let stream = db
                    .streams_by_id()
                    .get(&l.stream_id)
                    .ok_or_else(|| format_err!("no such stream {}", l.stream_id))?;
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
            None => bail!("no stream {}", stream_id),
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
        self.0
            .send(SyncerCommand::AsyncSaveRecording(id, wall_duration, f))
            .unwrap();
    }

    /// For testing: flushes the syncer, waiting for all currently-queued commands to complete,
    /// including the next scheduled database flush (if any). Note this doesn't wait for any
    /// post-database flush garbage collection.
    pub fn flush(&self) {
        let (snd, rcv) = mpsc::sync_channel(0);
        self.0.send(SyncerCommand::Flush(snd)).unwrap();
        rcv.recv().unwrap_err(); // syncer should just drop the channel, closing it.
    }
}

/// Lists files which should be "abandoned" (deleted without ever recording in the database)
/// on opening.
fn list_files_to_abandon(
    dir: &dir::SampleFileDir,
    streams_to_next: FnvHashMap<i32, i32>,
) -> Result<Vec<CompositeId>, Error> {
    let mut v = Vec::new();
    let mut d = dir.opendir()?;
    for e in d.iter() {
        let e = e?;
        let id = match dir::parse_id(e.file_name().to_bytes()) {
            Ok(i) => i,
            Err(_) => continue,
        };
        let next = match streams_to_next.get(&id.stream()) {
            Some(n) => *n,
            None => continue, // unknown stream.
        };
        if id.recording() >= next {
            v.push(id);
        }
    }
    Ok(v)
}

impl<C: Clocks + Clone> Syncer<C, Arc<dir::SampleFileDir>> {
    fn new(
        l: &db::LockedDatabase,
        shutdown_rx: base::shutdown::Receiver,
        db: Arc<db::Database<C>>,
        dir_id: i32,
    ) -> Result<(Self, PathBuf), Error> {
        let d = l
            .sample_file_dirs_by_id()
            .get(&dir_id)
            .ok_or_else(|| format_err!("no dir {}", dir_id))?;
        let dir = d.get()?;

        // Abandon files.
        // First, get a list of the streams in question.
        let streams_to_next: FnvHashMap<_, _> = l
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
        let to_abandon = list_files_to_abandon(&dir, streams_to_next)?;
        let mut undeletable = 0;
        for &id in &to_abandon {
            if let Err(e) = dir.unlink_file(id) {
                if e == nix::Error::ENOENT {
                    warn!("dir: abandoned recording {} already deleted!", id);
                } else {
                    warn!("dir: Unable to unlink abandoned recording {}: {}", id, e);
                    undeletable += 1;
                }
            }
        }
        if undeletable > 0 {
            bail!("Unable to delete {} abandoned recordings.", undeletable);
        }

        Ok((
            Syncer {
                dir_id,
                shutdown_rx,
                dir,
                db,
                planned_flushes: std::collections::BinaryHeap::new(),
            },
            d.path.clone(),
        ))
    }

    /// Rotates files for all streams and deletes stale files from previous runs.
    /// Called from main thread.
    fn initial_rotation(&mut self) -> Result<(), Error> {
        self.do_rotation(|db| {
            let streams: Vec<i32> = db.streams_by_id().keys().copied().collect();
            for &stream_id in &streams {
                delete_recordings(db, stream_id, 0)?;
            }
            Ok(())
        })
    }

    /// Helper to do initial or retention-lowering rotation. Called from main thread.
    fn do_rotation<F>(&mut self, delete_recordings: F) -> Result<(), Error>
    where
        F: Fn(&mut db::LockedDatabase) -> Result<(), Error>,
    {
        {
            let mut db = self.db.lock();
            delete_recordings(&mut db)?;
            db.flush("synchronous deletion")?;
        }
        let mut garbage: Vec<_> = {
            let l = self.db.lock();
            let d = l.sample_file_dirs_by_id().get(&self.dir_id).unwrap();
            d.garbage_needs_unlink.iter().copied().collect()
        };
        if !garbage.is_empty() {
            // Try to delete files; retain ones in `garbage` that don't exist.
            let mut errors = 0;
            for &id in &garbage {
                if let Err(e) = self.dir.unlink_file(id) {
                    if e != nix::Error::ENOENT {
                        warn!("dir: Unable to unlink {}: {}", id, e);
                        errors += 1;
                    }
                }
            }
            if errors > 0 {
                bail!(
                    "Unable to unlink {} files (see earlier warning messages for details)",
                    errors
                );
            }
            self.dir.sync()?;
            self.db.lock().delete_garbage(self.dir_id, &mut garbage)?;
            self.db.lock().flush("synchronous garbage collection")?;
        }
        Ok(())
    }
}

impl<C: Clocks + Clone, D: DirWriter> Syncer<C, D> {
    /// Processes a single command or timeout.
    ///
    /// Returns true iff the loop should continue.
    fn iter(&mut self, cmds: &mpsc::Receiver<SyncerCommand<D::File>>) -> bool {
        // Wait for a command, the next flush timeout (if specified), or channel disconnect.
        let next_flush = self.planned_flushes.peek().map(|f| f.when);
        let cmd = match next_flush {
            None => match cmds.recv() {
                Err(_) => return false, // all cmd senders are gone.
                Ok(cmd) => cmd,
            },
            Some(t) => {
                let now = self.db.clocks().monotonic();

                // Calculate the timeout to use, mapping negative durations to 0.
                let timeout = (t - now)
                    .to_std()
                    .unwrap_or_else(|_| StdDuration::new(0, 0));
                match self.db.clocks().recv_timeout(cmds, timeout) {
                    Err(mpsc::RecvTimeoutError::Disconnected) => return false, // cmd senders gone.
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        self.flush();
                        return true;
                    }
                    Ok(cmd) => cmd,
                }
            }
        };

        // Have a command; handle it.
        match cmd {
            SyncerCommand::AsyncSaveRecording(id, wall_dur, f) => {
                if self.save(id, wall_dur, f).is_err() {
                    return false;
                }
            }
            SyncerCommand::DatabaseFlushed => {
                if self.collect_garbage().is_err() {
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

    /// Collects garbage (without forcing a sync). Called from worker thread.
    fn collect_garbage(&mut self) -> Result<(), ShutdownError> {
        trace!("Collecting garbage");
        let mut garbage: Vec<_> = {
            let l = self.db.lock();
            let d = l.sample_file_dirs_by_id().get(&self.dir_id).unwrap();
            d.garbage_needs_unlink.iter().copied().collect()
        };
        if garbage.is_empty() {
            return Ok(());
        }
        let c = &self.db.clocks();
        for &id in &garbage {
            clock::retry(c, &self.shutdown_rx, &mut || {
                if let Err(e) = self.dir.unlink_file(id) {
                    if e == nix::Error::ENOENT {
                        warn!("dir: recording {} already deleted!", id);
                        return Ok(());
                    }
                    return Err(e);
                }
                Ok(())
            })?;
        }
        clock::retry(c, &self.shutdown_rx, &mut || self.dir.sync())?;
        clock::retry(c, &self.shutdown_rx, &mut || {
            self.db.lock().delete_garbage(self.dir_id, &mut garbage)
        })?;
        Ok(())
    }

    /// Saves the given recording and prompts rotation. Called from worker thread.
    /// Note that this doesn't flush immediately; SQLite transactions are batched to lower SSD
    /// wear. On the next flush, the old recordings will actually be marked as garbage in the
    /// database, and shortly afterward actually deleted from disk.
    fn save(
        &mut self,
        id: CompositeId,
        wall_duration: recording::Duration,
        f: D::File,
    ) -> Result<(), ShutdownError> {
        trace!("Processing save for {}", id);
        let stream_id = id.stream();

        // Free up a like number of bytes.
        clock::retry(&self.db.clocks(), &self.shutdown_rx, &mut || f.sync_all())?;
        clock::retry(&self.db.clocks(), &self.shutdown_rx, &mut || {
            self.dir.sync()
        })?;
        let mut db = self.db.lock();
        db.mark_synced(id).unwrap();
        delete_recordings(&mut db, stream_id, 0).unwrap();
        let s = db.streams_by_id().get(&stream_id).unwrap();
        let c = db.cameras_by_id().get(&s.camera_id).unwrap();

        // Schedule a flush.
        let how_soon =
            Duration::seconds(i64::from(s.config.flush_if_sec)) - wall_duration.to_tm_duration();
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
        trace!("scheduling flush in {} because {}", how_soon, &reason);
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
            let d = Duration::minutes(1);
            warn!(
                "flush failure on save for reason {}; will retry after {}: {:?}",
                f.reason, d, e
            );
            self.planned_flushes
                .peek_mut()
                .expect("planned_flushes is non-empty")
                .when = self.db.clocks().monotonic() + Duration::minutes(1);
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
    fn open(
        &mut self,
        shutdown_rx: &mut base::shutdown::Receiver,
        video_sample_entry_id: i32,
    ) -> Result<(), Error> {
        let prev = match self.state {
            WriterState::Unopened => None,
            WriterState::Open(ref o) => {
                if o.video_sample_entry_id != video_sample_entry_id {
                    bail!("inconsistent video_sample_entry_id");
                }
                return Ok(());
            }
            WriterState::Closed(prev) => Some(prev),
        };
        let (id, r) = self.db.lock().add_recording(
            self.stream_id,
            db::RecordingToInsert {
                run_offset: prev.map(|p| p.run_offset + 1).unwrap_or(0),
                start: prev
                    .map(|p| p.end)
                    .unwrap_or(recording::Time(i64::max_value())),
                video_sample_entry_id,
                flags: db::RecordingFlags::Growing as i32,
                ..Default::default()
            },
        )?;
        let f = clock::retry(&self.db.clocks(), shutdown_rx, &mut || {
            self.dir.create_file(id)
        })?;

        self.state = WriterState::Open(InnerWriter {
            f,
            r,
            e: recording::SampleIndexEncoder::default(),
            id,
            hasher: blake3::Hasher::new(),
            local_start: recording::Time(i64::max_value()),
            unindexed_sample: None,
            video_sample_entry_id,
        });
        Ok(())
    }

    pub fn previously_opened(&self) -> Result<bool, Error> {
        Ok(match self.state {
            WriterState::Unopened => false,
            WriterState::Closed(_) => true,
            WriterState::Open(_) => bail!("open!"),
        })
    }

    /// Writes a new frame to this recording.
    /// `local_time` should be the local clock's time as of when this packet was received.
    pub fn write(
        &mut self,
        shutdown_rx: &mut base::shutdown::Receiver,
        pkt: &[u8],
        local_time: recording::Time,
        pts_90k: i64,
        is_key: bool,
        video_sample_entry_id: i32,
    ) -> Result<(), Error> {
        self.open(shutdown_rx, video_sample_entry_id)?;
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
                    "pts not monotonically increasing; got {} then {}",
                    unindexed.pts_90k,
                    pts_90k
                );
            }
            let duration = match i32::try_from(duration) {
                Ok(d) => d,
                Err(_) => {
                    w.unindexed_sample = Some(unindexed); // restore invariant.
                    bail!(
                        "excessive pts jump from {} to {}",
                        unindexed.pts_90k,
                        pts_90k
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
        let mut remaining = pkt;
        while !remaining.is_empty() {
            let written =
                match clock::retry(&self.db.clocks(), shutdown_rx, &mut || w.f.write(remaining)) {
                    Ok(w) => w,
                    Err(e) => {
                        // close() will do nothing because unindexed_sample will be None.
                        log::warn!(
                            "Abandoning incompletely written recording {} on shutdown",
                            w.id
                        );
                        return Err(e.into());
                    }
                };
            remaining = &remaining[written..];
        }
        w.unindexed_sample = Some(UnindexedSample {
            local_time,
            pts_90k,
            len: i32::try_from(pkt.len()).unwrap(),
            is_key,
        });
        w.hasher.update(pkt);
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
        let mut l = self.r.lock().unwrap();

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
                "Duration {} exceeds maximum {}",
                wall_duration_90k,
                MAX_RECORDING_WALL_DURATION
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
                db::LiveSegment {
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
            format_err!(
                "Unable to add recording {} to database due to aborted write",
                self.id
            )
        })?;
        let (last_sample_duration, flags) = match next_pts {
            None => (0, db::RecordingFlags::TrailingZero as i32),
            Some(p) => (i32::try_from(p - unindexed.pts_90k)?, 0),
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
            let mut l = self.r.lock().unwrap();
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

impl<'a, C: Clocks + Clone, D: DirWriter> Drop for Writer<'a, C, D> {
    fn drop(&mut self) {
        if ::std::thread::panicking() {
            // This will probably panic again. Don't do it.
            return;
        }
        if let WriterState::Open(w) = mem::replace(&mut self.state, WriterState::Unopened) {
            // Swallow any error. The caller should only drop the Writer without calling close()
            // if there's already been an error. The caller should report that. No point in
            // complaining again.
            let _ = w.close(
                self.channel,
                None,
                self.db,
                self.stream_id,
                Some("drop".to_owned()),
            );
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
    use log::{trace, warn};
    use std::collections::VecDeque;
    use std::io;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::sync::Mutex;

    #[derive(Clone)]
    struct MockDir(Arc<Mutex<VecDeque<MockDirAction>>>);

    enum MockDirAction {
        Create(
            CompositeId,
            Box<dyn Fn(CompositeId) -> Result<MockFile, nix::Error> + Send>,
        ),
        Sync(Box<dyn Fn() -> Result<(), nix::Error> + Send>),
        Unlink(
            CompositeId,
            Box<dyn Fn(CompositeId) -> Result<(), nix::Error> + Send>,
        ),
    }

    impl MockDir {
        fn new() -> Self {
            MockDir(Arc::new(Mutex::new(VecDeque::new())))
        }
        fn expect(&self, action: MockDirAction) {
            self.0.lock().unwrap().push_back(action);
        }
        fn ensure_done(&self) {
            assert_eq!(self.0.lock().unwrap().len(), 0);
        }
    }

    impl super::DirWriter for MockDir {
        type File = MockFile;

        fn create_file(&self, id: CompositeId) -> Result<Self::File, nix::Error> {
            match self
                .0
                .lock()
                .unwrap()
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
        fn sync(&self) -> Result<(), nix::Error> {
            match self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .expect("got sync with no expectation")
            {
                MockDirAction::Sync(f) => f(),
                _ => panic!("got sync, expected something else"),
            }
        }
        fn unlink_file(&self, id: CompositeId) -> Result<(), nix::Error> {
            match self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .expect("got unlink_file with no expectation")
            {
                MockDirAction::Unlink(expected_id, f) => {
                    assert_eq!(id, expected_id);
                    f(id)
                }
                _ => panic!("got unlink({id}), expected something else"),
            }
        }
    }

    impl Drop for MockDir {
        fn drop(&mut self) {
            if !::std::thread::panicking() {
                assert_eq!(self.0.lock().unwrap().len(), 0);
            }
        }
    }

    #[derive(Clone)]
    struct MockFile(Arc<Mutex<VecDeque<MockFileAction>>>);

    enum MockFileAction {
        SyncAll(Box<dyn Fn() -> Result<(), io::Error> + Send>),
        Write(Box<dyn Fn(&[u8]) -> Result<usize, io::Error> + Send>),
    }

    impl MockFile {
        fn new() -> Self {
            MockFile(Arc::new(Mutex::new(VecDeque::new())))
        }
        fn expect(&self, action: MockFileAction) {
            self.0.lock().unwrap().push_back(action);
        }
        fn ensure_done(&self) {
            assert_eq!(self.0.lock().unwrap().len(), 0);
        }
    }

    impl super::FileWriter for MockFile {
        fn sync_all(&self) -> Result<(), io::Error> {
            match self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .expect("got sync_all with no expectation")
            {
                MockFileAction::SyncAll(f) => f(),
                _ => panic!("got sync_all, expected something else"),
            }
        }
        fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
            match self
                .0
                .lock()
                .unwrap()
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
        syncer_rx: mpsc::Receiver<super::SyncerCommand<MockFile>>,
    }

    fn new_harness(flush_if_sec: u32) -> Harness {
        let clocks = SimulatedClocks::new(::time::Timespec::new(0, 0));
        let tdb = testutil::TestDb::new_with_flush_if_sec(clocks, flush_if_sec);
        let dir_id = *tdb
            .db
            .lock()
            .sample_file_dirs_by_id()
            .keys()
            .next()
            .unwrap();

        // This starts a real fs-backed syncer. Get rid of it.
        tdb.db.lock().clear_on_flush();
        drop(tdb.syncer_channel);
        tdb.syncer_join.join().unwrap();

        // Start a mock syncer.
        let dir = MockDir::new();
        let (shutdown_tx, shutdown_rx) = base::shutdown::channel();
        let syncer = super::Syncer {
            dir_id: *tdb
                .db
                .lock()
                .sample_file_dirs_by_id()
                .keys()
                .next()
                .unwrap(),
            dir: dir.clone(),
            db: tdb.db.clone(),
            planned_flushes: std::collections::BinaryHeap::new(),
            shutdown_rx: shutdown_rx.clone(),
        };
        let (syncer_tx, syncer_rx) = mpsc::channel();
        tdb.db.lock().on_flush(Box::new({
            let snd = syncer_tx.clone();
            move || {
                if let Err(e) = snd.send(super::SyncerCommand::DatabaseFlushed) {
                    warn!("Unable to notify syncer for dir {} of flush: {}", dir_id, e);
                }
            }
        }));
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

    fn eio() -> io::Error {
        io::Error::new(io::ErrorKind::Other, "got EIO")
    }

    #[test]
    fn excessive_pts_jump() {
        testutil::init();
        let mut h = new_harness(0);
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
            Box::new(|_id| Err(nix::Error::EIO)),
        ));
        let f = MockFile::new();
        h.dir.expect(MockDirAction::Create(
            CompositeId::new(1, 0),
            Box::new({
                let f = f.clone();
                move |_id| Ok(f.clone())
            }),
        ));
        f.expect(MockFileAction::Write(Box::new(|_| Ok(1))));
        f.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            b"1",
            recording::Time(1),
            0,
            true,
            video_sample_entry_id,
        )
        .unwrap();

        let e = w
            .write(
                &mut h.shutdown_rx,
                b"2",
                recording::Time(2),
                i32::max_value() as i64 + 1,
                true,
                video_sample_entry_id,
            )
            .unwrap_err();
        assert!(e.to_string().contains("excessive pts jump"));
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));
        drop(w);
        assert!(h.syncer.iter(&h.syncer_rx)); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        assert!(h.syncer.iter(&h.syncer_rx)); // planned flush
        assert_eq!(h.syncer.planned_flushes.len(), 0);
        assert!(h.syncer.iter(&h.syncer_rx)); // DatabaseFlushed
        f.ensure_done();
        h.dir.ensure_done();
    }

    /// Tests the database flushing while a syncer is still processing a previous flush event.
    #[test]
    fn double_flush() {
        testutil::init();
        let mut h = new_harness(0);
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
            assert_eq!(buf, b"123");
            Ok(3)
        })));
        f.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            b"123",
            recording::Time(2),
            0,
            true,
            video_sample_entry_id,
        )
        .unwrap();
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));
        w.close(Some(1), None).unwrap();
        assert!(h.syncer.iter(&h.syncer_rx)); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        assert!(h.syncer.iter(&h.syncer_rx)); // planned flush
        assert_eq!(h.syncer.planned_flushes.len(), 0);
        assert!(h.syncer.iter(&h.syncer_rx)); // DatabaseFlushed
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
            assert_eq!(buf, b"4");
            Ok(1)
        })));
        f.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            b"4",
            recording::Time(3),
            1,
            true,
            video_sample_entry_id,
        )
        .unwrap();
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));
        h.dir.expect(MockDirAction::Unlink(
            CompositeId::new(1, 0),
            Box::new({
                let db = h.db.clone();
                move |_| {
                    // The drop(w) below should cause the old recording to be deleted (moved to
                    // garbage). When the database is flushed, the syncer forces garbage collection
                    // including this unlink.

                    // Do another database flush here, as if from another syncer.
                    db.lock().flush("another syncer running").unwrap();
                    Ok(())
                }
            }),
        ));
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));
        drop(w);

        trace!("expecting AsyncSave");
        assert!(h.syncer.iter(&h.syncer_rx)); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        trace!("expecting planned flush");
        assert!(h.syncer.iter(&h.syncer_rx)); // planned flush
        assert_eq!(h.syncer.planned_flushes.len(), 0);
        trace!("expecting DatabaseFlushed");
        assert!(h.syncer.iter(&h.syncer_rx)); // DatabaseFlushed
        trace!("expecting DatabaseFlushed again");
        assert!(h.syncer.iter(&h.syncer_rx)); // DatabaseFlushed again
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
        assert!(h.syncer.iter(&h.syncer_rx)); // DatabaseFlushed

        // The syncer should shut down cleanly.
        drop(h.channel);
        h.db.lock().clear_on_flush();
        assert_eq!(
            h.syncer_rx.try_recv().err(),
            Some(std::sync::mpsc::TryRecvError::Disconnected)
        );
        assert!(h.syncer.planned_flushes.is_empty());
    }

    #[test]
    fn write_path_retries() {
        testutil::init();
        let mut h = new_harness(0);
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
            Box::new(|_id| Err(nix::Error::EIO)),
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
            assert_eq!(buf, b"1234");
            Err(eio())
        })));
        f.expect(MockFileAction::Write(Box::new(|buf| {
            assert_eq!(buf, b"1234");
            Ok(1)
        })));
        f.expect(MockFileAction::Write(Box::new(|buf| {
            assert_eq!(buf, b"234");
            Err(eio())
        })));
        f.expect(MockFileAction::Write(Box::new(|buf| {
            assert_eq!(buf, b"234");
            Ok(3)
        })));
        f.expect(MockFileAction::SyncAll(Box::new(|| Err(eio()))));
        f.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            b"1234",
            recording::Time(1),
            0,
            true,
            video_sample_entry_id,
        )
        .unwrap();
        h.dir
            .expect(MockDirAction::Sync(Box::new(|| Err(nix::Error::EIO))));
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));
        drop(w);
        assert!(h.syncer.iter(&h.syncer_rx)); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        assert!(h.syncer.iter(&h.syncer_rx)); // planned flush
        assert_eq!(h.syncer.planned_flushes.len(), 0);
        assert!(h.syncer.iter(&h.syncer_rx)); // DatabaseFlushed
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
        h.db.lock().clear_on_flush();
        assert_eq!(
            h.syncer_rx.try_recv().err(),
            Some(std::sync::mpsc::TryRecvError::Disconnected)
        );
        assert!(h.syncer.planned_flushes.is_empty());
    }

    #[test]
    fn gc_path_retries() {
        testutil::init();
        let mut h = new_harness(0);
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
            assert_eq!(buf, b"123");
            Ok(3)
        })));
        f.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            b"123",
            recording::Time(2),
            0,
            true,
            video_sample_entry_id,
        )
        .unwrap();
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));
        w.close(Some(1), None).unwrap();

        assert!(h.syncer.iter(&h.syncer_rx)); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        assert!(h.syncer.iter(&h.syncer_rx)); // planned flush
        assert_eq!(h.syncer.planned_flushes.len(), 0);
        assert!(h.syncer.iter(&h.syncer_rx)); // DatabaseFlushed
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
            assert_eq!(buf, b"4");
            Ok(1)
        })));
        f.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            b"4",
            recording::Time(3),
            1,
            true,
            video_sample_entry_id,
        )
        .unwrap();
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));
        h.dir.expect(MockDirAction::Unlink(
            CompositeId::new(1, 0),
            Box::new({
                let db = h.db.clone();
                move |_| {
                    // The drop(w) below should cause the old recording to be deleted (moved to
                    // garbage).  When the database is flushed, the syncer forces garbage collection
                    // including this unlink.

                    // This should have already applied the changes to sample file bytes, even
                    // though the garbage has yet to be collected.
                    let l = db.lock();
                    let s = l.streams_by_id().get(&testutil::TEST_STREAM_ID).unwrap();
                    assert_eq!(s.bytes_to_delete, 0);
                    assert_eq!(s.bytes_to_add, 0);
                    assert_eq!(s.sample_file_bytes, 1);
                    Err(nix::Error::EIO) // force a retry.
                }
            }),
        ));
        h.dir.expect(MockDirAction::Unlink(
            CompositeId::new(1, 0),
            Box::new(|_| Ok(())),
        ));
        h.dir
            .expect(MockDirAction::Sync(Box::new(|| Err(nix::Error::EIO))));
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));

        drop(w);

        assert!(h.syncer.iter(&h.syncer_rx)); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        assert!(h.syncer.iter(&h.syncer_rx)); // planned flush
        assert_eq!(h.syncer.planned_flushes.len(), 0);
        assert!(h.syncer.iter(&h.syncer_rx)); // DatabaseFlushed
        f.ensure_done();
        h.dir.ensure_done();

        // Garbage should be marked collected on the next flush.
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

        assert!(h.syncer.iter(&h.syncer_rx)); // DatabaseFlushed

        // The syncer should shut down cleanly.
        drop(h.channel);
        h.db.lock().clear_on_flush();
        assert_eq!(
            h.syncer_rx.try_recv().err(),
            Some(std::sync::mpsc::TryRecvError::Disconnected)
        );
        assert!(h.syncer.planned_flushes.is_empty());
    }

    #[test]
    fn planned_flush() {
        testutil::init();
        let mut h = new_harness(60); // flush_if_sec=60

        // There's a database constraint forbidding a recording starting at t=0, so advance.
        h.db.clocks().sleep(time::Duration::seconds(1));

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
            assert_eq!(buf, b"123");
            Ok(3)
        })));
        f1.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            b"123",
            recording::Time(recording::TIME_UNITS_PER_SEC),
            0,
            true,
            video_sample_entry_id,
        )
        .unwrap();
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));
        drop(w);

        assert!(h.syncer.iter(&h.syncer_rx)); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 1);

        // Flush and let 30 seconds go by.
        h.db.lock().flush("forced").unwrap();
        assert!(h.syncer.iter(&h.syncer_rx)); // DatabaseFlushed
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        h.db.clocks().sleep(time::Duration::seconds(30));

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
            assert_eq!(buf, b"4");
            Ok(1)
        })));
        f2.expect(MockFileAction::SyncAll(Box::new(|| Ok(()))));
        w.write(
            &mut h.shutdown_rx,
            b"4",
            recording::Time(31 * recording::TIME_UNITS_PER_SEC),
            1,
            true,
            video_sample_entry_id,
        )
        .unwrap();
        h.dir.expect(MockDirAction::Sync(Box::new(|| Ok(()))));

        drop(w);

        assert!(h.syncer.iter(&h.syncer_rx)); // AsyncSave
        assert_eq!(h.syncer.planned_flushes.len(), 2);

        assert_eq!(h.syncer.planned_flushes.len(), 2);
        let db_flush_count_before = h.db.lock().flushes();
        assert_eq!(h.db.clocks().monotonic(), time::Timespec::new(31, 0));
        assert!(h.syncer.iter(&h.syncer_rx)); // planned flush (no-op)
        assert_eq!(h.db.clocks().monotonic(), time::Timespec::new(61, 0));
        assert_eq!(h.db.lock().flushes(), db_flush_count_before);
        assert_eq!(h.syncer.planned_flushes.len(), 1);
        assert!(h.syncer.iter(&h.syncer_rx)); // planned flush
        assert_eq!(h.db.clocks().monotonic(), time::Timespec::new(91, 0));
        assert_eq!(h.db.lock().flushes(), db_flush_count_before + 1);
        assert_eq!(h.syncer.planned_flushes.len(), 0);
        assert!(h.syncer.iter(&h.syncer_rx)); // DatabaseFlushed

        f1.ensure_done();
        f2.ensure_done();
        h.dir.ensure_done();

        // The syncer should shut down cleanly.
        drop(h.channel);
        h.db.lock().clear_on_flush();
        assert_eq!(
            h.syncer_rx.try_recv().err(),
            Some(std::sync::mpsc::TryRecvError::Disconnected)
        );
        assert!(h.syncer.planned_flushes.is_empty());
    }
}
