// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2025 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Lifecycle management for recordings.

use crate::db;
use crate::dir;
use crate::RecordingFlags;
use base::bail;
use base::clock::Clocks;
use base::Error;
use base::ErrorKind;
use base::FastHashMap;
use futures::StreamExt as _;
use std::convert::TryFrom;
use std::sync::Arc;
use tracing::{debug, error, info, info_span, trace, warn, Instrument as _};

/// Starts the flusher, as a tokio task.
///
/// The flusher is intended as a singleton and handles all sample file directories
/// and streams. It listens for notifications from directory pools that recordings
/// have been synced, marks them as ready to flush, deletes old recordings to make
/// room, and schedules flushes.
///
/// It currently also prunes `recent_recordings`, though this may change.
///
/// The flusher is unneeded in read-only mode.
///
/// The lock must not be held on `db` when this is called.
///
/// Returns a `FlusherChannel` which can be used to send commands (and can be cloned freely) and
/// a `JoinHandle` for the flusher task. At program shutdown, all
/// `FlusherChannel` clones should be dropped and then the handle joined to
/// allow all recordings to be persisted.
pub fn start_flusher<C: Clocks + Clone>(
    db: Arc<db::Database<C>>,
) -> (FlusherChannel, tokio::task::JoinHandle<()>) {
    let mut flusher = {
        let (db_flush, notify);
        {
            let l = db.lock();
            db_flush = l.on_flush();
            notify = l.flusher_notify.clone();
        }
        Flusher {
            db,
            db_flush,
            notify,
            planned_flush: None,
        }
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let span = info_span!("flusher");
    let handle = tokio::task::Builder::new()
        .name("flusher")
        .spawn(
            async move {
                info!("starting");
                while flusher.iter(&mut rx).await {}
                info!("ending");
            }
            .instrument(span),
        )
        .unwrap();
    (FlusherChannel(tx), handle)
}

/// A channel which can be used to send commands to the flusher.
/// Can be cloned to allow multiple threads to send commands.
/// When all such channels are dropped, the flusher will shut down,
/// even if there are still open writers or uncommitted recordings.
#[derive(Clone)]
pub struct FlusherChannel(tokio::sync::mpsc::Sender<FlusherCommand>);

impl FlusherChannel {
    /// For testing: waits until the flusher is idle: no pending notification, no planned flush.
    /// Note this doesn't wait for any post-database flush garbage collection.
    pub async fn await_idle(&self) {
        trace!("await_idle call starting...");
        let (snd, rcv) = tokio::sync::oneshot::channel();
        self.0.send(FlusherCommand::AwaitIdle(snd)).await.unwrap();
        rcv.await.unwrap_err(); // flusher should just drop the channel, closing it.
        trace!("...await_idle done");
    }
}

/// A command sent to a [Flusher].
enum FlusherCommand {
    /// Command sent by [FlusherChannel::await_idle].
    AwaitIdle(tokio::sync::oneshot::Sender<std::convert::Infallible>),
}

/// State of the flusher worker task created by [start_flusher].
struct Flusher<C: Clocks> {
    db: Arc<db::Database<C>>,
    db_flush: tokio::sync::watch::Receiver<u64>,
    notify: Arc<tokio::sync::Notify>,
    planned_flush: Option<PlannedFlush>,
}

/// A plan to flush at a given instant due to a recently-saved recording's `flush_if_sec` parameter.
struct PlannedFlush {
    /// Monotonic time at which this flush should happen.
    when: base::clock::Instant,

    flush_count: u64,

    /// A human-readable reason for the flush, for logs.
    reason: String,

    /// Senders to drop when this time is reached. This is for test instrumentation; see
    /// [FlusherChannel::flush].
    senders: Vec<tokio::sync::oneshot::Sender<std::convert::Infallible>>,
}

impl<C: Clocks + Clone> Flusher<C> {
    /// Processes a single command or timeout.
    ///
    /// Returns true iff the loop should continue.
    async fn iter(&mut self, cmds: &mut tokio::sync::mpsc::Receiver<FlusherCommand>) -> bool {
        // Set up future `next_planned_flush` that completes on the next planned flush, or never.
        let clocks = self.db.clocks();
        let next_planned_flush = match self.planned_flush.as_ref() {
            Some(f) => {
                // Calculate the timeout to use, mapping negative durations to 0.
                let now = clocks.monotonic();
                let timeout = f.when.saturating_sub(&now);
                futures::future::Either::Left(clocks.sleep(timeout))
            }
            None => futures::future::Either::Right(futures::future::pending()),
        };

        // Wait for a command, the next flush timeout (if specified), or channel disconnect.
        tokio::select! {
            // The tests expect that if there is data available, the
            // simulated clock will not be polled.
            biased;

            _ = self.notify.notified() => {
                self.handle_notify();
            }

            cmd = cmds.recv() => match cmd {
                Some(FlusherCommand::AwaitIdle(drop_when_idle)) => {
                    // The sender is waiting for the supplied writer to be dropped. If there's no
                    // timeout, do so immediately; otherwise hold onto the writer until that timeout;
                    // it will be dropped then.
                    if let Some(f) = self.planned_flush.as_mut() {
                        f.senders.push(drop_when_idle);
                    }
                }
                None => return false, // cmd senders gone, shutdown.
            },

            changed = self.db_flush.changed() => {
                changed.expect("database still exists");
                // The database has been flushed; garbage collection should be attempted.
                let l = self.db.lock();
                for d in l.sample_file_dirs_by_id().values() {
                    std::mem::drop(d.pool().collect_garbage()); // no need to await.
                }

                if self.planned_flush.as_ref().map(|f| f.flush_count < l.flushes()).unwrap_or(false) {
                    debug!("notified of flush that obsoletes plan");
                    self.planned_flush = None;
                }
                return true;
            },

            _ = next_planned_flush => {
                self.flush();
                return true;
            }
        };

        true
    }

    /// Handles notifications.
    ///
    /// Notifications include the following:
    ///
    /// * that recent recording pruning may be necessary due to aborted recordings or pruned frames.
    /// * that recordings are synced, so a flush should be planned.
    fn handle_notify(&mut self) {
        debug!("handling flusher notification");
        let mut db = self.db.lock();
        let flush_count = db.flushes();
        if self
            .planned_flush
            .as_ref()
            .is_some_and(|f| f.flush_count < flush_count)
        {
            debug!("while handling notification, found flush that obsoletes previous plan");
            self.planned_flush = None;
        }
        let mut streams_needing_delete = Vec::new();
        for (&stream_id, stream) in db.streams_by_id() {
            let mut stream = stream.inner.lock();
            let prev_flush_ready = stream.flush_ready;
            if stream.flush_ready == stream.writer_state.recording_id {
                continue; // fast-path.
            }
            let i = stream
                .recent_recordings
                .partition_point(|r| r.id < prev_flush_ready);
            stream.flush_ready = stream.writer_state.recording_id;
            let Some(first_newly_ready) = stream
                .recent_recordings
                .iter()
                .skip(i)
                .take_while(|r| r.id < stream.writer_state.recording_id)
                .find(|r| !r.flags.contains(RecordingFlags::DELETED))
            else {
                continue;
            };
            let wall_duration = base::time::Duration(first_newly_ready.wall_duration_90k.into());
            let flush_if_sec = stream.config.flush_if_sec;
            let stream_type = stream.type_;
            let recording_id = first_newly_ready.id;
            let camera_id = stream.camera_id;
            drop(stream);
            let c = db.cameras_by_id().get(&camera_id).unwrap();
            let how_soon = base::clock::Duration::from_secs(u64::from(flush_if_sec))
                .saturating_sub(
                    base::clock::Duration::try_from(wall_duration)
                        .expect("wall_duration is non-negative"),
                );
            let when = self.db.clocks().monotonic() + how_soon;
            if self.planned_flush.as_ref().is_none_or(|f| f.when > when) {
                let reason = format!(
                    "{flush_if_sec} sec after start of {wall_duration} \
                     {c_short_name}-{stream_type} recording {stream_id}/{recording_id}",
                    c_short_name = c.short_name,
                );
                debug!(
                    stream_id,
                    "scheduling flush {flush_count} in {how_soon:?} because {reason}"
                );
                self.planned_flush = Some(PlannedFlush {
                    when,
                    flush_count,
                    reason,
                    senders: self
                        .planned_flush
                        .take()
                        .map(|f| f.senders)
                        .unwrap_or_default(),
                });
            } else {
                debug!(stream_id, "existing flush will do");
            }
            streams_needing_delete.push(stream_id);
        }
        for &stream_id in &streams_needing_delete {
            if let Err(err) = enqueue_delete_recordings(&mut db, stream_id, 0) {
                error!(err = %err.chain(), stream_id, "enqueue_delete_recordings failed");
            }
        }
    }

    /// Flushes the database if necessary to honor `flush_if_sec` for some recording.
    /// Called from worker task when `planned_flush` arrives.
    fn flush(&mut self) {
        let mut l = self.db.lock();
        let Some(f) = self.planned_flush.take() else {
            error!("flush called with no planned flush; this is a bug");
            return;
        };
        if f.flush_count < l.flushes() {
            debug!("after waking to flush, found flush that obsoletes plan");
            return;
        }
        if let Err(e) = l.flush(&f.reason) {
            let d = base::clock::Duration::from_secs(60);
            warn!(
                "flush failure on save for reason {}; will retry after {:?}: {:?}",
                f.reason, d, e
            );
            self.planned_flush = Some(PlannedFlush {
                when: self.db.clocks().monotonic() + base::clock::Duration::from_secs(60),
                ..f
            });
        }
    }
}

/// Enqueues deletion of recordings to bring a stream's disk usage within bounds.
/// The next flush will mark the recordings as garbage in the SQLite database, and then they can
/// be deleted from disk.
fn enqueue_delete_recordings(
    db: &mut db::LockedDatabase,
    stream_id: i32,
    extra_bytes_needed: i64,
) -> Result<(), Error> {
    let fs_bytes_needed = {
        let stream = match db.streams_by_id().get(&stream_id) {
            None => bail!(NotFound, msg("no stream {stream_id}")),
            Some(s) => s,
        };
        let stream = stream.inner.lock();
        stream.committed.fs_bytes + stream.fs_bytes_to_add() - stream.fs_bytes_to_delete
            + extra_bytes_needed
            - stream.config.retain_bytes
    };
    let mut fs_bytes_to_delete = 0;
    if fs_bytes_needed <= 0 {
        debug!(
            "{}: have remaining quota of {}",
            stream_id,
            base::strutil::encode_size(-fs_bytes_needed)
        );
        return Ok(());
    }
    db.delete_oldest_recordings(stream_id, &mut |row| {
        if fs_bytes_needed >= fs_bytes_to_delete {
            fs_bytes_to_delete += db::round_up(i64::from(row.sample_file_bytes));
            return true;
        }
        false
    })?;
    Ok(())
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
pub async fn lower_retention(db: &db::Database, limits: &[NewLimit]) -> Result<(), Error> {
    do_rotation(db, |db| {
        for l in limits {
            let (fs_bytes_before, extra);
            {
                let Some(stream) = db.streams_by_id().get(&l.stream_id) else {
                    bail!(NotFound, msg("no such stream {}", l.stream_id));
                };
                let stream = stream.inner.lock();
                fs_bytes_before = stream.committed.fs_bytes + stream.fs_bytes_to_add()
                    - stream.fs_bytes_to_delete;
                extra = stream.config.retain_bytes - l.limit;
            }
            if l.limit >= fs_bytes_before {
                continue;
            }
            enqueue_delete_recordings(db, l.stream_id, extra)?;
        }
        Ok(())
    })
    .await
}

/// Rotates files for all streams and deletes stale files from previous runs.
pub async fn initial_rotation(db: &crate::Database) -> Result<(), Error> {
    do_rotation(db, |db| {
        let streams: Vec<i32> = db
            .streams_by_id()
            .iter()
            .filter_map(|(&id, s)| s.inner.lock().sample_file_dir.as_ref().map(|_| id))
            .collect();
        for &stream_id in &streams {
            enqueue_delete_recordings(db, stream_id, 0)?;
        }
        Ok(())
    })
    .await
}

/// Abandon any recordings newer than their stream's respective committed
/// `cum_recordings`. These are presumed to have been unflushed recordings from
/// a previous open.
///
/// This must not be called after writing is started.
pub async fn abandon(db: &crate::Database) -> Result<(), Error> {
    // Abandon files.
    // First, get a list of the dirs/streams in question.
    struct DirInfo {
        pool: dir::Pool,
        streams_to_next: FastHashMap<i32, i32>,
    }
    let dirs = {
        let l = db.lock();
        let mut m = FastHashMap::default();
        for (&stream_id, stream) in l.streams_by_id().iter() {
            let stream = stream.inner.lock();
            assert_eq!(stream.recent_recordings.len(), 0);
            let Some(dir) = stream.sample_file_dir.as_ref() else {
                continue;
            };
            let dir_info = m.entry(dir.id).or_insert_with(|| DirInfo {
                pool: dir.pool().clone(),
                streams_to_next: FastHashMap::default(),
            });
            dir_info
                .streams_to_next
                .insert(stream_id, stream.committed.cum_recordings);
        }
        m
    };
    let mut futures: futures::stream::FuturesUnordered<_> = dirs.into_iter().map(|(dir_id, dir_info)| dir_info.pool.run("abandon", move |ctx| {
            let mut dir = ctx.iterator()?;
            let mut undeletable = 0;
            while let Some(e) = dir.next() {
                let e = e?;
                let Ok(id) = e.recording_id() else {
                    continue;
                };
                let Some(next) = dir_info.streams_to_next.get(&id.stream()) else {
                    warn!(%id, "abandon: unknown stream");
                    continue; // unknown stream.
                };
                if id.recording() >= *next {
                    debug!("abandon: unlinking {id}");
                    match ctx.unlink(id) {
                        Err(e) if e.kind() == ErrorKind::NotFound => {}
                        Ok(()) => {}
                        Err(e) => {
                            warn!(err = %e.chain(), %id, "dir: unable to unlink abandoned recording");
                            undeletable += 1;
                        }
                    }
                }
            }
            if undeletable > 0 {
                bail!(
                    ErrorKind::Unknown,
                    msg("unable to delete {undeletable} abandoned recordings on directory {dir_id}; see logs")
                );
            }
            Ok(())
        }))
    .collect();
    while let Some(r) = futures.next().await {
        r?;
    }
    Ok(())
}

/// Helper to do initial or retention-lowering rotation.
async fn do_rotation<F>(db: &crate::Database, delete_recordings: F) -> Result<(), Error>
where
    F: Fn(&mut db::LockedDatabase) -> Result<(), Error>,
{
    let collections: Vec<_> = {
        let mut db = db.lock();
        delete_recordings(&mut db)?;
        db.flush("synchronous deletion")?;
        db.sample_file_dirs_by_id()
            .values()
            .map(|d| d.pool().clone().collect_garbage())
            .collect()
    };
    let mut need_flush = false;
    for c in collections {
        need_flush |= c.await?;
    }
    if need_flush {
        db.lock().flush("synchronous garbage collection")?;
    }
    Ok(())
}
