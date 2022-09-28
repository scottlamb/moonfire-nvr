// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Building and reading recordings via understanding of their sample indexes.

use crate::coding::{append_varint32, decode_varint32, unzigzag32, zigzag32};
use crate::db;
use failure::{bail, Error};
use log::trace;
use std::convert::TryFrom;
use std::ops::Range;

pub use base::time::TIME_UNITS_PER_SEC;

pub const DESIRED_RECORDING_WALL_DURATION: i64 = 60 * TIME_UNITS_PER_SEC;
pub const MAX_RECORDING_WALL_DURATION: i64 = 5 * 60 * TIME_UNITS_PER_SEC;

pub use base::time::Duration;
pub use base::time::Time;

/// Converts from a wall time offset within a recording to a media time offset or vice versa.
pub fn rescale(from_off_90k: i32, from_duration_90k: i32, to_duration_90k: i32) -> i32 {
    debug_assert!(
        from_off_90k <= from_duration_90k,
        "from_off_90k={} from_duration_90k={} to_duration_90k={}",
        from_off_90k,
        from_duration_90k,
        to_duration_90k
    );
    if from_duration_90k == 0 {
        return 0; // avoid a divide by zero.
    }

    // The intermediate values here may overflow i32, so use an i64 instead. The max wall
    // time is [`MAX_RECORDING_WALL_DURATION`]; the max media duration should be
    // roughly the same (design limit of 500 ppm correction). The final result should fit
    // within i32.
    i32::try_from(
        i64::from(from_off_90k) * i64::from(to_duration_90k) / i64::from(from_duration_90k),
    )
    .map_err(|_| {
        format!(
            "rescale overflow: {} * {} / {} > i32::max_value()",
            from_off_90k, to_duration_90k, from_duration_90k
        )
    })
    .unwrap()
}

/// An iterator through a sample index (as described in `design/recording.md`).
/// Initially invalid; call `next()` before each read.
#[derive(Clone, Copy, Debug, Default)]
pub struct SampleIndexIterator {
    /// The index byte position of the next sample to read (low 31 bits) and if the current
    /// same is a key frame (high bit).
    i_and_is_key: u32,

    /// The starting data byte position of this sample within the segment.
    pub pos: i32,

    /// The starting time of this sample within the segment (in 90 kHz units).
    pub start_90k: i32,

    /// The duration of this sample (in 90 kHz units).
    pub duration_90k: i32,

    /// The byte length of this frame.
    pub bytes: i32,

    /// The byte length of the last frame of the "other" type: if this one is key, the last
    /// non-key; if this one is non-key, the last key.
    bytes_other: i32,
}

impl SampleIndexIterator {
    pub fn next(&mut self, data: &[u8]) -> Result<bool, Error> {
        self.pos += self.bytes;
        self.start_90k += self.duration_90k;
        let i = (self.i_and_is_key & 0x7FFF_FFFF) as usize;
        if i == data.len() {
            return Ok(false);
        }
        let (raw1, i1) = match decode_varint32(data, i) {
            Ok(tuple) => tuple,
            Err(()) => bail!("bad varint 1 at offset {}", i),
        };
        let (raw2, i2) = match decode_varint32(data, i1) {
            Ok(tuple) => tuple,
            Err(()) => bail!("bad varint 2 at offset {}", i1),
        };
        let duration_90k_delta = unzigzag32(raw1 >> 1);
        self.duration_90k += duration_90k_delta;
        if self.duration_90k < 0 {
            bail!(
                "negative duration {} after applying delta {}",
                self.duration_90k,
                duration_90k_delta
            );
        }
        if self.duration_90k == 0 && data.len() > i2 {
            bail!(
                "zero duration only allowed at end; have {} bytes left",
                data.len() - i2
            );
        }
        let (prev_bytes_key, prev_bytes_nonkey) = match self.is_key() {
            true => (self.bytes, self.bytes_other),
            false => (self.bytes_other, self.bytes),
        };
        self.i_and_is_key = (i2 as u32) | (((raw1 & 1) as u32) << 31);
        let bytes_delta = unzigzag32(raw2);
        if self.is_key() {
            self.bytes = prev_bytes_key + bytes_delta;
            self.bytes_other = prev_bytes_nonkey;
        } else {
            self.bytes = prev_bytes_nonkey + bytes_delta;
            self.bytes_other = prev_bytes_key;
        }
        if self.bytes <= 0 {
            bail!(
                "non-positive bytes {} after applying delta {} to key={} frame at ts {}",
                self.bytes,
                bytes_delta,
                self.is_key(),
                self.start_90k
            );
        }
        Ok(true)
    }

    #[inline]
    pub fn is_key(&self) -> bool {
        (self.i_and_is_key & 0x8000_0000) != 0
    }
}

/// An encoder for a sample index (as described in `design/recording.md`).
#[derive(Debug, Default)]
pub struct SampleIndexEncoder {
    prev_duration_90k: i32,
    prev_bytes_key: i32,
    prev_bytes_nonkey: i32,
}

impl SampleIndexEncoder {
    pub fn add_sample(
        &mut self,
        duration_90k: i32,
        bytes: i32,
        is_key: bool,
        r: &mut db::RecordingToInsert,
    ) {
        let duration_delta = duration_90k - self.prev_duration_90k;
        self.prev_duration_90k = duration_90k;
        r.media_duration_90k += duration_90k;
        r.sample_file_bytes += bytes;
        r.video_samples += 1;
        let bytes_delta = bytes
            - if is_key {
                let prev = self.prev_bytes_key;
                r.video_sync_samples += 1;
                self.prev_bytes_key = bytes;
                prev
            } else {
                let prev = self.prev_bytes_nonkey;
                self.prev_bytes_nonkey = bytes;
                prev
            };
        append_varint32(
            (zigzag32(duration_delta) << 1) | (is_key as u32),
            &mut r.video_index,
        );
        append_varint32(zigzag32(bytes_delta), &mut r.video_index);
    }
}

/// A segment represents a view of some or all of a single recording.
/// This struct is not specific to a container format; for `.mp4`s, it's wrapped in a
/// `moonfire_nvr::mp4::Segment`. Other container/transport formats could be
/// supported in a similar manner.
#[derive(Debug)]
pub struct Segment {
    pub id: db::CompositeId,
    pub open_id: u32,

    /// An iterator positioned at the beginning of the segment, or `None`. Most segments are
    /// positioned at the beginning of the recording, so this is an optional box to shrink a long
    /// of segments. `None` is equivalent to `SampleIndexIterator::default()`.
    begin: Option<Box<SampleIndexIterator>>,
    pub file_end: i32,

    pub frames: u16,
    pub key_frames: u16,
    video_sample_entry_id_and_trailing_zero: i32,
}

impl Segment {
    /// Creates a segment.
    ///
    /// `desired_media_range_90k` represents the desired range of the segment relative to the start
    /// of the recording, in media time units.
    ///
    /// The actual range will start at the most recent acceptable frame's start at or before the
    /// desired start time. If `start_at_key` is true, only key frames are acceptable; otherwise
    /// any frame is. The caller is responsible for skipping over the undesired prefix, perhaps
    /// with an edit list in the case of a `.mp4`.
    ///
    /// The actual range will end at the first frame after the desired range (unless the desired
    /// range extends beyond the recording). Likewise, the caller is responsible for trimming the
    /// final frame's duration if desired.
    pub fn new(
        db: &db::LockedDatabase,
        recording: &db::ListRecordingsRow,
        desired_media_range_90k: Range<i32>,
        start_at_key: bool,
    ) -> Result<Segment, Error> {
        let mut self_ = Segment {
            id: recording.id,
            open_id: recording.open_id,
            begin: None,
            file_end: recording.sample_file_bytes,
            frames: recording.video_samples as u16,
            key_frames: recording.video_sync_samples as u16,
            video_sample_entry_id_and_trailing_zero: recording.video_sample_entry_id
                | ((((recording.flags & db::RecordingFlags::TrailingZero as i32) != 0) as i32)
                    << 31),
        };

        #[allow(clippy::suspicious_operation_groupings)]
        if desired_media_range_90k.start > desired_media_range_90k.end
            || desired_media_range_90k.end > recording.media_duration_90k
        {
            bail!(
                "desired media range [{}, {}) invalid for recording of length {}",
                desired_media_range_90k.start,
                desired_media_range_90k.end,
                recording.media_duration_90k
            );
        }

        if desired_media_range_90k.start == 0
            && desired_media_range_90k.end == recording.media_duration_90k
        {
            // Fast path. Existing entry is fine.
            trace!(
                "recording::Segment::new fast path, recording={:#?}",
                recording
            );
            return Ok(self_);
        }

        // Slow path. Need to iterate through the index.
        trace!(
            "recording::Segment::new slow path, desired_media_range_90k={:?}, recording={:#?}",
            desired_media_range_90k,
            recording
        );
        db.with_recording_playback(self_.id, &mut |playback| {
            let mut begin = Box::new(SampleIndexIterator::default());
            let data = &playback.video_index;
            let mut it = SampleIndexIterator::default();
            if !it.next(data)? {
                bail!("no index");
            }
            if !it.is_key() {
                bail!("not key frame");
            }

            // Stop when hitting a frame with this start time.
            // Going until the end of the recording is special-cased because there can be a trailing
            // frame of zero duration. It's unclear exactly how this should be handled, but let's
            // include it for consistency with the fast path. It'd be bizarre to have it included or
            // not based on desired_media_range_90k.start.
            let end_90k = if desired_media_range_90k.end == recording.media_duration_90k {
                i32::max_value()
            } else {
                desired_media_range_90k.end
            };

            loop {
                if it.start_90k <= desired_media_range_90k.start && (!start_at_key || it.is_key()) {
                    // new start candidate.
                    *begin = it;
                    self_.frames = 0;
                    self_.key_frames = 0;
                }
                if it.start_90k >= end_90k && self_.frames > 0 {
                    break;
                }
                self_.frames += 1;
                self_.key_frames += it.is_key() as u16;
                if !it.next(data)? {
                    break;
                }
            }
            self_.begin = Some(begin);
            self_.file_end = it.pos;
            self_.video_sample_entry_id_and_trailing_zero =
                recording.video_sample_entry_id | (((it.duration_90k == 0) as i32) << 31);
            Ok(())
        })?;
        Ok(self_)
    }

    pub fn video_sample_entry_id(&self) -> i32 {
        self.video_sample_entry_id_and_trailing_zero & 0x7FFFFFFF
    }

    pub fn have_trailing_zero(&self) -> bool {
        self.video_sample_entry_id_and_trailing_zero < 0
    }

    /// Returns the byte range within the sample file of data associated with this segment.
    pub fn sample_file_range(&self) -> Range<u64> {
        self.begin.as_ref().map(|b| b.pos as u64).unwrap_or(0)..self.file_end as u64
    }

    /// Returns the actual media start time. As described in `new`, this can be less than the
    /// desired media start time if there is no key frame at the right position.
    pub fn actual_start_90k(&self) -> i32 {
        self.begin.as_ref().map(|b| b.start_90k).unwrap_or(0)
    }

    /// Iterates through each frame in the segment.
    /// Must be called without the database lock held; retrieves video index from the cache.
    pub fn foreach<F>(&self, playback: &db::RecordingPlayback, mut f: F) -> Result<(), Error>
    where
        F: FnMut(&SampleIndexIterator) -> Result<(), Error>,
    {
        trace!(
            "foreach on recording {}: {} frames, actual_start_90k: {}",
            self.id,
            self.frames,
            self.actual_start_90k()
        );
        let data = &playback.video_index;
        let mut it = match self.begin {
            Some(ref b) => **b,
            None => {
                let mut it = SampleIndexIterator::default();
                if !it.next(data)? {
                    bail!("recording {} has no frames", self.id);
                }
                if !it.is_key() {
                    bail!("recording {} doesn't start with key frame", self.id);
                }
                it
            }
        };
        let mut have_frame = true;
        let mut key_frame = 0;

        for i in 0..self.frames {
            if !have_frame {
                bail!(
                    "recording {}: expected {} frames, found only {}",
                    self.id,
                    self.frames,
                    i + 1
                );
            }
            if it.is_key() {
                key_frame += 1;
                if key_frame > self.key_frames {
                    bail!(
                        "recording {}: more than expected {} key frames",
                        self.id,
                        self.key_frames
                    );
                }
            }

            // Note: this inner loop avoids ? for performance. Don't change these lines without
            // reading https://github.com/rust-lang/rust/issues/37939 and running
            // mp4::bench::build_index.
            #[allow(clippy::question_mark)]
            if let Err(e) = f(&it) {
                return Err(e);
            }
            have_frame = match it.next(data) {
                Err(e) => return Err(e),
                Ok(hf) => hf,
            };
        }
        if key_frame < self.key_frames {
            bail!(
                "recording {}: expected {} key frames, found only {}",
                self.id,
                self.key_frames,
                key_frame
            );
        }
        Ok(())
    }

    /// Returns true if this starts with a non-key frame.
    pub fn starts_with_nonkey(&self) -> bool {
        match self.begin {
            Some(ref b) => !b.is_key(),

            // Fast-path case, in which this holds an entire recording. They always start with a
            // key frame.
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, TestDb};
    use base::clock::RealClocks;

    /// Tests encoding the example from design/schema.md.
    #[test]
    fn test_encode_example() {
        testutil::init();
        let mut r = db::RecordingToInsert::default();
        let mut e = SampleIndexEncoder::default();
        e.add_sample(10, 1000, true, &mut r);
        e.add_sample(9, 10, false, &mut r);
        e.add_sample(11, 15, false, &mut r);
        e.add_sample(10, 12, false, &mut r);
        e.add_sample(10, 1050, true, &mut r);
        assert_eq!(
            r.video_index,
            b"\x29\xd0\x0f\x02\x14\x08\x0a\x02\x05\x01\x64"
        );
        assert_eq!(10 + 9 + 11 + 10 + 10, r.media_duration_90k);
        assert_eq!(5, r.video_samples);
        assert_eq!(2, r.video_sync_samples);
    }

    /// Tests a round trip from `SampleIndexEncoder` to `SampleIndexIterator`.
    #[test]
    fn test_round_trip() {
        testutil::init();
        #[derive(Debug, PartialEq, Eq)]
        struct Sample {
            duration_90k: i32,
            bytes: i32,
            is_key: bool,
        }
        #[rustfmt::skip]
        let samples = [
            Sample { duration_90k: 10, bytes: 30000, is_key: true,  },
            Sample { duration_90k:  9, bytes:  1000, is_key: false, },
            Sample { duration_90k: 11, bytes:  1100, is_key: false, },
            Sample { duration_90k: 18, bytes: 31000, is_key: true,  },
            Sample { duration_90k:  0, bytes:  1000, is_key: false, },
        ];
        let mut r = db::RecordingToInsert::default();
        let mut e = SampleIndexEncoder::default();
        for sample in &samples {
            e.add_sample(sample.duration_90k, sample.bytes, sample.is_key, &mut r);
        }
        let mut it = SampleIndexIterator::default();
        for sample in &samples {
            assert!(it.next(&r.video_index).unwrap());
            assert_eq!(
                sample,
                &Sample {
                    duration_90k: it.duration_90k,
                    bytes: it.bytes,
                    is_key: it.is_key()
                }
            );
        }
        assert!(!it.next(&r.video_index).unwrap());
    }

    /// Tests that `SampleIndexIterator` spots several classes of errors.
    /// TODO: test and fix overflow cases.
    #[test]
    fn test_iterator_errors() {
        testutil::init();
        struct Test {
            encoded: &'static [u8],
            err: &'static str,
        }
        let tests = [
            Test {
                encoded: b"\x80",
                err: "bad varint 1 at offset 0",
            },
            Test {
                encoded: b"\x00\x80",
                err: "bad varint 2 at offset 1",
            },
            Test {
                encoded: b"\x00\x02\x00\x00",
                err: "zero duration only allowed at end; have 2 bytes left",
            },
            Test {
                encoded: b"\x02\x02",
                err: "negative duration -1 after applying delta -1",
            },
            Test {
                encoded: b"\x04\x00",
                err: "non-positive bytes 0 after applying delta 0 to key=false frame at ts 0",
            },
        ];
        for test in &tests {
            let mut it = SampleIndexIterator::default();
            assert_eq!(it.next(test.encoded).unwrap_err().to_string(), test.err);
        }
    }

    fn get_frames<F, T>(db: &db::Database, segment: &Segment, f: F) -> Vec<T>
    where
        F: Fn(&SampleIndexIterator) -> T,
    {
        let mut v = Vec::new();
        db.lock()
            .with_recording_playback(segment.id, &mut |playback| {
                segment.foreach(playback, |it| {
                    v.push(f(it));
                    Ok(())
                })
            })
            .unwrap();
        v
    }

    /// Tests that a `Segment` correctly can clip at the beginning and end.
    /// This is a simpler case; all sync samples means we can start on any frame.
    #[test]
    fn test_segment_clipping_with_all_sync() {
        testutil::init();
        let mut r = db::RecordingToInsert::default();
        let mut encoder = SampleIndexEncoder::default();
        for i in 1..6 {
            let duration_90k = 2 * i;
            let bytes = 3 * i;
            encoder.add_sample(duration_90k, bytes, true, &mut r);
        }
        let db = TestDb::new(RealClocks {});
        let row = db.insert_recording_from_encoder(r);
        // Time range [2, 2 + 4 + 6 + 8) means the 2nd, 3rd, 4th samples should be
        // included.
        let segment = Segment::new(&db.db.lock(), &row, 2..2 + 4 + 6 + 8, true).unwrap();
        assert_eq!(
            &get_frames(&db.db, &segment, |it| it.duration_90k),
            &[4, 6, 8]
        );
    }

    /// Half sync frames means starting from the last sync frame <= desired point.
    #[test]
    fn test_segment_clipping_with_half_sync() {
        testutil::init();
        let mut r = db::RecordingToInsert::default();
        let mut encoder = SampleIndexEncoder::default();
        for i in 1..6 {
            let duration_90k = 2 * i;
            let bytes = 3 * i;
            encoder.add_sample(duration_90k, bytes, (i % 2) == 1, &mut r);
        }
        let db = TestDb::new(RealClocks {});
        let row = db.insert_recording_from_encoder(r);
        // Time range [2 + 4 + 6, 2 + 4 + 6 + 8) means the 4th sample should be included.
        // The 3rd also gets pulled in because it is a sync frame and the 4th is not.
        let segment = Segment::new(&db.db.lock(), &row, 2 + 4 + 6..2 + 4 + 6 + 8, true).unwrap();
        assert_eq!(&get_frames(&db.db, &segment, |it| it.duration_90k), &[6, 8]);
    }

    #[test]
    fn test_segment_clipping_with_trailing_zero() {
        testutil::init();
        let mut r = db::RecordingToInsert::default();
        let mut encoder = SampleIndexEncoder::default();
        encoder.add_sample(1, 1, true, &mut r);
        encoder.add_sample(1, 2, true, &mut r);
        encoder.add_sample(0, 3, true, &mut r);
        let db = TestDb::new(RealClocks {});
        let row = db.insert_recording_from_encoder(r);
        let segment = Segment::new(&db.db.lock(), &row, 1..2, true).unwrap();
        assert_eq!(&get_frames(&db.db, &segment, |it| it.bytes), &[2, 3]);
    }

    /// Even if the desired duration is 0, there should still be a frame.
    #[test]
    fn test_segment_zero_desired_duration() {
        testutil::init();
        let mut r = db::RecordingToInsert::default();
        let mut encoder = SampleIndexEncoder::default();
        encoder.add_sample(1, 1, true, &mut r);
        let db = TestDb::new(RealClocks {});
        let row = db.insert_recording_from_encoder(r);
        let segment = Segment::new(&db.db.lock(), &row, 0..0, true).unwrap();
        assert_eq!(&get_frames(&db.db, &segment, |it| it.bytes), &[1]);
    }

    /// Test a `Segment` which uses the whole recording.
    /// This takes a fast path which skips scanning the index in `new()`.
    #[test]
    fn test_segment_fast_path() {
        testutil::init();
        let mut r = db::RecordingToInsert::default();
        let mut encoder = SampleIndexEncoder::default();
        for i in 1..6 {
            let duration_90k = 2 * i;
            let bytes = 3 * i;
            encoder.add_sample(duration_90k, bytes, (i % 2) == 1, &mut r);
        }
        let db = TestDb::new(RealClocks {});
        let row = db.insert_recording_from_encoder(r);
        let segment = Segment::new(&db.db.lock(), &row, 0..2 + 4 + 6 + 8 + 10, true).unwrap();
        assert_eq!(
            &get_frames(&db.db, &segment, |it| it.duration_90k),
            &[2, 4, 6, 8, 10]
        );
    }

    #[test]
    fn test_segment_fast_path_with_trailing_zero() {
        testutil::init();
        let mut r = db::RecordingToInsert::default();
        let mut encoder = SampleIndexEncoder::default();
        encoder.add_sample(1, 1, true, &mut r);
        encoder.add_sample(1, 2, true, &mut r);
        encoder.add_sample(0, 3, true, &mut r);
        let db = TestDb::new(RealClocks {});
        let row = db.insert_recording_from_encoder(r);
        let segment = Segment::new(&db.db.lock(), &row, 0..2, true).unwrap();
        assert_eq!(&get_frames(&db.db, &segment, |it| it.bytes), &[1, 2, 3]);
    }

    // TODO: test segment error cases involving mismatch between row frames/key_frames and index.
}

#[cfg(all(test, feature = "nightly"))]
mod bench {
    extern crate test;

    use super::*;

    /// Benchmarks the decoder, which is performance-critical for .mp4 serving.
    #[bench]
    fn bench_decoder(b: &mut test::Bencher) {
        let data = include_bytes!("testdata/video_sample_index.bin");
        b.bytes = data.len() as u64;
        b.iter(|| {
            let mut it = SampleIndexIterator::default();
            while it.next(data).unwrap() {}
            assert_eq!(30104460, it.pos);
            assert_eq!(5399985, it.start_90k);
        });
    }
}
