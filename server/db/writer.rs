// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Writing recordings.

use crate::db::{self, CompositeId};
use crate::recording::{self, SampleIndexEncoder, MAX_RECORDING_WALL_DURATION};
use crate::stream::recent_frames::RecentFrame;
use crate::stream::{LockedStream, Stream};
use crate::{dir, RecentRecording, RecordingFlags};
use base::{bail, Error};
use std::cmp;
use std::collections::VecDeque;
use std::convert::TryFrom;
use std::sync::Arc;
use tracing::debug;

/// Struct to manage writing a single run (of potentially several recordings) into the database in
/// memory. Each streamer task should have a single `Writer`.
///
/// The `Writer` creates recent recordings lazily (when adding a frame, which
/// lags by one `write` call due to duration handling). Created recordings
/// always have at least one frame before becoming visible (stream lock
/// dropped). Unless the writer is dropped without any calls to `write`, the run
/// will always terminate with a zero-duration frame and an "end reason" (which
/// is just "drop" if `close` is not called).
///
/// Recordings' "local start time" will be determined by minimum of local time +
/// previous frames' duration for all frames included in the recording. (Notably,
/// this includes the zero-duration frame at the end, which may be the only frame.)
/// This will also be the "start time" for the first recording in a run.
///
/// Calls into the `Writer` do not actually perform I/O before returning; they make frames visible
/// to watchers and trigger directory I/O pool operations asynchronously, which in turn will prompt
/// the flusher to perform SQLite operations.
///
/// If there is no open directory I/O pool or it falls behind, recordings will
/// ultimately be aborted.
///
/// If there is no flusher, unflushed recordings will accumulate, and the sample files will be
/// abandoned on next startup.
pub struct Writer {
    stream: Arc<Stream>,

    /// Always `Some` unless poisoned / in drop.
    inner: Option<InnerWriter>,
}

/// State for writing a single recording, used within [Writer].
#[derive(Default)]
struct InnerWriter {
    /// The active recording. Must be the most recent recording on the stream, and must have `GROWING | UNCOMMITTED` flags.
    recording_id: Option<i32>,

    run_offset: i32,
    e: SampleIndexEncoder,
    media_duration_90k: i32,
    wall_duration_90k: i32,
    start: recording::Time,
    local_start: recording::Time,
    hasher: base::Antilock<0, blake3::Hasher>,

    /// A sample which is not finished (included in the fields above, or added to a `RecentRecording`) because its duration
    /// will be determined by the following sample's pts (or as 0 by an unclean close).
    unfinished_sample: Option<UnfinishedSample>,
}

/// A sample which is queued within the `Writer`.
/// The `RecentFrame` includes the sample's duration, which is calculated from the
/// _following_ sample's pts, so the most recent sample is always unfinished.
struct UnfinishedSample {
    local_time: recording::Time,
    pts_90k: i64, // relative to the start of the run, not a single recording.
    sample: Vec<u8>,
    is_key: bool,
    video_sample_entry_id: i32,
}

impl Writer {
    /// Creates a new writer for the given stream.
    ///
    /// `stream` must not be locked by the current thread, or the operation will deadlock.
    /// Returns an error if the stream already has an open writer.
    pub fn new(stream: Arc<Stream>) -> Result<Self, Error> {
        let (stream_id, existing_writer) = {
            let mut l = stream.inner.lock();
            (l.id, std::mem::replace(&mut l.open_writer, true))
        };
        if existing_writer {
            bail!(
                FailedPrecondition,
                msg("stream {stream_id} already has an open writer")
            );
        }
        Ok(Writer {
            stream,
            inner: Some(InnerWriter {
                local_start: recording::Time::MAX,
                start: recording::Time::MAX,
                ..Default::default()
            }),
        })
    }

    /// Takes responsibility for the given sample.
    ///
    /// `local_time` should be the local clock's time as of when this packet was received.
    ///
    /// This actually writes the *previous* frame, if any. The lag is to allow the caller to
    /// accept frames with a PTS only and store frames with a duration.
    ///
    /// On `Err` return, the current frame was discarded due to invalid
    /// timestamps, and the previous frame was written with zero duration. The
    /// writer should not be reused; following calls will panic.
    pub fn write(
        &mut self,
        sample: Vec<u8>,
        local_time: recording::Time,
        pts_90k: i64,
        is_key: bool,
        rotate_now: bool,
        video_sample_entry_id: i32,
    ) -> Result<(), String> {
        debug_assert!(local_time > recording::Time(0));
        let inner = self.inner.as_mut().expect("should be unpoisoned");

        // If there's a previous sample, flush it.
        if let Some(unfinished) = inner.unfinished_sample.take() {
            let prev_vse = unfinished.video_sample_entry_id;
            match inner.adj_media_duration(&unfinished, pts_90k) {
                Err(e) => {
                    let blake3 = inner.hasher.borrow().finalize();
                    let mut locked = self.stream.inner.lock();
                    inner.adj_wall_and_start(&unfinished);
                    inner.push(&self.stream, &mut locked, unfinished, 0);
                    inner.close(&self.stream, &mut locked, blake3, Some(e.clone()));
                    drop(locked);
                    self.inner.take(); // poison.
                    return Err(e);
                }
                Ok(duration_90k) => {
                    let close_with_blake3 = if rotate_now || video_sample_entry_id != prev_vse {
                        let mut hasher = inner.hasher.borrow_mut();
                        let blake3 = hasher.finalize();
                        hasher.reset();
                        drop(hasher);
                        Some(blake3)
                    } else {
                        None
                    };
                    let mut locked = self.stream.inner.lock();
                    inner.push(&self.stream, &mut locked, unfinished, duration_90k);
                    if let Some(blake3) = close_with_blake3 {
                        inner.close(&self.stream, &mut locked, blake3, None);
                    }
                }
            };
        }

        inner.hasher.borrow_mut().update(&sample);
        inner.unfinished_sample = Some(UnfinishedSample {
            local_time,
            pts_90k,
            sample,
            is_key,
            video_sample_entry_id,
        });
        Ok(())
    }

    /// Ends the run with the given reason.
    pub fn close(mut self, reason: String) {
        let Some(mut inner) = self.inner.take() else {
            return;
        };
        let Some(sample) = inner.unfinished_sample.take() else {
            return;
        };
        let blake3 = inner.hasher.borrow_mut().finalize();
        let mut locked = self.stream.inner.lock();
        inner.adj_wall_and_start(&sample);
        inner.push(&self.stream, &mut locked, sample, 0);
        inner.close(&self.stream, &mut locked, blake3, Some(reason));
    }
}

fn get(recent: &mut VecDeque<RecentRecording>, expected_recording_id: i32) -> &mut RecentRecording {
    let r = recent.back_mut().expect("no recent recordings");
    assert!(
        r.id == expected_recording_id && r.flags.contains(RecordingFlags::UNCOMMITTED),
        "expected growing recording {expected_recording_id}; got {flags:?} recording {id}",
        flags = r.flags,
        id = r.id
    );
    r
}

impl InnerWriter {
    /// Adjusts the total media duration to include `sample`.
    ///
    /// Returns the sample duration on success, as needed by `SampleIndexEncoder::add_sample`.
    ///
    /// Fails on out-of-range timestamps without adjusting anything.
    fn adj_media_duration(
        &mut self,
        sample: &UnfinishedSample,
        next_pts_90k: i64,
    ) -> Result<i32, String> {
        let duration_90k = next_pts_90k.wrapping_sub(sample.pts_90k);
        if duration_90k <= 0 {
            return Err(format!(
                "pts not monotonically increasing; got {sample_pts_90k} then {next_pts_90k}",
                sample_pts_90k = sample.pts_90k,
            ));
        }

        // It's really the wall duration that has to be within bounds, but the
        // media->wall duration allows some wiggle room. The easiest thing to do
        // is to ensure the media duration is within bounds, then
        // duration to match.
        let media_duration_90k = i64::from(self.media_duration_90k).saturating_add(duration_90k);
        if media_duration_90k > MAX_RECORDING_WALL_DURATION {
            return Err(format!(
                "media duration {media_duration_90k} exceeds maximum {MAX_RECORDING_WALL_DURATION}",
                media_duration_90k = recording::Duration(media_duration_90k),
                MAX_RECORDING_WALL_DURATION = recording::Duration(MAX_RECORDING_WALL_DURATION),
            ));
        }
        self.media_duration_90k = media_duration_90k as i32;
        self.adj_wall_and_start(sample);
        Ok(duration_90k as i32)
    }

    fn adj_wall_and_start(&mut self, sample: &UnfinishedSample) {
        let local_start = cmp::min(
            self.local_start,
            sample.local_time - recording::Duration(i64::from(self.media_duration_90k)),
        );
        let limit = i64::from(self.media_duration_90k) / 2000; // 1/2000th, aka 500 ppm.
        let start = if self.run_offset == 0 {
            // Start time isn't anchored to previous recording's end; adjust.
            local_start
        } else {
            self.start
        };
        let wall_duration_90k = (i64::from(self.media_duration_90k)
            + (local_start.0 - start.0).clamp(-limit, limit))
        .min(MAX_RECORDING_WALL_DURATION);

        // `limit` should always be <= media_duration_90k, so media_duration_90k + (...).clamp(-limit, ...) shoudl always be non-negative.
        debug_assert!(wall_duration_90k >= 0);
        self.wall_duration_90k = wall_duration_90k as i32;
        self.local_start = local_start;
        self.start = start;
    }

    /// Pushes the given sample, which may have zero duration.
    ///
    /// `duration_90k` should be as returned by `adj_media_duration` or 0;
    /// in the latter case, `adj_wall_and_start` should have been called separately.
    fn push(
        &mut self,
        stream_arc: &Arc<Stream>,
        locked: &mut LockedStream,
        sample: UnfinishedSample,
        duration_90k: i32,
    ) {
        let r = match self.recording_id {
            Some(id) => get(&mut locked.recent_recordings, id),
            None => {
                let id = locked.add_recording(RecentRecording {
                    run_offset: self.run_offset,
                    start: self.start,
                    local_time_delta: self.local_start - self.start,
                    video_sample_entry_id: sample.video_sample_entry_id,
                    flags: db::RecordingFlags::GROWING | db::RecordingFlags::UNCOMMITTED,
                    ..Default::default()
                });
                debug!(id = %CompositeId::new(locked.id, id), "added recording");
                self.recording_id = Some(id);
                get(&mut locked.recent_recordings, id)
            }
        };
        if duration_90k == 0 {
            r.flags.insert(RecordingFlags::TRAILING_ZERO);
        }
        let prev_media_duration_90k = r.media_duration_90k;
        let sample_start = r.sample_file_bytes;
        self.e.add_sample(
            duration_90k,
            u32::try_from(sample.sample.len()).unwrap(),
            sample.is_key,
            r,
        );
        assert_eq!(r.media_duration_90k, self.media_duration_90k); // `SampleIndexEncoder` just made this change.
        r.wall_duration_90k = self.wall_duration_90k;
        r.start = self.start;
        r.local_time_delta = self.local_start - self.start;
        let wake_writer = (sample.is_key && prev_media_duration_90k > 0)
            || (locked.writer_state.recording_id == r.id
                && r.sample_file_bytes.strict_sub(locked.writer_state.written)
                    >= (crate::stream::recent_frames::BYTES_FOR_WRITER >> 1) as u32);
        let frame = RecentFrame {
            recording_id: r.id,
            is_key: sample.is_key,
            media_off_90k: prev_media_duration_90k..r.media_duration_90k,
            sample: Arc::new(sample.sample),
            sample_start,
        };
        locked.recent_frames.push_back(frame);
        locked.recent_frames.prune_front(locked.writer_state.pos());
        locked.maybe_prune_recent_recordings();
        stream_arc.recent_frames_notify.notify_waiters();
        if wake_writer {
            dir::writer::wake(stream_arc, locked);
        }
    }

    /// Closes the active recording.
    ///
    /// Resets much of the state; caller is responsible for `hasher`.
    fn close(
        &mut self,
        stream_arc: &Arc<Stream>,
        locked: &mut LockedStream,
        blake3: blake3::Hash,
        end_reason: Option<String>,
    ) {
        let recording_id = self.recording_id.expect("have recording id");
        let r = get(&mut locked.recent_recordings, recording_id);
        assert_eq!(r.media_duration_90k, self.media_duration_90k);
        assert_eq!(r.wall_duration_90k, self.wall_duration_90k);
        assert_eq!(r.start, self.start);
        assert_eq!(r.start + r.local_time_delta, self.local_start);
        r.flags.remove(RecordingFlags::GROWING);
        assert_eq!(locked.complete.cum_recordings, r.id);
        locked.complete.cum_media_duration.0 += i64::from(r.media_duration_90k);
        locked.complete.cum_runs += i32::from(self.run_offset == 0);
        locked.complete.cum_recordings += 1;
        r.sample_file_blake3 = Some(*blake3.as_bytes());
        r.end_reason = end_reason;
        dir::writer::wake(stream_arc, locked);
        self.start.0 += i64::from(self.wall_duration_90k);
        self.local_start = base::time::Time::MAX;
        self.wall_duration_90k = 0;
        self.media_duration_90k = 0;
        self.run_offset += 1;
        self.recording_id = None;
        self.e = SampleIndexEncoder::default();
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let mut locked = None;
        if let Some(inner) = self.inner.as_mut() {
            if let Some(sample) = inner.unfinished_sample.take() {
                let blake3 = inner.hasher.borrow_mut().finalize();
                let locked = locked.insert(self.stream.inner.lock());
                inner.adj_wall_and_start(&sample);
                inner.push(&self.stream, locked, sample, 0);
                inner.close(&self.stream, locked, blake3, Some("drop".to_owned()));
            }
        }
        let l = locked.get_or_insert_with(|| self.stream.inner.lock());
        assert!(l.open_writer);
        l.open_writer = false;
    }
}

#[cfg(test)]
mod tests {
    use tracing::debug;

    use super::Writer;
    use crate::stream::{BytePos, LockedStream, Stream};
    use crate::{recording, testutil, RecordingFlags};

    #[test]
    fn write_normal() {
        testutil::init();
        const VIDEO_SAMPLE_ENTRY_ID: i32 = 0;
        let stream = Stream::new(LockedStream::dummy());

        let mut writer = Writer::new(stream.clone()).unwrap();
        debug!("writing first sample");
        writer
            .write(
                Vec::from(b"foo"),
                recording::Time(2),
                0,
                true,
                false,
                VIDEO_SAMPLE_ENTRY_ID,
            )
            .unwrap();
        debug!("writing second sample");
        writer
            .write(
                Vec::from(b"bar"),
                recording::Time(3),
                1,
                false,
                false,
                VIDEO_SAMPLE_ENTRY_ID,
            )
            .unwrap();
        debug!("closing");
        writer.close("done".to_owned());
        debug!("done");
        let l = stream.inner.lock();
        let r = l.recent_recordings.back().unwrap();
        assert_eq!(r.sample_file_bytes, 6);
        assert_eq!(r.video_samples, 2);
        assert_eq!(
            r.sample_file_blake3.as_ref(),
            Some(blake3::hash(b"foobar").as_bytes()),
        );
        let frames: Vec<_> = l
            .recent_frames
            .iter_from_byte_pos(BytePos {
                recording_id: r.id,
                byte_pos: 0,
            })
            .collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].1.media_off_90k.end, 1);
        assert_eq!(frames[1].1.media_off_90k.end, 1);
        assert_eq!(
            r.flags,
            RecordingFlags::TRAILING_ZERO | RecordingFlags::UNCOMMITTED
        );
        assert_eq!(r.end_reason.as_deref(), Some("done"));
    }

    #[test]
    fn write_non_monotonic_pts() {
        testutil::init();
        const VIDEO_SAMPLE_ENTRY_ID: i32 = 0;
        let stream = Stream::new(LockedStream::dummy());

        let mut writer = Writer::new(stream.clone()).unwrap();
        writer
            .write(
                Vec::from(b"frame1"),
                recording::Time(2),
                1000,
                true,
                false,
                VIDEO_SAMPLE_ENTRY_ID,
            )
            .unwrap();

        // Test non-monotonic pts
        let err = writer
            .write(
                Vec::from(b"frame2"),
                recording::Time(1),
                1000,
                true,
                false,
                VIDEO_SAMPLE_ENTRY_ID,
            )
            .unwrap_err();
        assert!(err.contains("pts not monotonically increasing"));
        drop(writer);
        let l = stream.inner.lock();
        let r = l.recent_recordings.back().unwrap();
        let frames: Vec<_> = l
            .recent_frames
            .iter_from_byte_pos(BytePos {
                recording_id: r.id,
                byte_pos: 0,
            })
            .collect();
        assert_eq!(frames.len(), 1);
        assert_eq!(
            r.flags,
            RecordingFlags::TRAILING_ZERO | RecordingFlags::UNCOMMITTED
        );
    }

    /// Tests properties of consecutive recordings within a single run.
    ///
    /// Verifies that each recording's `start`, `run_offset`, `wall_duration_90k`, and
    /// `local_time_delta` are computed correctly with respect to each recording's own frames,
    /// independently of earlier recordings in the same run.
    #[test]
    fn multi_recording_run() {
        testutil::init();
        const VIDEO_SAMPLE_ENTRY_ID: i32 = 0;
        let stream = Stream::new(LockedStream::dummy());

        // 90 kHz units. Each frame has media duration F = 90_000 ticks (1 second).
        // T is an arbitrary nonzero base time for the wall clock.
        //
        // local_time for frame i = T + (i+1)*F, modeling a clock that starts at T and advances
        // one frame duration per frame. The estimated wall start is
        //   local_time_i - media_duration_including_i = T + (i+1)*F - (i+1)*F = T.
        //
        // Each write(fN) flushes the previous unfinished frame with duration pts_fN - pts_f(N-1),
        // optionally closing the active recording after that flush (rotate_now). So rotate_now on
        // write(fN) closes recording 1 after flushing f(N-1); fN becomes the first frame of rec 2.
        //
        // Recording 1: 3 frames (f0, f1, f2) with local clock matching media (no skew).
        //   pts:        0,    F,   2F  (f3's pts determines f2's duration)
        //   local_time: T+F, T+2F, T+3F
        //   run_offset = 0, so start floats with local_start.
        //   local_start = min(T+F-F, T+2F-2F, T+3F-3F) = T
        //   start       = local_start = T
        //   wall_duration  = 3F + clamp(T - T, ±(3F/2000)) = 3F
        //   local_time_delta = 0
        //
        // Recording 2: 2 frames (f3, f4) with local clock matching media (no skew).
        //   After close of rec 1:  self.start = T + 3F,  self.local_start = T + 3F
        //   pts:        3F, 4F  (trailing zero at 5F from writer.close())
        //   local_time: T+4F, T+5F
        //   run_offset = 1, so start is anchored (= T+3F).
        //   f4 flushes f3: local_start = min(T+3F, T+4F-F) = T+3F; wall = F
        //   trailing zero: local_start = min(T+3F, T+5F-F) = T+3F; wall = F
        //   local_time_delta = 0

        const F: i64 = 90_000;
        const T: i64 = 90_000 * 90_000; // arbitrary nonzero base time

        let mut writer = Writer::new(stream.clone()).unwrap();

        // Recording 1, frames 0–2 (local clock matches media).
        writer
            .write(
                Vec::from(b"f0"),
                recording::Time(T + F),
                0,
                true,
                false,
                VIDEO_SAMPLE_ENTRY_ID,
            )
            .unwrap();
        writer
            .write(
                Vec::from(b"f1"),
                recording::Time(T + 2 * F),
                F,
                false,
                false,
                VIDEO_SAMPLE_ENTRY_ID,
            )
            .unwrap();
        writer
            .write(
                Vec::from(b"f2"),
                recording::Time(T + 3 * F),
                2 * F,
                false,
                false,
                VIDEO_SAMPLE_ENTRY_ID,
            )
            .unwrap();
        // rotate_now: write(f3) flushes f2 into recording 1, then closes it.
        // f3 becomes the first frame of recording 2.
        writer
            .write(
                Vec::from(b"f3"),
                recording::Time(T + 4 * F),
                3 * F,
                true,
                true,
                VIDEO_SAMPLE_ENTRY_ID,
            )
            .unwrap();
        // Recording 2, frame 4.
        writer
            .write(
                Vec::from(b"f4"),
                recording::Time(T + 5 * F),
                4 * F,
                false,
                false,
                VIDEO_SAMPLE_ENTRY_ID,
            )
            .unwrap();

        writer.close("done".to_owned());

        let l = stream.inner.lock();
        let recordings: Vec<_> = l.recent_recordings.iter().collect();
        assert_eq!(recordings.len(), 2);
        let r0 = recordings[0];
        let r1 = recordings[1];

        // run_offset increments per recording within a run.
        assert_eq!(r0.run_offset, 0);
        assert_eq!(r1.run_offset, 1);

        // Recording 1: 3 frames, no clock skew; wall == media.
        assert_eq!(r0.media_duration_90k, 3 * F as i32);
        assert_eq!(r0.wall_duration_90k, 3 * F as i32);
        assert_eq!(r0.local_time_delta, recording::Duration(0));

        // Recording 2's wall-clock start is contiguous with recording 1's wall-clock end.
        assert_eq!(r1.start, recording::Time(T + 3 * F));
        assert_eq!(
            r1.start,
            r0.start + recording::Duration(r0.wall_duration_90k as i64)
        );

        // Recording 2: 1 frame, no clock skew, gets trailing zero from writer.close().
        assert_eq!(r1.media_duration_90k, F as i32);
        assert_eq!(r1.wall_duration_90k, F as i32);
        assert_eq!(r1.local_time_delta, recording::Duration(0));
    }

    /// Tests total wall-clock duration over 4 hours of recordings at 30 fps, rotating every 60 s.
    ///
    /// With a perfect local clock, the sum of all recordings' `wall_duration_90k` should equal
    /// the total elapsed media time (4 hours).
    #[test]
    fn total_wall_duration() {
        testutil::init();
        const VIDEO_SAMPLE_ENTRY_ID: i32 = 0;
        let stream = Stream::new(LockedStream::dummy());

        const FPS: i64 = 30;
        const FRAME_DURATION: i64 = 90_000 / FPS; // 3_000 ticks
        const FRAMES_PER_RECORDING: i64 = 60 * FPS; // 1_800 frames = 60 seconds
        const NUM_RECORDINGS: i64 = 4 * 60; // 240 recordings = 4 hours

        let total_frames = NUM_RECORDINGS * FRAMES_PER_RECORDING;
        // local_time models a wall clock starting at T that advances in lockstep with media.
        // Frame i has pts = i * FRAME_DURATION and arrives at T + (i+1) * FRAME_DURATION.
        // The estimated wall start is local_time - media_duration_including_frame = T (constant).
        const T: i64 = 90_000 * 90_000; // arbitrary nonzero base time
        let local_time = |i: i64| recording::Time(T + (i + 1) * FRAME_DURATION);

        let mut writer = Writer::new(stream.clone()).unwrap();
        let mut pts = 0i64;

        for i in 0..total_frames {
            let is_key = i % FRAMES_PER_RECORDING == 0;
            // rotate_now is true for the first frame of each new recording (except the first),
            // which causes the previous recording to be closed after flushing its last frame.
            let rotate_now = is_key && i > 0;
            writer
                .write(
                    vec![],
                    local_time(i),
                    pts,
                    is_key,
                    rotate_now,
                    VIDEO_SAMPLE_ENTRY_ID,
                )
                .unwrap();
            pts += FRAME_DURATION;
        }
        writer.close("done".to_owned());

        let l = stream.inner.lock();
        let total_wall: i64 = l
            .recent_recordings
            .iter()
            .map(|r| i64::from(r.wall_duration_90k))
            .sum();
        // The last frame in the loop is always closed with duration 0 (trailing zero),
        // so the actual total media duration is one frame short of total_frames * FRAME_DURATION.
        let total_media = (total_frames - 1) * FRAME_DURATION;

        assert_eq!(
            total_wall, total_media,
            "total_wall={total_wall} total_media={total_media}"
        );
    }

    #[test]
    fn write_excessive_jump() {
        testutil::init();
        const VIDEO_SAMPLE_ENTRY_ID: i32 = 0;
        let stream = Stream::new(LockedStream::dummy());

        let mut writer = Writer::new(stream.clone()).unwrap();
        writer
            .write(
                Vec::from(b"frame1"),
                recording::Time(2),
                1000,
                true,
                false,
                VIDEO_SAMPLE_ENTRY_ID,
            )
            .unwrap();

        // TODO:

        // Test excessive jump
        let err = writer
            .write(
                Vec::from(b"frame3"),
                recording::Time(2),
                1000 + i64::from(i32::MAX) + 1,
                true,
                false,
                VIDEO_SAMPLE_ENTRY_ID,
            )
            .unwrap_err();
        assert_eq!(
            err,
            "media duration 6 hours 37 minutes 40 seconds exceeds maximum 5 minutes"
        );
    }
}
