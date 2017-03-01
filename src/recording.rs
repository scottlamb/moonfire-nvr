// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2016 Scott Lamb <slamb@slamb.org>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In addition, as a special exception, the copyright holders give
// permission to link the code of portions of this program with the
// OpenSSL library under certain conditions as described in each
// individual source file, and distribute linked combinations including
// the two.
//
// You must obey the GNU General Public License in all respects for all
// of the code used other than OpenSSL. If you modify file(s) with this
// exception, you may extend this exception to your version of the
// file(s), but you are not obligated to do so. If you do not wish to do
// so, delete this exception statement from your version. If you delete
// this exception statement from all source files in the program, then
// also delete it here.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

extern crate uuid;

use coding::{append_varint32, decode_varint32, unzigzag32, zigzag32};
use core::str::FromStr;
use db;
use error::Error;
use regex::Regex;
use std::ops;
use std::fmt;
use std::ops::Range;
use std::string::String;
use time;

pub const TIME_UNITS_PER_SEC: i64 = 90000;
pub const DESIRED_RECORDING_DURATION: i64 = 60 * TIME_UNITS_PER_SEC;
pub const MAX_RECORDING_DURATION: i64 = 5 * 60 * TIME_UNITS_PER_SEC;

/// A time specified as 90,000ths of a second since 1970-01-01 00:00:00 UTC.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Time(pub i64);

impl Time {
    pub fn new(tm: time::Timespec) -> Self {
        Time(tm.sec * TIME_UNITS_PER_SEC + tm.nsec as i64 * TIME_UNITS_PER_SEC / 1_000_000_000)
    }

    /// Parses a time as either 90,000ths of a second since epoch or a RFC 3339-like string.
    ///
    /// The former is 90,000ths of a second since 1970-01-01T00:00:00 UTC, excluding leap seconds.
    ///
    /// The latter is a string such as `2006-01-02T15:04:05`, followed by an optional 90,000ths of
    /// a second such as `:00001`, followed by an optional time zone offset such as `Z` or
    /// `-07:00`. A missing fraction is assumed to be 0. A missing time zone offset implies the
    /// local time zone.
    pub fn parse(s: &str) -> Result<Self, Error> {
        lazy_static! {
            static ref RE: Regex = Regex::new(r#"(?x)
                ^
                ([0-9]{4})-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2})
                (?::([0-9]{5}))?
                (Z|[+-]([0-9]{2}):([0-9]{2}))?
                $"#).unwrap();
        }

        // First try parsing as 90,000ths of a second since epoch.
        match i64::from_str(s) {
            Ok(i) => return Ok(Time(i)),
            Err(_) => {},
        }

        // If that failed, parse as a time string or bust.
        let c = RE.captures(s).ok_or_else(|| Error::new(format!("unparseable time {:?}", s)))?;
        let mut tm = time::Tm{
            tm_sec: i32::from_str(c.get(6).unwrap().as_str()).unwrap(),
            tm_min: i32::from_str(c.get(5).unwrap().as_str()).unwrap(),
            tm_hour: i32::from_str(c.get(4).unwrap().as_str()).unwrap(),
            tm_mday: i32::from_str(c.get(3).unwrap().as_str()).unwrap(),
            tm_mon: i32::from_str(c.get(2).unwrap().as_str()).unwrap(),
            tm_year: i32::from_str(c.get(1).unwrap().as_str()).unwrap(),
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: -1,
            tm_utcoff: 0,
            tm_nsec: 0,
        };
        if tm.tm_mon == 0 {
            return Err(Error::new(format!("time {:?} has month 0", s)));
        }
        tm.tm_mon -= 1;
        if tm.tm_year < 1900 {
            return Err(Error::new(format!("time {:?} has year before 1900", s)));
        }
        tm.tm_year -= 1900;

        // The time crate doesn't use tm_utcoff properly; it just calls timegm() if tm_utcoff == 0,
        // mktime() otherwise. If a zone is specified, use the timegm path and a manual offset.
        // If no zone is specified, use the tm_utcoff path. This is pretty lame, but follow the
        // chrono crate's lead and just use 0 or 1 to choose between these functions.
        let sec = if let Some(zone) = c.get(8) {
            tm.to_timespec().sec + if zone.as_str() == "Z" {
                0
            } else {
                let off = i64::from_str(c.get(9).unwrap().as_str()).unwrap() * 3600 +
                          i64::from_str(c.get(10).unwrap().as_str()).unwrap() * 60;
                if zone.as_str().as_bytes()[0] == b'-' { off } else { -off }
            }
        } else {
            tm.tm_utcoff = 1;
            tm.to_timespec().sec
        };
        let fraction = if let Some(f) = c.get(7) { i64::from_str(f.as_str()).unwrap() } else { 0 };
        Ok(Time(sec * TIME_UNITS_PER_SEC + fraction))
    }

    pub fn unix_seconds(&self) -> i64 { self.0 / TIME_UNITS_PER_SEC }
}

impl ops::Sub for Time {
    type Output = Duration;
    fn sub(self, rhs: Time) -> Duration { Duration(self.0 - rhs.0) }
}

impl ops::AddAssign<Duration> for Time {
    fn add_assign(&mut self, rhs: Duration) { self.0 += rhs.0 }
}

impl ops::Add<Duration> for Time {
    type Output = Time;
    fn add(self, rhs: Duration) -> Time { Time(self.0 + rhs.0) }
}

impl ops::Sub<Duration> for Time {
    type Output = Time;
    fn sub(self, rhs: Duration) -> Time { Time(self.0 - rhs.0) }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let tm = time::at(time::Timespec{sec: self.0 / TIME_UNITS_PER_SEC, nsec: 0});
        let zone_minutes = tm.tm_utcoff.abs() / 60;
        write!(f, "{}:{:05}{}{:02}:{:02}", tm.strftime("%FT%T").or_else(|_| Err(fmt::Error))?,
               self.0 % TIME_UNITS_PER_SEC,
               if tm.tm_utcoff > 0 { '+' } else { '-' }, zone_minutes / 60, zone_minutes % 60)
    }
}

/// A duration specified in 1/90,000ths of a second.
/// Durations are typically non-negative, but a `db::CameraDayValue::duration` may be negative.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Duration(pub i64);

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut seconds = self.0 / TIME_UNITS_PER_SEC;
        const MINUTE_IN_SECONDS: i64 = 60;
        const HOUR_IN_SECONDS: i64 = 60 * MINUTE_IN_SECONDS;
        const DAY_IN_SECONDS: i64 = 24 * HOUR_IN_SECONDS;
        let days = seconds / DAY_IN_SECONDS;
        seconds %= DAY_IN_SECONDS;
        let hours = seconds / HOUR_IN_SECONDS;
        seconds %= HOUR_IN_SECONDS;
        let minutes = seconds / MINUTE_IN_SECONDS;
        seconds %= MINUTE_IN_SECONDS;
        let mut have_written = if days > 0 {
            write!(f, "{} day{}", days, if days == 1 { "" } else { "s" })?;
            true
        } else {
            false
        };
        if hours > 0 {
            write!(f, "{}{} hour{}", if have_written { " " } else { "" },
                   hours, if hours == 1 { "" } else { "s" })?;
            have_written = true;
        }
        if minutes > 0 {
            write!(f, "{}{} minute{}", if have_written { " " } else { "" },
                   minutes, if minutes == 1 { "" } else { "s" })?;
            have_written = true;
        }
        if seconds > 0 || !have_written {
            write!(f, "{}{} second{}", if have_written { " " } else { "" },
                   seconds, if seconds == 1 { "" } else { "s" })?;
        }
        Ok(())
    }
}

impl ops::Add for Duration {
    type Output = Duration;
    fn add(self, rhs: Duration) -> Duration { Duration(self.0 + rhs.0) }
}

impl ops::AddAssign for Duration {
    fn add_assign(&mut self, rhs: Duration) { self.0 += rhs.0 }
}

impl ops::SubAssign for Duration {
    fn sub_assign(&mut self, rhs: Duration) { self.0 -= rhs.0 }
}

/// An iterator through a sample index.
/// Initially invalid; call `next()` before each read.
#[derive(Clone, Copy, Debug)]
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
    pub fn new() -> SampleIndexIterator {
        SampleIndexIterator{i_and_is_key: 0,
                            pos: 0,
                            start_90k: 0,
                            duration_90k: 0,
                            bytes: 0,
                            bytes_other: 0}
    }

    pub fn next(&mut self, data: &[u8]) -> Result<bool, Error> {
        self.pos += self.bytes;
        self.start_90k += self.duration_90k;
        let i = (self.i_and_is_key & 0x7FFF_FFFF) as usize;
        if i == data.len() {
            return Ok(false)
        }
        let (raw1, i1) = match decode_varint32(data, i) {
            Ok(tuple) => tuple,
            Err(()) => return Err(Error::new(format!("bad varint 1 at offset {}", i))),
        };
        let (raw2, i2) = match decode_varint32(data, i1) {
            Ok(tuple) => tuple,
            Err(()) => return Err(Error::new(format!("bad varint 2 at offset {}", i1))),
        };
        let duration_90k_delta = unzigzag32(raw1 >> 1);
        self.duration_90k += duration_90k_delta;
        if self.duration_90k < 0 {
            return Err(Error{
                description: format!("negative duration {} after applying delta {}",
                                     self.duration_90k, duration_90k_delta),
                cause: None});
        }
        if self.duration_90k == 0 && data.len() > i2 {
            return Err(Error{
                description: format!("zero duration only allowed at end; have {} bytes left",
                                     data.len() - i2),
                cause: None});
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
            return Err(Error{
                description: format!("non-positive bytes {} after applying delta {} to key={} \
                                      frame at ts {}", self.bytes, bytes_delta, self.is_key(),
                                      self.start_90k),
                cause: None});
        }
        Ok(true)
    }

    pub fn uninitialized(&self) -> bool { self.i_and_is_key == 0 }
    pub fn is_key(&self) -> bool { (self.i_and_is_key & 0x8000_0000) != 0 }
}

#[derive(Debug)]
pub struct SampleIndexEncoder {
    // Internal state.
    prev_duration_90k: i32,
    prev_bytes_key: i32,
    prev_bytes_nonkey: i32,

    // Eventual output.
    // TODO: move to another struct?
    pub sample_file_bytes: i32,
    pub total_duration_90k: i32,
    pub video_samples: i32,
    pub video_sync_samples: i32,
    pub video_index: Vec<u8>,
}

impl SampleIndexEncoder {
    pub fn new() -> Self {
        SampleIndexEncoder{
            prev_duration_90k: 0,
            prev_bytes_key: 0,
            prev_bytes_nonkey: 0,
            total_duration_90k: 0,
            sample_file_bytes: 0,
            video_samples: 0,
            video_sync_samples: 0,
            video_index: Vec::new(),
        }
    }

    pub fn add_sample(&mut self, duration_90k: i32, bytes: i32, is_key: bool) {
        let duration_delta = duration_90k - self.prev_duration_90k;
        self.prev_duration_90k = duration_90k;
        self.total_duration_90k += duration_90k;
        self.sample_file_bytes += bytes;
        self.video_samples += 1;
        let bytes_delta = bytes - if is_key {
            let prev = self.prev_bytes_key;
            self.video_sync_samples += 1;
            self.prev_bytes_key = bytes;
            prev
        } else {
            let prev = self.prev_bytes_nonkey;
            self.prev_bytes_nonkey = bytes;
            prev
        };
        append_varint32((zigzag32(duration_delta) << 1) | (is_key as u32), &mut self.video_index);
        append_varint32(zigzag32(bytes_delta), &mut self.video_index);
    }

    pub fn has_trailing_zero(&self) -> bool { self.prev_duration_90k == 0 }
}

/// A segment represents a view of some or all of a single recording, starting from a key frame.
/// Used by the `Mp4FileBuilder` class to splice together recordings into a single virtual .mp4.
pub struct Segment {
    pub camera_id: i32,
    pub recording_id: i32,
    pub start: Time,
    begin: SampleIndexIterator,
    pub file_end: i32,
    pub desired_range_90k: Range<i32>,
    actual_end_90k: i32,
    pub frames: u16,
    pub key_frames: u16,
    video_sample_entry_id_and_trailing_zero: i32,
}

impl Segment {
    /// Creates a segment.
    ///
    /// `desired_range_90k` represents the desired range of the segment relative to the start of
    /// the recording. The actual range will start at the first key frame at or before the
    /// desired start time. (The caller is responsible for creating an edit list to skip the
    /// undesired portion.) It will end at the first frame after the desired range (unless the
    /// desired range extends beyond the recording).
    pub fn new(db: &db::LockedDatabase, recording: &db::ListRecordingsRow,
               desired_range_90k: Range<i32>) -> Result<Segment, Error> {
        let mut self_ = Segment{
            camera_id: recording.camera_id,
            recording_id: recording.id,
            start: recording.start,
            begin: SampleIndexIterator::new(),
            file_end: recording.sample_file_bytes,
            desired_range_90k: desired_range_90k,
            actual_end_90k: recording.duration_90k,
            frames: recording.video_samples as u16,
            key_frames: recording.video_sync_samples as u16,
            video_sample_entry_id_and_trailing_zero:
                recording.video_sample_entry.id |
                ((((recording.flags & db::RecordingFlags::TrailingZero as i32) != 0) as i32) << 31),
        };

        if self_.desired_range_90k.start > self_.desired_range_90k.end ||
           self_.desired_range_90k.end > self_.actual_end_90k {
            return Err(Error::new(format!(
                "desired range [{}, {}) invalid for recording of length {}",
                self_.desired_range_90k.start, self_.desired_range_90k.end, self_.actual_end_90k)));
        }

        if self_.desired_range_90k.start == 0 &&
           self_.desired_range_90k.end == self_.actual_end_90k {
            // Fast path. Existing entry is fine.
            return Ok(self_)
        }

        // Slow path. Need to iterate through the index.
        db.with_recording_playback(self_.camera_id, self_.recording_id, |playback| {
            let data = &(&playback).video_index;
            let mut it = SampleIndexIterator::new();
            if !it.next(data)? {
                return Err(Error{description: String::from("no index"),
                                 cause: None});
            }
            if !it.is_key() {
                return Err(Error{description: String::from("not key frame"),
                                 cause: None});
            }

            // Stop when hitting a frame with this start time.
            // Going until the end of the recording is special-cased because there can be a trailing
            // frame of zero duration. It's unclear exactly how this should be handled, but let's
            // include it for consistency with the fast path. It'd be bizarre to have it included or
            // not based on desired_range_90k.start.
            let end_90k = if self_.desired_range_90k.end == self_.actual_end_90k {
                i32::max_value()
            } else {
                self_.desired_range_90k.end
            };

            loop {
                if it.start_90k <= self_.desired_range_90k.start && it.is_key() {
                    // new start candidate.
                    self_.begin = it;
                    self_.frames = 0;
                    self_.key_frames = 0;
                }
                if it.start_90k >= end_90k {
                    break;
                }
                self_.frames += 1;
                self_.key_frames += it.is_key() as u16;
                if !it.next(data)? {
                    break;
                }
            }
            self_.file_end = it.pos;
            self_.actual_end_90k = it.start_90k;
            self_.video_sample_entry_id_and_trailing_zero =
                recording.video_sample_entry.id |
                (((it.duration_90k == 0) as i32) << 31);
            Ok(self_)
        })
    }

    pub fn video_sample_entry_id(&self) -> i32 {
        self.video_sample_entry_id_and_trailing_zero & 0x7FFFFFFF
    }

    pub fn have_trailing_zero(&self) -> bool { self.video_sample_entry_id_and_trailing_zero < 0 }

    /// Returns the byte range within the sample file of data associated with this segment.
    pub fn sample_file_range(&self) -> Range<u64> { self.begin.pos as u64 .. self.file_end as u64 }

    /// Returns the actual time range as described in `new`.
    pub fn actual_time_90k(&self) -> Range<i32> { self.begin.start_90k .. self.actual_end_90k }

    /// Iterates through each frame in the segment.
    /// Must be called without the database lock held; retrieves video index from the cache.
    pub fn foreach<F>(&self, playback: &db::RecordingPlayback, mut f: F) -> Result<(), Error>
    where F: FnMut(&SampleIndexIterator) -> Result<(), Error> {
        trace!("foreach on recording {}/{}: {} frames, actual_time_90k: {:?}",
              self.camera_id, self.recording_id, self.frames, self.actual_time_90k());
        let data = &(&playback).video_index;
        let mut it = self.begin;
        if it.uninitialized() {
            if !it.next(data)? {
                return Err(Error::new(format!("recording {}/{}: no frames",
                                              self.camera_id, self.recording_id)));
            }
            if !it.is_key() {
                return Err(Error::new(format!("recording {}/{}: doesn't start with key frame",
                                              self.camera_id, self.recording_id)));
            }
        }
        let mut have_frame = true;
        let mut key_frame = 0;
        for i in 0 .. self.frames {
            if !have_frame {
                return Err(Error::new(format!("recording {}/{}: expected {} frames, found only {}",
                                              self.camera_id, self.recording_id, self.frames,
                                              i+1)));
            }
            if it.is_key() {
                key_frame += 1;
                if key_frame > self.key_frames {
                    return Err(Error::new(format!(
                        "recording {}/{}: more than expected {} key frames",
                        self.camera_id, self.recording_id, self.key_frames)));
                }
            }

            // Note: this inner loop uses try! rather than ? for performance. Don't change these
            // lines without reading https://github.com/rust-lang/rust/issues/37939 and running
            // mp4::bench::build_index.
            try!(f(&it));
            have_frame = try!(it.next(data));
        }
        if key_frame < self.key_frames {
            return Err(Error::new(format!("recording {}/{}: expected {} key frames, found only {}",
                                          self.camera_id, self.recording_id, self.key_frames,
                                          key_frame)));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testutil::TestDb;

    #[test]
    fn test_parse_time() {
        let tests = &[
            ("2006-01-02T15:04:05-07:00",       102261550050000),
            ("2006-01-02T15:04:05:00001-07:00", 102261550050001),
            ("2006-01-02T15:04:05-08:00",       102261874050000),
            ("2006-01-02T15:04:05",             102261874050000),  // implied -08:00
            ("2006-01-02T15:04:05:00001",       102261874050001),  // implied -08:00
            ("2006-01-02T15:04:05-00:00",       102259282050000),
            ("2006-01-02T15:04:05Z",            102259282050000),
            ("102261550050000",                 102261550050000),
        ];
        for test in tests {
            assert_eq!(test.1, Time::parse(test.0).unwrap().0, "parsing {}", test.0);
        }
    }

    #[test]
    fn test_format_time() {
        assert_eq!("2006-01-02T15:04:05:00000-08:00", format!("{}", Time(102261874050000)));
    }

    #[test]
    fn test_display_duration() {
        let tests = &[
            // (output, seconds)
            ("0 seconds", 0),
            ("1 second", 1),
            ("1 minute", 60),
            ("1 minute 1 second", 61),
            ("2 minutes", 120),
            ("1 hour", 3600),
            ("1 hour 1 minute", 3660),
            ("2 hours", 7200),
            ("1 day", 86400),
            ("1 day 1 hour", 86400 + 3600),
            ("2 days", 2 * 86400),
        ];
        for test in tests {
            assert_eq!(test.0, format!("{}", Duration(test.1 * TIME_UNITS_PER_SEC)));
        }
    }

    /// Tests encoding the example from design/schema.md.
    #[test]
    fn test_encode_example() {
        let mut e = SampleIndexEncoder::new();
        e.add_sample(10, 1000, true);
        e.add_sample(9, 10, false);
        e.add_sample(11, 15, false);
        e.add_sample(10, 12, false);
        e.add_sample(10, 1050, true);
        assert_eq!(e.video_index, b"\x29\xd0\x0f\x02\x14\x08\x0a\x02\x05\x01\x64");
        assert_eq!(10 + 9 + 11 + 10 + 10, e.total_duration_90k);
        assert_eq!(5, e.video_samples);
        assert_eq!(2, e.video_sync_samples);
    }

    /// Tests a round trip from `SampleIndexEncoder` to `SampleIndexIterator`.
    #[test]
    fn test_round_trip() {
        #[derive(Debug, PartialEq, Eq)]
        struct Sample {
            duration_90k: i32,
            bytes: i32,
            is_key: bool,
        }
        let samples = [
            Sample{duration_90k: 10, bytes: 30000, is_key: true},
            Sample{duration_90k:  9, bytes:  1000, is_key: false},
            Sample{duration_90k: 11, bytes:  1100, is_key: false},
            Sample{duration_90k: 18, bytes: 31000, is_key: true},
            Sample{duration_90k:  0, bytes:  1000, is_key: false},
        ];
        let mut e = SampleIndexEncoder::new();
        for sample in &samples {
            e.add_sample(sample.duration_90k, sample.bytes, sample.is_key);
        }
        let mut it = SampleIndexIterator::new();
        for sample in &samples {
            assert!(it.next(&e.video_index).unwrap());
            assert_eq!(sample,
                       &Sample{duration_90k: it.duration_90k,
                               bytes: it.bytes,
                               is_key: it.is_key()});
        }
        assert!(!it.next(&e.video_index).unwrap());
    }

    /// Tests that `SampleIndexIterator` spots several classes of errors.
    /// TODO: test and fix overflow cases.
    #[test]
    fn test_iterator_errors() {
        struct Test {
            encoded: &'static [u8],
            err: &'static str,
        }
        let tests = [
            Test{encoded: b"\x80",                     err: "bad varint 1 at offset 0"},
            Test{encoded: b"\x00\x80",                 err: "bad varint 2 at offset 1"},
            Test{encoded: b"\x00\x02\x00\x00",
                 err: "zero duration only allowed at end; have 2 bytes left"},
            Test{encoded: b"\x02\x02",
                 err: "negative duration -1 after applying delta -1"},
            Test{encoded: b"\x04\x00",
                 err: "non-positive bytes 0 after applying delta 0 to key=false frame at ts 0"},
        ];
        for test in &tests {
            let mut it = SampleIndexIterator::new();
            assert_eq!(it.next(test.encoded).unwrap_err().description, test.err);
        }
    }

    fn get_frames<F, T>(db: &db::Database, segment: &Segment, f: F) -> Vec<T>
    where F: Fn(&SampleIndexIterator) -> T {
        let mut v = Vec::new();
        db.lock().with_recording_playback(segment.camera_id, segment.recording_id, |playback| {
            segment.foreach(playback, |it| { v.push(f(it)); Ok(()) })
        }).unwrap();
        v
    }

    /// Tests that a `Segment` correctly can clip at the beginning and end.
    /// This is a simpler case; all sync samples means we can start on any frame.
    #[test]
    fn test_segment_clipping_with_all_sync() {
        let mut encoder = SampleIndexEncoder::new();
        for i in 1..6 {
            let duration_90k = 2 * i;
            let bytes = 3 * i;
            encoder.add_sample(duration_90k, bytes, true);
        }
        let db = TestDb::new();
        let row = db.create_recording_from_encoder(encoder);
        // Time range [2, 2 + 4 + 6 + 8) means the 2nd, 3rd, 4th samples should be
        // included.
        let segment = Segment::new(&db.db.lock(), &row, 2 .. 2+4+6+8).unwrap();
        assert_eq!(&get_frames(&db.db, &segment, |it| it.duration_90k), &[4, 6, 8]);
    }

    /// Half sync frames means starting from the last sync frame <= desired point.
    #[test]
    fn test_segment_clipping_with_half_sync() {
        let mut encoder = SampleIndexEncoder::new();
        for i in 1..6 {
            let duration_90k = 2 * i;
            let bytes = 3 * i;
            encoder.add_sample(duration_90k, bytes, (i % 2) == 1);
        }
        let db = TestDb::new();
        let row = db.create_recording_from_encoder(encoder);
        // Time range [2 + 4 + 6, 2 + 4 + 6 + 8) means the 4th sample should be included.
        // The 3rd also gets pulled in because it is a sync frame and the 4th is not.
        let segment = Segment::new(&db.db.lock(), &row, 2+4+6 .. 2+4+6+8).unwrap();
        assert_eq!(&get_frames(&db.db, &segment, |it| it.duration_90k), &[6, 8]);
    }

    #[test]
    fn test_segment_clipping_with_trailing_zero() {
        let mut encoder = SampleIndexEncoder::new();
        encoder.add_sample(1, 1, true);
        encoder.add_sample(1, 2, true);
        encoder.add_sample(0, 3, true);
        let db = TestDb::new();
        let row = db.create_recording_from_encoder(encoder);
        let segment = Segment::new(&db.db.lock(), &row, 1 .. 2).unwrap();
        assert_eq!(&get_frames(&db.db, &segment, |it| it.bytes), &[2, 3]);
    }

    /// Test a `Segment` which uses the whole recording.
    /// This takes a fast path which skips scanning the index in `new()`.
    #[test]
    fn test_segment_fast_path() {
        let mut encoder = SampleIndexEncoder::new();
        for i in 1..6 {
            let duration_90k = 2 * i;
            let bytes = 3 * i;
            encoder.add_sample(duration_90k, bytes, (i % 2) == 1);
        }
        let db = TestDb::new();
        let row = db.create_recording_from_encoder(encoder);
        let segment = Segment::new(&db.db.lock(), &row, 0 .. 2+4+6+8+10).unwrap();
        assert_eq!(&get_frames(&db.db, &segment, |it| it.duration_90k), &[2, 4, 6, 8, 10]);
    }

    #[test]
    fn test_segment_fast_path_with_trailing_zero() {
        let mut encoder = SampleIndexEncoder::new();
        encoder.add_sample(1, 1, true);
        encoder.add_sample(1, 2, true);
        encoder.add_sample(0, 3, true);
        let db = TestDb::new();
        let row = db.create_recording_from_encoder(encoder);
        let segment = Segment::new(&db.db.lock(), &row, 0 .. 2).unwrap();
        assert_eq!(&get_frames(&db.db, &segment, |it| it.bytes), &[1, 2, 3]);
    }

    // TODO: test segment error cases involving mismatch between row frames/key_frames and index.
}

#[cfg(all(test, feature="nightly"))]
mod bench {
    extern crate test;
    use self::test::Bencher;
    use super::*;

    /// Benchmarks the decoder, which is performance-critical for .mp4 serving.
    #[bench]
    fn bench_decoder(b: &mut Bencher) {
        let data = include_bytes!("testdata/video_sample_index.bin");
        b.bytes = data.len() as u64;
        b.iter(|| {
            let mut it = SampleIndexIterator::new();
            while it.next(data).unwrap() {}
            assert_eq!(30104460, it.pos);
            assert_eq!(5399985, it.start_90k);
        });
    }
}
