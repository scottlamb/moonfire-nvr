// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2025 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use std::{
    collections::VecDeque,
    num::{NonZero, NonZeroU64},
    ops::Range,
    sync::Arc,
};

use crate::stream::BytePos;

pub const BYTES_FOR_WRITER: usize = 64 << 20; // 64 MiB

/// A recent video frame as in [`RecentFrames`].
#[derive(Clone)]
pub struct RecentFrame {
    /// The recording id, which should exist within [`crate::db::LockedStream::recent_recordings`].
    pub recording_id: i32,

    /// If this is a key (IDR) frame.
    pub is_key: bool,

    /// The pts, relative to the start of the recording, of the start and end of this frame,
    /// in 90kHz units.
    pub media_off_90k: Range<i32>,

    pub sample: Arc<Vec<u8>>,

    /// The start position of the sample within the recording file.
    /// (The end is `sample_start + sample.len()`.)
    pub sample_start: u32,
}

impl RecentFrame {
    /// The starting byte position of this frame.
    pub fn start(&self) -> super::BytePos {
        super::BytePos {
            recording_id: self.recording_id,
            byte_pos: self.sample_start,
        }
    }

    /// The ending byte position of this frame.
    pub fn end(&self) -> super::BytePos {
        super::BytePos {
            recording_id: self.recording_id,
            byte_pos: self.sample_start + self.sample.len() as u32,
        }
    }
}

/// Custom `Debug` impl that skips the verbose `sample` field.
impl std::fmt::Debug for RecentFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecentFrame")
            .field("recording_id", &self.recording_id)
            .field("is_key", &self.is_key)
            .field("media_off_90k", &self.media_off_90k)
            .field(
                "sample_pos",
                &(self.sample_start..self.sample_start + self.sample.len() as u32),
            )
            .finish_non_exhaustive()
    }
}

/// The most recent frames received on a stream, stored within [`crate::db::LockedStream::recent_frames`].
///
/// Provides limited accessors which preserve internal invariants. Frames are immutable once added.
///
/// Mutations (`push_back` and `prune_front`) run in amortized O(1) time.
///
/// Search operations are O(log n).
#[derive(Default)]
pub struct RecentFrames {
    frames: VecDeque<RecentFrame>,

    /// The number of frames discarded from the front of the queue.
    discarded: u64,
    last_gop_i: u32,
    last_gop_sample_bytes: usize,
    total_sample_bytes: usize,
}

impl std::fmt::Debug for RecentFrames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(
                self.frames
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (i as u64 + self.discarded + 1, f)),
            )
            .finish()
    }
}

/// An iterator over `(frame_num: NonZeroU64, frame: &RecentFrame)`.
///
/// The frame number can be used to count dropped frames; it starts from 1 on
/// program start and increments by 1 for each frame.
pub struct Iter<'a> {
    /// `next_frame_num` must be correct if the iterator is not fused.
    next_frame_num: NonZeroU64,
    iter: std::collections::vec_deque::Iter<'a, RecentFrame>,
}

impl Iter<'_> {}

impl<'a> Iterator for Iter<'a> {
    type Item = (NonZeroU64, &'a RecentFrame);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let r = self.iter.next().map(|frame| (self.next_frame_num, frame));
        self.next_frame_num = self
            .next_frame_num
            .checked_add(1)
            .expect("next_frame_num never overflows");
        r
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl std::iter::FusedIterator for Iter<'_> {}
impl ExactSizeIterator for Iter<'_> {}

impl RecentFrames {
    /// Pushes a new frame.
    ///
    /// The frame must be a valid successor of the previous frame, if any.
    /// The caller is encouraged to call `prune_front` immediately afterward.
    pub fn push_back(&mut self, frame: RecentFrame) {
        #[cfg(debug_assertions)]
        if let Some(prev) = self.frames.back() {
            assert!(
                (prev.recording_id == frame.recording_id
                    && prev.sample_start + prev.sample.len() as u32 == frame.sample_start
                    && prev.media_off_90k.end == frame.media_off_90k.start)
                    || (Some(prev.recording_id) == frame.recording_id.checked_sub(1)
                        && frame.sample_start == 0
                        && frame.media_off_90k.start == 0),
                "can't follow {prev:#?} with {frame:#?}"
            );
        }
        let i = u32::try_from(self.frames.len()).expect("recent_frames indices fit in u32");
        if frame.is_key {
            self.last_gop_i = i;
            self.last_gop_sample_bytes = 0;
        }
        self.last_gop_sample_bytes += frame.sample.len();
        self.total_sample_bytes += frame.sample.len();
        self.frames.push_back(frame);
    }

    /// Returns an iterator over frames[i..].
    fn iter_from_i(&self, i: usize) -> Iter<'_> {
        let mut iter = self.frames.iter();
        if i > 0 {
            iter.nth(i - 1); // if this returns `None`, the iterator is fused.
        }
        Iter {
            next_frame_num: NonZeroU64::MIN
                .checked_add(i as u64 + self.discarded)
                .expect("next_frame_num never overflows"),
            iter,
        }
    }

    /// Iteration from the last GOP. Will start with a key frame unless no key
    /// frame has ever been added.
    pub fn iter_last_gop(&self) -> Iter<'_> {
        self.iter_from_i(self.last_gop_i as usize)
    }

    /// Iterates from the first frame that does not end before the given byte.
    pub fn iter_from_byte_pos(&self, pos: BytePos) -> Iter<'_> {
        let i = self.frames.partition_point(|f| f.end() <= pos);
        self.iter_from_i(i)
    }

    /// Iterates from the first frame >= `num`.
    pub fn iter_from_frame_num(&self, num: NonZero<u64>) -> Iter<'_> {
        assert!(num.get() <= self.discarded + (self.frames.len() as u64) + 1);
        self.iter_from_i(num.get().saturating_sub(self.discarded + 1) as usize)
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Prunes unnecessary frames.
    ///
    /// Frames are considered necessary if they satisfy either of the following conditions:
    ///
    /// * within the full last GOP, to allow rapidly starting a live stream.
    /// * fully within the last `BYTES_FOR_WRITER` and not fully written.
    ///
    /// TODO: consider refining this—time-based component instead for the writer?
    pub fn prune_front(&mut self, writer_pos: BytePos) {
        let mut prune_count = 0;
        for f in self.frames.iter().take(self.last_gop_i as usize) {
            if self.total_sample_bytes <= BYTES_FOR_WRITER && f.end() > writer_pos {
                break;
            }
            self.total_sample_bytes -= f.sample.len();
            prune_count += 1;
        }
        self.frames.drain(..prune_count);
        self.last_gop_i -= prune_count as u32;
        self.discarded += prune_count as u64;
    }

    pub fn front(&self) -> Option<&RecentFrame> {
        self.frames.front()
    }

    pub fn bytes(&self) -> usize {
        self.total_sample_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FrameGen {
        recording_id: i32,
        next_sample_start: u32,
        next_pts_90k: i32,
    }

    impl FrameGen {
        fn new() -> Self {
            Self {
                recording_id: 1,
                next_sample_start: 0,
                next_pts_90k: 0,
            }
        }

        fn next(&mut self, is_key: bool, size: usize) -> RecentFrame {
            const DURATION: i32 = 1000;
            let frame = RecentFrame {
                recording_id: self.recording_id,
                is_key,
                media_off_90k: self.next_pts_90k..(self.next_pts_90k + DURATION),
                sample: Arc::new(vec![0; size]),
                sample_start: self.next_sample_start,
            };
            self.next_sample_start += size as u32;
            self.next_pts_90k += DURATION;
            frame
        }
    }

    #[test]
    fn test_prune_written() {
        let mut rf = RecentFrames::default();
        let mut gen = FrameGen::new();
        // GOP 1: frames 1(K), 2, 3.
        rf.push_back(gen.next(true, 100));
        rf.push_back(gen.next(false, 100));
        rf.push_back(gen.next(false, 100));

        // GOP 2: frames 4(K), 5.
        rf.push_back(gen.next(true, 100));
        rf.push_back(gen.next(false, 100));

        // Nothing pruned yet. Writer at 0.
        rf.prune_front(BytePos {
            recording_id: 1,
            byte_pos: 0,
        });
        assert_eq!(rf.frames.len(), 5);
        assert_eq!(rf.iter_from_i(0).next().unwrap().0.get(), 1);

        // Writer partially wrote frame 1.
        // Frame 1: start=0, len=100 -> end=100.
        rf.prune_front(BytePos {
            recording_id: 1,
            byte_pos: 50,
        });
        assert_eq!(rf.frames.len(), 5);
        assert_eq!(rf.iter_from_i(0).next().unwrap().0.get(), 1);

        // Writer wrote frame 1.
        rf.prune_front(BytePos {
            recording_id: 1,
            byte_pos: 100,
        });
        assert_eq!(rf.frames.len(), 4);
        assert_eq!(rf.iter_from_i(0).next().unwrap().0.get(), 2);

        // Writer partially wrote frame 2.
        // Frame 2: start=100, len=100 -> end=200.
        rf.prune_front(BytePos {
            recording_id: 1,
            byte_pos: 150,
        });
        assert_eq!(rf.frames.len(), 4);
        assert_eq!(rf.iter_from_i(0).next().unwrap().0.get(), 2);

        // Writer wrote frame 2.
        rf.prune_front(BytePos {
            recording_id: 1,
            byte_pos: 200,
        });
        assert_eq!(rf.frames.len(), 3);
        assert_eq!(rf.iter_from_i(0).next().unwrap().0.get(), 3);
    }

    #[test]
    fn test_prune_memory_limit_with_gops() {
        let mut rf = RecentFrames::default();
        let mut gen = FrameGen::new();
        let frame_size = 1024 * 1024; // 1 MiB
                                      // We can fit 64 frames.

        // Start GOP 1.
        rf.push_back(gen.next(true, frame_size));

        // Add many frames, starting new GOPs frequently.
        // This ensures `last_gop_i` moves forward, making previous GOPs candidates for pruning.

        for _ in 0..200 {
            rf.push_back(gen.next(true, frame_size));
            rf.prune_front(BytePos {
                recording_id: 1,
                byte_pos: 0,
            });
        }

        // We pushed 201 frames total (1 + 200).
        // Each is a keyframe (new GOP).
        // So `last_gop_i` is always the last added frame.
        // The frames before it are candidates for pruning.

        // we would think we have less memory usage than we do, and store more frames.
        // For example, if we decremented for the boundary frame that stopped the loop,
        // `total_sample_bytes` would be lower than reality, allowing extra frames to accumulate.

        let len = rf.frames.len();
        assert_eq!(len, 64, "len was {}, expected 64", len);
    }

    #[test]
    fn test_iter_last_gop() {
        let mut rf = RecentFrames::default();
        let mut gen = FrameGen::new();
        // GOP 1
        rf.push_back(gen.next(true, 100));
        rf.push_back(gen.next(false, 100));
        // GOP 2
        rf.push_back(gen.next(true, 100));
        rf.push_back(gen.next(false, 100));

        let frames: Vec<_> = rf.iter_last_gop().collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].0.get(), 3);
        assert_eq!(frames[1].0.get(), 4);
    }

    #[test]
    fn test_iter_from_byte_pos() {
        let mut rf = RecentFrames::default();
        let mut gen = FrameGen::new();
        rf.push_back(gen.next(true, 100)); // 0..100
        rf.push_back(gen.next(false, 100)); // 100..200
        rf.push_back(gen.next(false, 100)); // 200..300

        // Before frame 1 end
        let frames: Vec<_> = rf
            .iter_from_byte_pos(BytePos {
                recording_id: 1,
                byte_pos: 50,
            })
            .collect();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].0.get(), 1);

        // At frame 1 end
        let frames: Vec<_> = rf
            .iter_from_byte_pos(BytePos {
                recording_id: 1,
                byte_pos: 100,
            })
            .collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].0.get(), 2);

        // Middle of frame 2
        let frames: Vec<_> = rf
            .iter_from_byte_pos(BytePos {
                recording_id: 1,
                byte_pos: 150,
            })
            .collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].0.get(), 2);

        // After all frames
        let frames: Vec<_> = rf
            .iter_from_byte_pos(BytePos {
                recording_id: 1,
                byte_pos: 3000,
            })
            .collect();
        assert_eq!(frames.len(), 0);
    }

    #[test]
    fn test_iter_from_frame_num() {
        let mut rf = RecentFrames::default();
        let mut gen = FrameGen::new();
        rf.push_back(gen.next(true, 100)); // 1
        rf.push_back(gen.next(false, 100)); // 2

        let frames: Vec<_> = rf
            .iter_from_frame_num(NonZeroU64::new(1).unwrap())
            .collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].0.get(), 1);

        let frames: Vec<_> = rf
            .iter_from_frame_num(NonZeroU64::new(2).unwrap())
            .collect();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0.get(), 2);
    }
}
