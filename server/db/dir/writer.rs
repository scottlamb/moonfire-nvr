// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2026 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

use std::{io::Write as _, sync::Arc};

use arrayvec::ArrayVec;
use base::{clock::TimerGuard, Antilock};
use itertools::Itertools;
use nix::{fcntl::OFlag, sys::stat::Mode};
use reffers::ARefss;

use crate::{
    stream::{recent_frames::RecentFrames, BytePos, LockedStream, Stream},
    CompositeId, RecentRecording, RecordingFlags,
};

use super::{IoCommand, Worker};

const MAX_CHUNKS_PER_WRITE: usize = 32;

type Chunk = reffers::ARefss<'static, [u8]>;

/// The directory pool's writer state for a given [`create::db::LockedStream`].
#[derive(Default)]
pub(crate) struct State {
    /// The current/next recording to examine. All recordings before this are
    /// synced or aborted.
    pub(crate) recording_id: i32,

    /// The number of bytes for which `write*` system calls have returned.
    pub(crate) written: u32,

    /// The actual file, if it exists and is not currently on the worker's stack.
    /// It is stored here between but must be only used or dropped by the worker.
    file: Option<Antilock<0, std::fs::File>>,

    /// True iff a worker is going to acquire the lock and check for work before going to sleep.
    /// Otherwise it must be retriggered for more work to happen.
    on_worker: bool,
}

impl State {
    pub(crate) fn pos(&self) -> BytePos {
        BytePos {
            recording_id: self.recording_id,
            byte_pos: self.written,
        }
    }
}

/// Attempts to wake the directory pool's writer.
///
/// If the directory pool doesn't exist or isn't open, recordings will be
/// aborted without reaching disk.
pub(crate) fn wake(stream: &Arc<Stream>, l: &mut LockedStream) {
    let Some(pool) = l.sample_file_dir.as_ref().map(|d| d.pool.clone()) else {
        tracing::warn!(stream_id = l.id, "no directory pool to wake");
        return;
    };
    if std::mem::replace(&mut l.writer_state.on_worker, true) {
        return;
    }

    let already_active = l.writer_state.file.is_some();
    if !already_active {
        // It was not on worker and had no file. So it was inactive. Now it is.
        pool.0.inner.lock().write_streams += 1;
    }

    // Allow the send if the pool is open (normal case) or if this stream is
    // already active (has an open file). The latter is needed during pool
    // shutdown: the closing logic waits for `write_streams` to reach 0, so an
    // already-active stream must be woken to finish/abort its recording and
    // decrement `write_streams`.
    if let Err(err) = pool.send(
        |s| already_active || super::is_open(s),
        IoCommand::WakeWriter(stream.clone()),
    ) {
        tracing::warn!(err = %err.chain(), "unable to wake directory pool writer");
        l.writer_state.on_worker = false;
        if !already_active {
            pool.0.inner.lock().write_streams -= 1;
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(f) = self.file.take() {
            std::mem::forget(f);
            if !std::thread::panicking() {
                panic!("open writer file not handled by I/O worker!");
            }
        }
    }
}

struct Abandon;

enum IoStep<'a> {
    Abort {
        file: Antilock<0, std::fs::File>,
    },
    Write {
        file: Option<Antilock<0, std::fs::File>>,
        chunks: &'a mut ArrayVec<Chunk, MAX_CHUNKS_PER_WRITE>,
    },
    Sync {
        file: Antilock<0, std::fs::File>,
    },
}

enum IoStepOutcome {
    Aborted,
    Written {
        /// The newly written bytes.
        bytes: u32,
        file: Antilock<0, std::fs::File>,
    },
    Synced,
}

/// Prepares a list of chunks to write to the file.
fn prepare_write(
    recording: &RecentRecording,
    recent_frames: &RecentFrames,
    written_before: u32,
    chunks: &mut ArrayVec<Chunk, MAX_CHUNKS_PER_WRITE>,
) -> Result<(), Abandon> {
    chunks.clear();
    let mut frames = recent_frames
        .iter_from_byte_pos(BytePos {
            recording_id: recording.id,
            byte_pos: written_before,
        })
        .take_while(|(_, f)| f.recording_id == recording.id);
    if let Some((_, f)) = frames.next() {
        let Some(overlap) = written_before.checked_sub(f.sample_start) else {
            // Writing has fallen too far behind; the next bytes are no longer in RAM.
            // No choice but to abandon the recording.
            return Err(Abandon);
        };
        let chunk = ARefss::new(f.sample.clone()).map(|s| &s[overlap as usize..]);
        assert!(
            !chunk.is_empty(),
            "f.sample_start={f_sample_start} f.sample.len()={f_sample_len} written_before={written_before} overlap={overlap}\n\
            recent_frames={recent_frames:#?}",
            f_sample_start=f.sample_start, f_sample_len=f.sample.len(),
        );
        let mut written_after = f.sample_start + f.sample.len() as u32;
        chunks.push(chunk);
        for (_, f) in frames {
            // There should be no missing frames in the middle.
            if written_after != f.sample_start {
                panic!(
                    "written_before={written_before} recording.id={recording_id}\n\
                        written_after={written_after} f.sample_start={f_sample_start}\n\
                        recent_frames={recent_frames:#?}",
                    recording_id = recording.id,
                    f_sample_start = f.sample_start
                );
            }
            debug_assert_eq!(written_after, f.sample_start);
            let chunk = ARefss::new(f.sample.clone()).map(|s| &s[..]);
            if chunks.try_push(chunk).is_err() {
                // We have reached the maximum number of chunks we can write in one go.
                debug_assert!(written_after < recording.sample_file_bytes);
                return Ok(());
            }
            written_after += f.sample.len() as u32;
        }
        debug_assert_eq!(written_after, recording.sample_file_bytes);
    } else if written_before < recording.sample_file_bytes {
        return Err(Abandon);
    }
    Ok(())
}

impl Worker {
    /// Performs all pending writes for the given stream, handing off finished files to the flusher.
    pub(super) fn write(&self, stream: Arc<Stream>) {
        let _t = TimerGuard::new(&base::clock::RealClocks {}, |_| "dir::Worker::write");
        let mut scratch = ArrayVec::new();
        let mut last: Option<(i32, IoStepOutcome)> = None;

        loop {
            let mut wake_flusher = false; // if it's worth waking the flusher, ideally after dropping the lock.
            let mut stream = stream.inner.lock();
            assert!(stream.writer_state.on_worker);
            let step =
                self.write_iter_prep(&mut stream, &mut wake_flusher, last.take(), &mut scratch);
            if wake_flusher {
                self.shared.config.flusher_notify.notify_one();
            }
            let Some((id, step)) = step else {
                stream.writer_state.on_worker = false;
                if stream.writer_state.file.is_none() {
                    self.dec_write_streams();
                }
                drop(stream);
                return;
            };
            drop(stream);
            last = Some((id.recording(), self.write_iter_step(id, step)))
        }
    }

    /// Write iteration prep step: acquire stream lock and figure out what to do.
    ///
    /// Returns the next action (or `None` to exit the loop) and if the flusher should be awakened.
    fn write_iter_prep<'scratch>(
        &self,
        stream: &mut LockedStream,
        wake_flusher: &mut bool,
        mut last: Option<(i32, IoStepOutcome)>,
        scratch: &'scratch mut ArrayVec<Chunk, MAX_CHUNKS_PER_WRITE>,
    ) -> Option<(CompositeId, IoStep<'scratch>)> {
        let stream_id = stream.id;
        loop {
            let i = stream
                .recent_recordings
                .partition_point(|r| r.id < stream.writer_state.recording_id);
            let mut recording = stream.recent_recordings.get_mut(i);

            // Deal with last iteration.
            if let Some((last_id, last_outcome)) = last.take() {
                assert_eq!(stream.writer_state.recording_id, last_id);
                assert!(stream.writer_state.file.is_none());
                let last_recording = recording.as_mut().filter(|r| r.id == last_id);
                match last_outcome {
                    IoStepOutcome::Aborted => {
                        if let Some(r) = last_recording {
                            r.flags.insert(RecordingFlags::DELETED);
                        }
                        stream.writer_state = State {
                            recording_id: last_id + 1,
                            written: 0,
                            file: None,
                            on_worker: true,
                        };
                        stream.recent_frames.prune_front(stream.writer_state.pos());
                        stream.maybe_prune_recent_recordings();
                        continue; // look at next recording.
                    }
                    IoStepOutcome::Written { bytes, file } => {
                        stream.writer_state.written += bytes;
                        stream.writer_state.file = Some(file);
                        stream.recent_frames.prune_front(stream.writer_state.pos());
                    }
                    IoStepOutcome::Synced => {
                        let Some(r) = last_recording else {
                            panic!("fully written recording {stream_id}/{last_id} was pruned")
                        };
                        assert_eq!(stream.writer_state.written, r.sample_file_bytes);
                        stream.writer_state = State {
                            recording_id: last_id + 1,
                            written: 0,
                            file: None,
                            on_worker: true,
                        };
                        stream.recent_frames.prune_front(stream.writer_state.pos());
                        *wake_flusher = true;
                        continue; // look at next recording.
                    }
                }
            }

            if let Some(file) = stream.writer_state.file.take_if(|_| {
                Some(stream.writer_state.recording_id) != recording.as_ref().map(|r| r.id)
            }) {
                // Opened file for a pruned recording.
                tracing::debug!(
                    stream_id,
                    "apparently recording {} has been pruned, recents = [{}]",
                    stream.writer_state.recording_id,
                    stream.recent_recordings.iter().map(|r| r.id).join(", "),
                );
                return Some((
                    CompositeId::new(stream_id, stream.writer_state.recording_id),
                    IoStep::Abort { file },
                ));
            }

            let Some(recording) = recording else {
                tracing::debug!(
                    stream_id,
                    "waiting for recording >= {}",
                    stream.writer_state.recording_id,
                );
                return None; // wait for more recordings
            };

            if recording.id != stream.writer_state.recording_id {
                assert_eq!(stream.writer_state.written, 0);
                stream.writer_state.recording_id = recording.id; // advance
            }

            // `recording` and `writer_state` match.
            if prepare_write(
                recording,
                &stream.recent_frames,
                stream.writer_state.written,
                scratch,
            )
            .is_err()
            {
                tracing::warn!(
                    stream_id,
                    recording_id = recording.id,
                    "abandoning recording after falling behind",
                );
                if let Some(file) = stream.writer_state.file.take() {
                    return Some((
                        CompositeId::new(stream_id, recording.id),
                        IoStep::Abort { file },
                    ));
                }
                recording.flags.insert(RecordingFlags::DELETED);
                stream.writer_state = State {
                    recording_id: recording.id + 1,
                    file: None,
                    written: 0,
                    on_worker: true,
                };
                continue; // look at next recording
            }
            if !scratch.is_empty() {
                return Some((
                    CompositeId::new(stream_id, recording.id),
                    IoStep::Write {
                        file: stream.writer_state.file.take(),
                        chunks: scratch,
                    },
                ));
            }
            if recording.flags.contains(RecordingFlags::GROWING) {
                return None; // wait for more data in `recording`
            }
            let file = stream
                .writer_state
                .file
                .take()
                .expect("fully written file should be open");
            return Some((
                CompositeId::new(stream_id, recording.id),
                IoStep::Sync { file },
            ));
        }
    }

    fn abort(&self, id: CompositeId, file: Antilock<0, std::fs::File>) -> IoStepOutcome {
        tracing::debug!(%id, "abort");
        drop(file);
        if let Err(err) = self.unlink(id) {
            tracing::error!(%id, "unable to unlink on abort: {err}");

            // XXX: In theory before the stream's `committed.cum_recordings` is
            // advanced past this id, it should be appended to the `garbage`
            // table and the pool's `garbage_needs_unlink` set. This would require
            // some extra plumbing to achieve, like an extra staging area within
            // `LockedStream` that is checked from `flush`.
        }
        IoStepOutcome::Aborted
    }

    fn write_iter_step(&self, id: CompositeId, step: IoStep) -> IoStepOutcome {
        match step {
            IoStep::Abort { file } => self.abort(id, file),
            IoStep::Write { file, chunks } => {
                let mut file = match file.map(Ok).unwrap_or_else(|| {
                    // Create a new file for the recording.
                    let p = super::CompositeIdPath::from(id);
                    crate::fs::openat(
                        self.dir.0,
                        &p,
                        OFlag::O_WRONLY | OFlag::O_EXCL | OFlag::O_CREAT,
                        Mode::S_IRUSR | Mode::S_IWUSR,
                    )
                    .map(Antilock::new)
                }) {
                    Ok(f) => f,
                    Err(err) => {
                        tracing::error!(%err, %id, "failed to open recording for writing");
                        return IoStepOutcome::Aborted;
                    }
                };
                let mut bufs = ArrayVec::<std::io::IoSlice, MAX_CHUNKS_PER_WRITE>::new();
                let mut tried = 0;
                for chunk in chunks.iter() {
                    bufs.push(std::io::IoSlice::new(chunk));
                    tried += chunk.len();
                }
                match file.borrow_mut().write_vectored(&bufs[..]) {
                    Ok(bytes) => {
                        assert!(bytes <= tried, "written:{bytes} should be <= tried:{tried}");
                        IoStepOutcome::Written {
                            bytes: bytes as u32,
                            file,
                        }
                    }
                    Err(err) => {
                        tracing::warn!(%id, %err, "abandoning recording due to write error");
                        self.abort(id, file)
                    }
                }
            }
            IoStep::Sync { file } => {
                if let Err(e) = file.borrow().sync_all() {
                    // On Linux, after an `fsync` failure, the file contents are
                    // unknown. See the PostgreSQL "fsyncgate 2018" discussions:
                    // <https://wiki.postgresql.org/wiki/Fsync_Errors>
                    //
                    // We could see if all the frames are still in
                    // `recent_frames` and thus the file is potentially
                    // recoverable, but it's not worth it. Just abort.
                    tracing::warn!(%id, %e, "failed to sync file; will abandon recording");
                    return self.abort(id, file);
                }
                drop(file);

                if let Err(err) = nix::unistd::fsync(self.dir.0) {
                    // Unsure what the OS behavior is here: is the filesystem in
                    // a valid state in-RAM? What happened to other streams'
                    // files in the same directory that were written since the
                    // last sync? Is there any safe way to proceed? Is it likely
                    // anyway that following `fsync` calls will go through?
                    // Without answers, we're giving up.

                    // 1. Log normally.
                    tracing::error!(err = %err, "aborting due to failed fsync of sample file directory");

                    // 2. `eprintln!` to ensure the message is seen, even if the logging
                    // filter is strange or the abort below happens before any
                    // asynchronous logging completes.
                    eprintln!(
                        "aborting due to failed fsync of sample file directory {dir}: {err}",
                        dir = self.shared.config.path.display()
                    );

                    // 3. Abort. Not just panic this thread; we don't want other
                    // threads in this sample file directory pool to continue
                    // on. We could poison the pool's writing in some fashion
                    // instead and proceed with operation otherwise, but that's
                    // a lot of extra tricky plumbing for a rare failure mode.
                    std::process::abort();
                }
                tracing::debug!(%id, "synced");
                IoStepOutcome::Synced
            }
        }
    }

    #[inline(never)]
    fn dec_write_streams(&self) {
        let mut l = self.shared.inner.lock();
        l.write_streams = l
            .write_streams
            .checked_sub(1)
            .expect("write_streams is balanced");
        if l.write_streams == 0 {
            if matches!(l.state, super::State::Closing { .. }) {
                self.shared.worker_notify.notify_all();
            }
            self.shared.no_write_streams_notify.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{pin::pin, sync::Arc};

    use futures::FutureExt as _;

    use crate::{
        recording,
        stream::{recent_frames::RecentFrames, LockedStream},
        writer::Writer,
        RecentFrame, RecordingFlags,
    };

    #[tokio::test]
    async fn basic() {
        crate::testutil::init();
        let tmpdir = tempfile::Builder::new()
            .prefix("moonfire-db-test-dir-writer")
            .tempdir()
            .unwrap();

        let stream = crate::stream::Stream::new(LockedStream::dummy());
        let flusher_notify = Arc::new(tokio::sync::Notify::new());
        let dir_pool = crate::dir::Pool::new_for_test(tmpdir.path(), flusher_notify.clone()).await;
        {
            let mut l = stream.inner.lock();
            l.sample_file_dir = Some(crate::db::SampleFileDir {
                id: 0,
                pool: dir_pool.clone(),
            });
        }

        let mut writer = Writer::new(stream.clone()).unwrap();
        writer
            .write(
                b"hello"[..].into(),
                recording::Time(1),
                0,
                true,
                false,
                /* video_sample_entry_id */ 0,
            )
            .unwrap();
        let notified = flusher_notify.notified();
        writer.close("asdf".to_owned());
        let recording_id = {
            let l = stream.inner.lock();
            let r = l.recent_recordings.back().unwrap();
            assert_eq!(
                r.flags,
                (RecordingFlags::TRAILING_ZERO | RecordingFlags::UNCOMMITTED)
            );
            r.id
        };

        notified.await;

        let l = stream.inner.lock();
        assert_eq!(l.writer_state.recording_id, recording_id + 1);
        assert_eq!(l.writer_state.written, 0);
        let r = l.recent_recordings.back().unwrap();
        assert_eq!(
            r.flags,
            RecordingFlags::UNCOMMITTED | RecordingFlags::TRAILING_ZERO
        );
        assert_eq!(
            l.recent_frames.front().map(RecentFrame::start),
            Some(crate::stream::BytePos {
                recording_id: 0,
                byte_pos: 0
            })
        );

        let path = tmpdir
            .path()
            .join(crate::dir::CompositeIdPath::from(crate::CompositeId::new(
                l.id,
                recording_id,
            )));
        let content = std::fs::read(path).unwrap();
        assert_eq!(content, b"hello");
    }

    /// Tests that a recording is abandoned (marked DELETED) when its frames
    /// have been pruned from `recent_frames` before the worker could write them.
    #[tokio::test]
    async fn abandon_behind() {
        crate::testutil::init();
        let tmpdir = tempfile::Builder::new()
            .prefix("moonfire-db-test-dir-writer-abandon")
            .tempdir()
            .unwrap();

        // Create a stream *without* a pool so writes via Writer create
        // recent_recordings + recent_frames but no worker runs.
        let stream = crate::stream::Stream::new(LockedStream::dummy());

        let mut writer = Writer::new(stream.clone()).unwrap();
        writer
            .write(b"hello"[..].into(), recording::Time(1), 0, true, false, 0)
            .unwrap();
        writer.close("test".to_owned());

        let recording_id = {
            let l = stream.inner.lock();
            l.recent_recordings.back().unwrap().id
        };

        // Simulate frames falling behind by clearing recent_frames.
        // The recording still expects sample_file_bytes > 0, but there are
        // no frames to satisfy the writer.
        {
            let mut l = stream.inner.lock();
            assert!(l.recent_frames.len() > 0);
            l.recent_frames = RecentFrames::default();
        }

        // Now attach the pool and wake the writer.
        let flusher_notify = Arc::new(tokio::sync::Notify::new());
        let dir_pool = crate::dir::Pool::new_for_test(tmpdir.path(), flusher_notify).await;
        {
            let mut l = stream.inner.lock();
            l.sample_file_dir = Some(crate::db::SampleFileDir {
                id: 0,
                pool: dir_pool.clone(),
            });
            super::wake(&stream, &mut l);
        }

        // Wait for the worker to finish processing.
        dir_pool.await_no_write_streams().await;

        let l = stream.inner.lock();
        // Writer should have advanced past the abandoned recording.
        assert_eq!(l.writer_state.recording_id, recording_id + 1);
        assert_eq!(l.writer_state.written, 0);
        // The recording should be marked DELETED.
        let r = l
            .recent_recordings
            .iter()
            .find(|r| r.id == recording_id)
            .unwrap();
        assert!(r.flags.contains(RecordingFlags::DELETED));
        // No file should have been created.
        let path = tmpdir
            .path()
            .join(crate::dir::CompositeIdPath::from(crate::CompositeId::new(
                l.id,
                recording_id,
            )));
        assert!(!path.exists());
    }

    /// Tests that the writer handles multiple recordings in sequence:
    /// write + sync recording 1, then write + sync recording 2.
    /// This exercises the `IoStepOutcome::Synced` path that advances to the
    /// next recording.
    #[tokio::test]
    async fn multi_recording() {
        crate::testutil::init();
        let tmpdir = tempfile::Builder::new()
            .prefix("moonfire-db-test-dir-writer-multi")
            .tempdir()
            .unwrap();

        let stream = crate::stream::Stream::new(LockedStream::dummy());
        let flusher_notify = Arc::new(tokio::sync::Notify::new());
        let dir_pool = crate::dir::Pool::new_for_test(tmpdir.path(), flusher_notify.clone()).await;
        {
            let mut l = stream.inner.lock();
            l.sample_file_dir = Some(crate::db::SampleFileDir {
                id: 0,
                pool: dir_pool.clone(),
            });
        }

        let mut writer = Writer::new(stream.clone()).unwrap();

        // First frame of recording 1.
        writer
            .write(b"rec1a"[..].into(), recording::Time(1), 0, true, false, 0)
            .unwrap();
        // Second frame, with rotate_now=true to close recording 1 and start recording 2.
        writer
            .write(
                b"rec2a"[..].into(),
                recording::Time(2),
                1,
                true,
                /* rotate_now */ true,
                0,
            )
            .unwrap();

        writer.close("done".to_owned());

        // The dir writer will notify the flusher via `notify_one` when it's done with both recordings.
        flusher_notify.notified().await;

        // ...but it may also notify after the first recording, if it finished it while the stream writer
        // was writing the second. So wait a second time if necessary.
        {
            let l = stream.inner.lock();
            assert_eq!(l.recent_recordings.len(), 2);
            if l.writer_state.recording_id < l.recent_recordings[1].id + 1 {
                drop(l);
                flusher_notify.notified().await;
            }
        }

        let l = stream.inner.lock();
        // Should have two recordings.
        assert_eq!(l.recent_recordings.len(), 2);
        let r0 = &l.recent_recordings[0];
        let r1 = &l.recent_recordings[1];
        let r0_id = r0.id;
        let r1_id = r1.id;
        assert_eq!(r1_id, r0_id + 1);

        // Writer state should be past both recordings.
        assert_eq!(l.writer_state.recording_id, r1_id + 1);
        assert_eq!(l.writer_state.written, 0);

        // Both should be non-GROWING and not DELETED.
        assert!(!r0.flags.contains(RecordingFlags::GROWING));
        assert!(!r0.flags.contains(RecordingFlags::DELETED));
        assert!(!r1.flags.contains(RecordingFlags::GROWING));
        assert!(!r1.flags.contains(RecordingFlags::DELETED));

        // Verify file contents on disk.
        let path0 = tmpdir
            .path()
            .join(crate::dir::CompositeIdPath::from(crate::CompositeId::new(
                l.id, r0_id,
            )));
        let path1 = tmpdir
            .path()
            .join(crate::dir::CompositeIdPath::from(crate::CompositeId::new(
                l.id, r1_id,
            )));
        assert_eq!(std::fs::read(path0).unwrap(), b"rec1a");
        assert_eq!(std::fs::read(path1).unwrap(), b"rec2a");
    }

    /// Tests that `wake` with no directory pool logs a warning and returns
    /// without panicking or setting `on_worker`.
    #[tokio::test]
    async fn wake_no_pool() {
        crate::testutil::init();
        let stream = crate::stream::Stream::new(LockedStream::dummy());
        {
            let mut l = stream.inner.lock();
            assert!(l.sample_file_dir.is_none());
            super::wake(&stream, &mut l);
            // on_worker should not have been set.
            assert!(!l.writer_state.on_worker);
        }
    }

    /// Tests pool shutdown with two concurrent write streams.
    ///
    /// This exercises the fix for a busy-loop bug in `Worker::run`. With two
    /// streams, when the first finishes `write_streams` goes 2→1 (not yet 0),
    /// so `dec_write_streams` doesn't call `worker_notify.notify_all()`. The
    /// worker re-enters its idle loop, sees `Closing + write_streams > 0`, and
    /// without the fix busy-loops holding the mutex — preventing the second
    /// stream from ever decrementing `write_streams` and completing shutdown.
    #[tokio::test]
    async fn two_streams_shutdown() {
        crate::testutil::init();
        let tmpdir = tempfile::Builder::new()
            .prefix("moonfire-db-test-dir-writer-two")
            .tempdir()
            .unwrap();

        let stream_a = crate::stream::Stream::new(LockedStream::dummy_with_id(1));
        let stream_b = crate::stream::Stream::new(LockedStream::dummy_with_id(2));
        let flusher_notify = Arc::new(tokio::sync::Notify::new());
        let dir_pool = crate::dir::Pool::new_for_test(tmpdir.path(), flusher_notify.clone()).await;

        for stream in [&stream_a, &stream_b] {
            let mut l = stream.inner.lock();
            l.sample_file_dir = Some(crate::db::SampleFileDir {
                id: 0,
                pool: dir_pool.clone(),
            });
        }

        // Give each stream 3 key frames:
        // frame 0: is deferred because there's no duration
        // frame 1: causes frame 0 to be written to `recent_frames`, but no wake
        // frame 2: wakes the worker to write frame 0.
        let mut writer_a = Writer::new(stream_a.clone()).unwrap();
        writer_a
            .write(b"aa1"[..].into(), recording::Time(1), 0, true, false, 0)
            .unwrap();
        writer_a
            .write(b"aa2"[..].into(), recording::Time(2), 1, true, false, 0)
            .unwrap();
        writer_a
            .write(b"aa3"[..].into(), recording::Time(3), 2, true, false, 0)
            .unwrap();
        assert_eq!(dir_pool.0.inner.lock().write_streams, 1);

        let mut writer_b = Writer::new(stream_b.clone()).unwrap();
        writer_b
            .write(b"bb1"[..].into(), recording::Time(1), 0, true, false, 0)
            .unwrap();
        writer_b
            .write(b"bb2"[..].into(), recording::Time(2), 1, true, false, 0)
            .unwrap();
        writer_b
            .write(b"bb3"[..].into(), recording::Time(3), 2, true, false, 0)
            .unwrap();
        assert_eq!(dir_pool.0.inner.lock().write_streams, 2);

        // Start closing the directory pool. It should not complete because the open write streams prevent this.
        let mut close = pin!(dir_pool.close());
        assert!(close.as_mut().now_or_never().is_none());

        drop(writer_a);
        drop(writer_b);

        close.await.unwrap();
    }

    /// Tests that `wake` short-circuits when `on_worker` is already true.
    #[tokio::test]
    async fn wake_already_on_worker() {
        crate::testutil::init();
        let tmpdir = tempfile::Builder::new()
            .prefix("moonfire-db-test-dir-writer-wake")
            .tempdir()
            .unwrap();

        let stream = crate::stream::Stream::new(LockedStream::dummy());
        let flusher_notify = Arc::new(tokio::sync::Notify::new());
        let dir_pool = crate::dir::Pool::new_for_test(tmpdir.path(), flusher_notify).await;
        {
            let mut l = stream.inner.lock();
            l.sample_file_dir = Some(crate::db::SampleFileDir {
                id: 0,
                pool: dir_pool.clone(),
            });

            // First wake: should set on_worker and increment write_streams.
            super::wake(&stream, &mut l);
            assert!(l.writer_state.on_worker);

            // Second wake: should short-circuit (on_worker already true).
            // No panic, no double-increment of write_streams.
            super::wake(&stream, &mut l);
            assert!(l.writer_state.on_worker);
        }

        // The worker was woken but there's no work — it will set on_worker=false
        // and decrement write_streams.
        dir_pool.await_no_write_streams().await;

        let l = stream.inner.lock();
        assert!(!l.writer_state.on_worker);
    }
}
