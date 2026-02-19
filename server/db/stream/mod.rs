// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2025 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Video streams and recording metadata.
//!
//! Streams, unlike most database entities, have their own locks. Their latest
//! state, including "recent" recordings/frames, can be accessed independently of the
//! [`create::db::Database`].
//!
//! What should be in this module: fast operations that would be performed under
//! the stream lock.
//!
//! What should not be in this module:
//!
//! * interactions with the directory I/O pool: those tend to be within [`crate::dir`] instead.
//! * interactions with the SQLite connection: those tend to be within [`crate::db`] or
//!   [`crate::lifecycle`] instead.

use std::{cmp, collections::VecDeque, num::NonZeroU64, ops::Range, sync::Arc};

use base::Mutex;

use crate::{
    days, recording, round_up, stream::recent_frames::RecentFrames, CompositeId, RecentFrame,
    RecentRecording, RecordingFlags,
};

pub(crate) mod recent_frames;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StreamType {
    Main,
    Sub,
    Ext,
}

pub const NUM_STREAM_TYPES: usize = 3;

impl StreamType {
    pub fn from_index(i: usize) -> Option<Self> {
        match i {
            0 => Some(StreamType::Main),
            1 => Some(StreamType::Sub),
            2 => Some(StreamType::Ext),
            _ => None,
        }
    }

    pub fn index(self) -> usize {
        match self {
            StreamType::Main => 0,
            StreamType::Sub => 1,
            StreamType::Ext => 2,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StreamType::Main => "main",
            StreamType::Sub => "sub",
            StreamType::Ext => "ext",
        }
    }

    pub fn parse(type_: &str) -> Option<Self> {
        match type_ {
            "main" => Some(StreamType::Main),
            "sub" => Some(StreamType::Sub),
            "ext" => Some(StreamType::Ext),
            _ => None,
        }
    }
}

impl ::std::fmt::Display for StreamType {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> Result<(), ::std::fmt::Error> {
        f.write_str(self.as_str())
    }
}

pub const ALL_STREAM_TYPES: [StreamType; NUM_STREAM_TYPES] =
    [StreamType::Main, StreamType::Sub, StreamType::Ext];

pub struct Stream {
    pub inner: Mutex<LockedStream, 2>,

    /// Notification of a frame, for subscriptions. `Notify::notify_all` is called with `inner`
    /// locked when a new frame is appended.
    pub(crate) recent_frames_notify: tokio::sync::Notify,
}

impl Stream {
    pub(crate) fn new(locked: LockedStream) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(locked),
            recent_frames_notify: tokio::sync::Notify::new(),
        })
    }

    /// Subscribes to frames, starting with a key frame.
    pub fn frames(&self) -> FramesSubscription<'_> {
        FramesSubscription {
            stream: self,
            next: None,
        }
    }
}

pub struct LockedStream {
    pub id: i32,
    pub camera_id: i32,

    /// Invariant: if `sample_file_dir_id_and_pool` is `Some`, the directory id exists in [`LockedDatabase::sample_file_dirs_by_id`].
    pub sample_file_dir: Option<crate::db::SampleFileDir>,
    pub type_: StreamType,
    pub config: crate::json::StreamConfig,

    /// On flush, delete the following recordings (move them to the `garbage` table, to be
    /// collected later). Note they must be the oldest recordings. The later collection involves
    /// the syncer unlinking the files on disk and syncing the directory then enqueueing for
    /// another following flush removal from the `garbage` table.
    pub(crate) to_delete: Vec<crate::db::ListOldestRecordingsRow>,

    pub(crate) open_writer: bool,

    /// The total bytes to delete with the next flush.
    pub bytes_to_delete: i64,
    pub fs_bytes_to_delete: i64,

    /// The next id beyond those ready to flush.
    ///
    /// All "ready to flush" recordings must be fully synced to disk; additionally the
    /// `Flusher` has examined them and decided which old recordings to make room
    /// for them. (Maybe in the future this responsibility will be moved into `flush` itself,
    /// obsoleting this extra state.)
    ///
    /// Invariant:
    /// `committed.cum_recordings <= flush_ready <= writer_state.recording_id <= complete.cum_recordings`.
    pub(crate) flush_ready: i32,

    pub committed: StreamCommitted,
    pub(crate) complete: StreamComplete,

    /// Recently written recordings.
    ///
    /// Recordings are pushed (appended to the back) when started. They are
    /// popped only when *all* of the following conditions are satisfied:
    ///
    /// * from the front
    /// * when unpinned (see `recent_recordings_pinned`)
    /// * when there are no matching frames in `recent_frames`
    /// * when either fully committed or aborted
    pub recent_recordings: VecDeque<RecentRecording>,

    /// Whether `recent_recordings` is currently pinned.
    ///
    /// The `list_recordings_by_*` operations depend on recordings not disappearing in the middle.
    /// Pins should be placed and removed with the database lock held the entire
    /// time; thus a single pin suffices. The stream lock is released and
    /// reacquired between pinning and unpinning.
    pub(crate) recent_recordings_pinned: bool,

    pub recent_frames: RecentFrames,

    pub(crate) writer_state: crate::dir::writer::State,
}

/// Per-stream information matching what is committed to the database; updated on startup and during `LockedRecording::flush`.
///
/// This is separated out both to make it extremely clear which fields track committed vs in-memory state and to group for borrows.
#[derive(Default)]
pub struct StreamCommitted {
    /// The time range of recorded data associated with this stream (minimum start time and maximum
    /// end time). `None` iff there are no recordings for this camera.
    pub range: Option<Range<recording::Time>>,

    /// Mapping of calendar day (in the server's time zone) to a summary of recordings on that day.
    pub days: days::Map<days::StreamValue>,

    /// The total bytes of flushed sample files. This doesn't include disk space wasted in the
    /// last filesystem block allocated to each file ("internal fragmentation").
    pub sample_file_bytes: i64,

    /// The total bytes on the filesystem used by this stream. This slightly more than
    /// `sample_file_bytes` because it includes the wasted space in the last filesystem block.
    pub fs_bytes: i64,

    /// The total duration of undeleted recorded data. This may not be `range.end - range.start`
    /// due to gaps and overlap.
    pub duration: recording::Duration,

    pub cum_recordings: i32,
}

/// The state of the stream including any complete (non-growing) but uncommitted
/// recordings.
///
/// The `cum_*` fields are allowed to backtrack only on fresh "open" (restart).
/// Thus, they must include any recordings that were aborted and pruned before
/// ever being committed, and cannot be restructured from the `StreamCommitted`
/// and `recent_recordings`.
#[derive(Default)]
pub(crate) struct StreamComplete {
    pub cum_recordings: i32,
    pub cum_runs: i32,

    /// The cumulative media duration of all recordings, as of the next
    /// recording to be assigned. This is the same as the database row
    /// `cum_media_duration_90k` on startup.
    pub cum_media_duration: recording::Duration,
}

/// A subscription to frames, starting with the most recent key frame.
pub struct FramesSubscription<'s> {
    stream: &'s Stream,

    /// The next expected `RecentFrame::num`.
    next: Option<NonZeroU64>,
}

pub struct DroppedFramesError {
    pub last: NonZeroU64,
    pub next: NonZeroU64,
}

impl FramesSubscription<'_> {
    /// Resets the position to the latest key frame, returning the frame number.
    pub fn reset(&mut self) -> Option<NonZeroU64> {
        let next = self
            .stream
            .inner
            .lock()
            .recent_frames
            .iter_last_gop()
            .next()
            .map(|(num, _)| num);
        self.next = next;
        next
    }

    pub async fn next(&mut self) -> Result<(NonZeroU64, RecentFrame), DroppedFramesError> {
        let mut first_this_call = true;
        loop {
            let notified = {
                let l = self.stream.inner.lock();
                if let Some(next) = self.next {
                    match l.recent_frames.iter_from_frame_num(next).next() {
                        Some((num, f)) if num == next => {
                            self.next =
                                Some(next.checked_add(1).expect("recent frame num never wraps"));
                            return Ok((num, f.clone()));
                        }
                        Some((num, _)) => {
                            self.next = Some(num);
                            return Err(DroppedFramesError {
                                last: next,
                                next: num,
                            });
                        }
                        None => {
                            assert!(
                                first_this_call,
                                "frame should be available post-notification"
                            );
                        }
                    }
                } else if let Some((num, f)) = l.recent_frames.iter_last_gop().next() {
                    if f.is_key {
                        self.next = Some(num.checked_add(1).expect("recent frame num never wraps"));
                        return Ok((num, f.clone()));
                    }
                };
                self.stream.recent_frames_notify.notified()
            };
            notified.await;
            first_this_call = false;
        }
    }
}

/// A byte position within a stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct BytePos {
    pub recording_id: i32,
    pub byte_pos: u32,
}

impl LockedStream {
    #[cfg(test)]
    pub(crate) fn dummy() -> Self {
        Self::dummy_with_id(1)
    }

    #[cfg(test)]
    pub(crate) fn dummy_with_id(id: i32) -> Self {
        Self {
            id,
            camera_id: 1,
            open_writer: false,
            sample_file_dir: None,
            type_: StreamType::Main,
            config: Default::default(),
            to_delete: Vec::new(),
            bytes_to_delete: 0,
            fs_bytes_to_delete: 0,
            flush_ready: 0,
            committed: Default::default(),
            complete: Default::default(),
            recent_recordings: VecDeque::new(),
            recent_recordings_pinned: false,
            recent_frames: RecentFrames::default(),
            writer_state: Default::default(),
        }
    }

    /// Adds a placeholder for an uncommitted recording, returning a freshly assigned id.
    ///
    /// The caller should write samples and fill the returned `RecordingToInsert` as it goes
    /// (noting that while holding the lock, it should not perform I/O or acquire the database
    /// lock). Then it should sync to permanent storage and call `mark_synced`. The data will
    /// be written to the database on the next `flush`.
    ///
    /// A call to `add_recording` is also a promise that previous recordings (even if not yet
    /// synced and committed) won't change.
    ///
    /// This fills the `id`, `prev_media_duration`, and `prev_runs` fields.
    /// XXX: maybe the caller should use a smaller struct with the fields it's actually expected to fill in.
    pub(crate) fn add_recording(&mut self, mut r: RecentRecording) -> i32 {
        if let Some(back) = self.recent_recordings.back() {
            assert!(!back.flags.contains(RecordingFlags::GROWING));
        }
        debug_assert!(
            recording::Time(0) < r.start && r.start < recording::Time::MAX,
            "start time must be valid; was {:?}",
            r.start
        );
        let id = self.complete.cum_recordings;
        r.id = id;
        r.prev_media_duration = self.complete.cum_media_duration;
        r.prev_runs = self.complete.cum_runs;
        self.recent_recordings.push_back(r);
        id
    }

    /// The currently expected number of bytes to add on the next commit.
    pub(crate) fn fs_bytes_to_add(&self) -> i64 {
        let i = self
            .recent_recordings
            .partition_point(|r| r.id < self.committed.cum_recordings);
        self.recent_recordings
            .iter()
            .skip(i)
            .take_while(|r| r.id < self.flush_ready)
            .map(|r| round_up(i64::from(r.sample_file_bytes)))
            .sum::<i64>()
    }

    pub(crate) fn maybe_prune_recent_recordings(&mut self) {
        if self.recent_recordings_pinned {
            tracing::debug!(
                stream_id = self.id,
                "not pruning {} recordings because pinned",
                self.recent_recordings.len()
            );
            return;
        }
        let front_frame_recording_id = self
            .recent_frames
            .front()
            .map(|f| f.recording_id)
            .unwrap_or(i32::MAX);
        let mut remaining = self.recent_recordings.len();
        while let Some(front) = self.recent_recordings.front_mut() {
            if front.id >= front_frame_recording_id {
                // still relevant to frames.
                tracing::trace!(stream_id = self.id, "pruning stopped with {remaining} remaining because front recording id {} >= front recent frame recording id {}", front.id, front_frame_recording_id);
                return;
            }
            match front.id.cmp(&self.writer_state.recording_id) {
                cmp::Ordering::Less => {
                    if !front.flags.contains(RecordingFlags::DELETED)
                        && front.flags.contains(RecordingFlags::UNCOMMITTED)
                    {
                        tracing::trace!(
                            stream_id = self.id,
                            "pruning stopped with {remaining} remaining because front recording id {} with flags {:?} < writer state recording id {}",
                            front.id,
                            front.flags,
                            self.writer_state.recording_id,
                        );
                        return; // has been synced, waiting for commit.
                    }
                }
                cmp::Ordering::Equal => {
                    if self.writer_state.written == front.sample_file_bytes {
                        tracing::trace!(stream_id = self.id, "pruning stopped with {remaining} remaining because front recording id {} == writer state recording id {}", front.id, self.writer_state.recording_id);
                        return; // being synced.
                    }
                }
                cmp::Ordering::Greater => {}
            }
            self.recent_recordings.pop_front();
            remaining -= 1;
        }
    }

    /// Marks recordings before `end` as deleted.
    ///
    /// This is called when deleting them from the database; typically none of
    /// the range is recent, but it is possible with extreme low retention.
    pub(crate) fn delete_until(&mut self, end: i32) {
        for r in self.recent_recordings.iter_mut().take_while(|r| r.id < end) {
            assert!(
                !r.flags.contains(RecordingFlags::UNCOMMITTED),
                "delete_until({end}): recording {id} has surprising flags {flags:?}",
                id = CompositeId::new(self.id, r.id),
                flags = r.flags,
            );
            r.flags |= RecordingFlags::DELETED;
        }
    }

    /// Returns a days map including uncommitted recordings.
    pub fn days(&self) -> days::Map<days::StreamValue> {
        let mut days = self.committed.days.clone();
        let i = self
            .recent_recordings
            .partition_point(|r| r.id < self.committed.cum_recordings);
        for r in self.recent_recordings.iter().skip(i) {
            days.adjust(
                r.start..r.start + recording::Duration(i64::from(r.wall_duration_90k)),
                1,
            );
        }
        days
    }
}

impl StreamCommitted {
    /// Adds a single fully committed recording with the given properties to the in-memory state.
    pub(crate) fn add_recording(&mut self, r: Range<recording::Time>, sample_file_bytes: u32) {
        self.range = Some(match self.range {
            Some(ref e) => cmp::min(e.start, r.start)..cmp::max(e.end, r.end),
            None => r.start..r.end,
        });
        self.duration += r.end - r.start;
        self.sample_file_bytes += i64::from(sample_file_bytes);
        self.fs_bytes += round_up(i64::from(sample_file_bytes));
        self.days.adjust(r, 1);
    }
}
