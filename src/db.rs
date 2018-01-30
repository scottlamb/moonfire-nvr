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

//! Database access logic for the Moonfire NVR SQLite schema.
//!
//! The SQLite schema includes everything except the actual video samples (see the `dir` module
//! for management of those). See `schema.sql` for a more detailed description.
//!
//! The `Database` struct caches data in RAM, making the assumption that only one process is
//! accessing the database at a time. Performance and efficiency notes:
//!
//!   * several query operations here feature row callbacks. The callback is invoked with
//!     the database lock. Thus, the callback shouldn't perform long-running operations.
//!
//!   * startup may be slow, as it scans the entire index for the recording table. This seems
//!     acceptable.
//!
//!   * the operations used for web file serving should return results with acceptable latency.
//!
//!   * however, the database lock may be held for longer than is acceptable for
//!     the critical path of recording frames. The caller should preallocate sample file uuids
//!     and such to avoid database operations in these paths.
//!
//!   * the `Transaction` interface allows callers to batch write operations to reduce latency and
//!     SSD write cycles.

use error::{Error, ResultExt};
use fnv;
use h264;
use lru_cache::LruCache;
use openssl::hash;
use parking_lot::{Mutex,MutexGuard};
use recording::{self, TIME_UNITS_PER_SEC};
use rusqlite;
use std::collections::BTreeMap;
use std::collections::btree_map;
use std::cell::RefCell;
use std::cmp;
use std::io::Write;
use std::ops::Range;
use std::mem;
use std::str;
use std::string::String;
use std::sync::Arc;
use std::vec::Vec;
use time;
use uuid::Uuid;

/// Expected schema version. See `guide/schema.md` for more information.
pub const EXPECTED_VERSION: i32 = 2;

const GET_RECORDING_PLAYBACK_SQL: &'static str = r#"
    select
      sample_file_uuid,
      video_index
    from
      recording_playback
    where
      composite_id = :composite_id
"#;

const DELETE_RESERVATION_SQL: &'static str =
    "delete from reserved_sample_files where uuid = :uuid";

const INSERT_RESERVATION_SQL: &'static str = r#"
    insert into reserved_sample_files (uuid,  state)
                               values (:uuid, :state)
"#;

/// Valid values for the `state` column in the `reserved_sample_files` table.
enum ReservationState {
    /// This uuid has not yet been added to the `recording` table. The file may be unwritten,
    /// partially written, or fully written.
    Writing = 0,

    /// This uuid was previously in the `recording` table. The file may be fully written or
    /// unlinked.
    Deleting = 1,
}

const INSERT_VIDEO_SAMPLE_ENTRY_SQL: &'static str = r#"
    insert into video_sample_entry (sha1,  width,  height,  data)
                            values (:sha1, :width, :height, :data)
"#;

const INSERT_RECORDING_SQL: &'static str = r#"
    insert into recording (composite_id, stream_id, run_offset, flags, sample_file_bytes,
                           start_time_90k, duration_90k, local_time_delta_90k, video_samples,
                           video_sync_samples, video_sample_entry_id)
                   values (:composite_id, :stream_id, :run_offset, :flags, :sample_file_bytes,
                           :start_time_90k, :duration_90k, :local_time_delta_90k,
                           :video_samples, :video_sync_samples, :video_sample_entry_id)
"#;

const INSERT_RECORDING_PLAYBACK_SQL: &'static str = r#"
    insert into recording_playback (composite_id, sample_file_uuid, sample_file_sha1, video_index)
                            values (:composite_id, :sample_file_uuid, :sample_file_sha1,
                                    :video_index)
"#;

const UPDATE_NEXT_RECORDING_ID_SQL: &'static str =
    "update stream set next_recording_id = :next_recording_id where id = :stream_id";

const LIST_OLDEST_SAMPLE_FILES_SQL: &'static str = r#"
    select
      recording.composite_id,
      recording_playback.sample_file_uuid,
      recording.start_time_90k,
      recording.duration_90k,
      recording.sample_file_bytes
    from
      recording
      join recording_playback on (recording.composite_id = recording_playback.composite_id)
    where
      :start <= recording.composite_id and
      recording.composite_id < :end
    order by
      recording.composite_id
"#;

const DELETE_RECORDING_SQL: &'static str = r#"
    delete from recording where composite_id = :composite_id
"#;

const DELETE_RECORDING_PLAYBACK_SQL: &'static str = r#"
    delete from recording_playback where composite_id = :composite_id
"#;

const STREAM_MIN_START_SQL: &'static str = r#"
    select
      start_time_90k
    from
      recording
    where
      stream_id = :stream_id
    order by start_time_90k limit 1
"#;

const STREAM_MAX_START_SQL: &'static str = r#"
    select
      start_time_90k,
      duration_90k
    from
      recording
    where
      stream_id = :stream_id
    order by start_time_90k desc;
"#;

const LIST_RECORDINGS_BY_ID_SQL: &'static str = r#"
    select
        recording.composite_id,
        recording.run_offset,
        recording.flags,
        recording.start_time_90k,
        recording.duration_90k,
        recording.sample_file_bytes,
        recording.video_samples,
        recording.video_sync_samples,
        recording.video_sample_entry_id
    from
        recording
    where
        :start <= composite_id and
        composite_id < :end
    order by
        recording.composite_id
"#;

struct FromSqlUuid(Uuid);

impl rusqlite::types::FromSql for FromSqlUuid {
    fn column_result(value: rusqlite::types::ValueRef) -> rusqlite::types::FromSqlResult<Self> {
        let uuid = Uuid::from_bytes(value.as_blob()?)
            .map_err(|e| rusqlite::types::FromSqlError::Other(Box::new(e)))?;
        Ok(FromSqlUuid(uuid))
    }
}

/// A box with space for the uuid (initially uninitialized) and the video index.
/// The caller must fill the uuid bytes.
struct PlaybackData(Box<[u8]>);

impl rusqlite::types::FromSql for PlaybackData {
    fn column_result(value: rusqlite::types::ValueRef) -> rusqlite::types::FromSqlResult<Self> {
        let blob = value.as_blob()?;
        let len = 16 + blob.len();
        let mut v = Vec::with_capacity(len);
        unsafe { v.set_len(len) };
        v[16..].copy_from_slice(blob);
        Ok(PlaybackData(v.into_boxed_slice()))
    }
}

/// A concrete box derived from a ISO/IEC 14496-12 section 8.5.2 VisualSampleEntry box. Describes
/// the codec, width, height, etc.
#[derive(Debug)]
pub struct VideoSampleEntry {
    pub data: Vec<u8>,
    pub rfc6381_codec: String,
    pub id: i32,
    pub width: u16,
    pub height: u16,
    pub sha1: [u8; 20],
}

/// A row used in `list_recordings_by_time` and `list_recordings_by_id`.
#[derive(Debug)]
pub struct ListRecordingsRow {
    pub start: recording::Time,
    pub video_sample_entry: Arc<VideoSampleEntry>,

    pub stream_id: i32,
    pub id: i32,

    /// This is a recording::Duration, but a single recording's duration fits into an i32.
    pub duration_90k: i32,
    pub video_samples: i32,
    pub video_sync_samples: i32,
    pub sample_file_bytes: i32,
    pub run_offset: i32,
    pub flags: i32,
}

/// A row used in `list_aggregated_recordings`.
#[derive(Clone, Debug)]
pub struct ListAggregatedRecordingsRow {
    pub time: Range<recording::Time>,
    pub ids: Range<i32>,
    pub video_samples: i64,
    pub video_sync_samples: i64,
    pub sample_file_bytes: i64,
    pub video_sample_entry: Arc<VideoSampleEntry>,
    pub stream_id: i32,
    pub flags: i32,
    pub run_start_id: i32,
}

/// Select fields from the `recordings_playback` table. Retrieve with `with_recording_playback`.
#[derive(Debug)]
pub struct RecordingPlayback<'a> {
    pub sample_file_uuid: Uuid,
    pub video_index: &'a [u8],
}

impl<'a> RecordingPlayback<'a> {
    fn new(data: &'a [u8]) -> Self {
        RecordingPlayback {
            sample_file_uuid: Uuid::from_bytes(&data[..16]).unwrap(),
            video_index: &data[16..],
        }
    }
}

/// Bitmask in the `flags` field in the `recordings` table; see `schema.sql`.
pub enum RecordingFlags {
    TrailingZero = 1,
}

/// A recording to pass to `insert_recording`.
#[derive(Debug)]
pub struct RecordingToInsert {
    pub stream_id: i32,
    pub run_offset: i32,
    pub flags: i32,
    pub sample_file_bytes: i32,
    pub time: Range<recording::Time>,
    pub local_time_delta: recording::Duration,
    pub video_samples: i32,
    pub video_sync_samples: i32,
    pub video_sample_entry_id: i32,
    pub sample_file_uuid: Uuid,
    pub video_index: Vec<u8>,
    pub sample_file_sha1: [u8; 20],
}

/// A row used in `list_oldest_sample_files`.
#[derive(Debug)]
pub struct ListOldestSampleFilesRow {
    pub uuid: Uuid,
    pub stream_id: i32,
    pub recording_id: i32,
    pub time: Range<recording::Time>,
    pub sample_file_bytes: i32,
}

/// A calendar day in `YYYY-mm-dd` format.
#[derive(Copy, Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct StreamDayKey([u8; 10]);

impl StreamDayKey {
    fn new(tm: time::Tm) -> Result<Self, Error> {
        let mut s = StreamDayKey([0u8; 10]);
        write!(&mut s.0[..], "{}", tm.strftime("%Y-%m-%d")?)?;
        Ok(s)
    }

    pub fn bounds(&self) -> Range<recording::Time> {
        let mut my_tm = time::strptime(self.as_ref(), "%Y-%m-%d").expect("days must be parseable");
        my_tm.tm_utcoff = 1;  // to the time crate, values != 0 mean local time.
        my_tm.tm_isdst = -1;
        let start = recording::Time(my_tm.to_timespec().sec * recording::TIME_UNITS_PER_SEC);
        my_tm.tm_hour = 0;
        my_tm.tm_min = 0;
        my_tm.tm_sec = 0;
        my_tm.tm_mday += 1;
        let end = recording::Time(my_tm.to_timespec().sec * recording::TIME_UNITS_PER_SEC);
        start .. end
    }
}

impl AsRef<str> for StreamDayKey {
    fn as_ref(&self) -> &str { str::from_utf8(&self.0[..]).expect("days are always UTF-8") }
}

/// In-memory state about a particular camera on a particular day.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StreamDayValue {
    /// The number of recordings that overlap with this day. Note that `adjust_day` automatically
    /// prunes days with 0 recordings.
    pub recordings: i64,

    /// The total duration recorded on this day. This can be 0; because frames' durations are taken
    /// from the time of the next frame, a recording that ends unexpectedly after a single frame
    /// will have 0 duration of that frame and thus the whole recording.
    pub duration: recording::Duration,
}

/// In-memory state about a camera.
#[derive(Debug)]
pub struct Camera {
    pub id: i32,
    pub uuid: Uuid,
    pub short_name: String,
    pub description: String,
    pub host: String,
    pub username: String,
    pub password: String,
    pub streams: [Option<i32>; 2],
}

#[derive(Copy, Clone, Debug)]
pub enum StreamType { MAIN, SUB }

impl StreamType {
    pub fn from_index(i: usize) -> Option<Self> {
        match i {
            0 => Some(StreamType::MAIN),
            1 => Some(StreamType::SUB),
            _ => None,
        }
    }

    pub fn index(self) -> usize {
        match self {
            StreamType::MAIN => 0,
            StreamType::SUB => 1,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StreamType::MAIN => "main",
            StreamType::SUB => "sub",
        }
    }

    pub fn parse(type_: &str) -> Option<Self> {
        match type_ {
            "main" => Some(StreamType::MAIN),
            "sub" => Some(StreamType::SUB),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Stream {
    pub id: i32,
    pub camera_id: i32,
    pub type_: StreamType,
    pub rtsp_path: String,
    pub retain_bytes: i64,

    /// The time range of recorded data associated with this stream (minimum start time and maximum
    /// end time). `None` iff there are no recordings for this camera.
    pub range: Option<Range<recording::Time>>,
    pub sample_file_bytes: i64,

    /// The total duration of recorded data. This may not be `range.end - range.start` due to
    /// gaps and overlap.
    pub duration: recording::Duration,

    /// Mapping of calendar day (in the server's time zone) to a summary of recordings on that day.
    pub days: BTreeMap<StreamDayKey, StreamDayValue>,
    pub record: bool,
    next_recording_id: i32,
}

/// Information about a camera, used by `add_camera` and `update_camera`.
#[derive(Debug)]
pub struct CameraChange {
    pub short_name: String,
    pub description: String,
    pub host: String,
    pub username: String,
    pub password: String,
    pub rtsp_paths: [String; 2],
}

/// Adds non-zero `delta` to the day represented by `day` in the map `m`.
/// Inserts a map entry if absent; removes the entry if it has 0 entries on exit.
fn adjust_day(day: StreamDayKey, delta: StreamDayValue,
              m: &mut BTreeMap<StreamDayKey, StreamDayValue>) {
    use ::std::collections::btree_map::Entry;
    match m.entry(day) {
        Entry::Vacant(e) => { e.insert(delta); },
        Entry::Occupied(mut e) => {
            let remove = {
                let v = e.get_mut();
                v.recordings += delta.recordings;
                v.duration += delta.duration;
                v.recordings == 0
            };
            if remove {
                e.remove_entry();
            }
        },
    }
}

/// Adjusts the day map `m` to reflect the range of the given recording.
/// Note that the specified range may span two days. It will never span more because the maximum
/// length of a recording entry is less than a day (even a 23-hour "spring forward" day).
///
/// This function swallows/logs date formatting errors because they shouldn't happen and there's
/// not much that can be done about them. (The database operation has already gone through.)
fn adjust_days(r: Range<recording::Time>, sign: i64,
               m: &mut BTreeMap<StreamDayKey, StreamDayValue>) {
    // Find first day key.
    let mut my_tm = time::at(time::Timespec{sec: r.start.unix_seconds(), nsec: 0});
    let day = match StreamDayKey::new(my_tm) {
        Ok(d) => d,
        Err(ref e) => {
            error!("Unable to fill first day key from {:?}: {}; will ignore.", my_tm, e);
            return;
        }
    };

    // Determine the start of the next day.
    // Use mytm to hold a non-normalized representation of the boundary.
    my_tm.tm_isdst = -1;
    my_tm.tm_hour = 0;
    my_tm.tm_min = 0;
    my_tm.tm_sec = 0;
    my_tm.tm_mday += 1;
    let boundary = my_tm.to_timespec();
    let boundary_90k = boundary.sec * TIME_UNITS_PER_SEC;

    // Adjust the first day.
    let first_day_delta = StreamDayValue{
        recordings: sign,
        duration: recording::Duration(sign * (cmp::min(r.end.0, boundary_90k) - r.start.0)),
    };
    adjust_day(day, first_day_delta, m);

    if r.end.0 <= boundary_90k {
        return;
    }

    // Fill day with the second day. This requires a normalized representation so recalculate.
    // (The C mktime(3) already normalized for us once, but .to_timespec() discarded that result.)
    let my_tm = time::at(boundary);
    let day = match StreamDayKey::new(my_tm) {
        Ok(d) => d,
        Err(ref e) => {
            error!("Unable to fill second day key from {:?}: {}; will ignore.", my_tm, e);
            return;
        }
    };
    let second_day_delta = StreamDayValue{
        recordings: sign,
        duration: recording::Duration(sign * (r.end.0 - boundary_90k)),
    };
    adjust_day(day, second_day_delta, m);
}

impl Stream {
    /// Adds a single recording with the given properties to the in-memory state.
    fn add_recording(&mut self, r: Range<recording::Time>, sample_file_bytes: i32) {
        self.range = Some(match self.range {
            Some(ref e) => cmp::min(e.start, r.start) .. cmp::max(e.end, r.end),
            None => r.start .. r.end,
        });
        self.duration += r.end - r.start;
        self.sample_file_bytes += sample_file_bytes as i64;
        adjust_days(r, 1, &mut self.days);
    }
}

/// Initializes the recordings associated with the given camera.
fn init_recordings(conn: &mut rusqlite::Connection, stream_id: i32, camera: &Camera,
                   stream: &mut Stream)
    -> Result<(), Error> {
    info!("Loading recordings for camera {} stream {:?}", camera.short_name, stream.type_);
    let mut stmt = conn.prepare(r#"
        select
          recording.start_time_90k,
          recording.duration_90k,
          recording.sample_file_bytes
        from
          recording
        where
          stream_id = :stream_id
    "#)?;
    let mut rows = stmt.query_named(&[(":stream_id", &stream_id)])?;
    let mut i = 0;
    while let Some(row) = rows.next() {
        let row = row?;
        let start = recording::Time(row.get_checked(0)?);
        let duration = recording::Duration(row.get_checked(1)?);
        let bytes = row.get_checked(2)?;
        stream.add_recording(start .. start + duration, bytes);
        i += 1;
    }
    info!("Loaded {} recordings for camera {} stream {:?}", i, camera.short_name, stream.type_);
    Ok(())
}

#[derive(Debug)]
pub struct LockedDatabase {
    conn: rusqlite::Connection,
    state: State,
}

/// In-memory state from the database.
/// This is separated out of `LockedDatabase` so that `Transaction` can mutably borrow `state`
/// while its underlying `rusqlite::Transaction` is borrowing `conn`.
#[derive(Debug)]
struct State {
    cameras_by_id: BTreeMap<i32, Camera>,
    streams_by_id: BTreeMap<i32, Stream>,
    cameras_by_uuid: BTreeMap<Uuid, i32>,
    video_sample_entries: BTreeMap<i32, Arc<VideoSampleEntry>>,
    list_recordings_by_time_sql: String,
    playback_cache: RefCell<LruCache<i64, Box<[u8]>, fnv::FnvBuildHasher>>,
}

/// A high-level transaction. This manages the SQLite transaction and the matching modification to
/// be applied to the in-memory state on successful commit.
pub struct Transaction<'a> {
    state: &'a mut State,
    mods_by_stream: fnv::FnvHashMap<i32, StreamModification>,
    tx: rusqlite::Transaction<'a>,

    /// True if due to an earlier error the transaction must be rolled back rather than committed.
    /// Insert and delete are multi-part. If later parts fail, earlier parts should be aborted as
    /// well. We could use savepoints (nested transactions) for this, but for simplicity we just
    /// require the entire transaction be rolled back.
    must_rollback: bool,

    /// Normally sample file uuids must be reserved prior to a recording being inserted.
    /// It's convenient in benchmarks though to allow the same segment to be inserted into the
    /// database many times, so this safety check can be disabled.
    pub bypass_reservation_for_testing: bool,
}

/// A modification to be done to a `Stream` after a `Transaction` is committed.
#[derive(Default)]
struct StreamModification {
    /// Add this to `camera.duration`. Thus, positive values indicate a net addition;
    /// negative values indicate a net subtraction.
    duration: recording::Duration,

    /// Add this to `camera.sample_file_bytes`.
    sample_file_bytes: i64,

    /// Add this to `stream.days`.
    days: BTreeMap<StreamDayKey, StreamDayValue>,

    /// Reset the Stream range to this value. This should be populated immediately prior to the
    /// commit.
    range: Option<Range<recording::Time>>,

    /// Reset the next_recording_id to the specified value.
    new_next_recording_id: Option<i32>,

    /// Reset the retain_bytes to the specified value.
    new_retain_bytes: Option<i64>,

    /// Reset the record to the specified value.
    new_record: Option<bool>,
}

fn composite_id(stream_id: i32, recording_id: i32) -> i64 {
    (stream_id as i64) << 32 | recording_id as i64
}

impl<'a> Transaction<'a> {
    /// Reserves a new, randomly generated UUID to be used as a sample file.
    pub fn reserve_sample_file(&mut self) -> Result<Uuid, Error> {
        let mut stmt = self.tx.prepare_cached(INSERT_RESERVATION_SQL)?;
        let uuid = Uuid::new_v4();
        let uuid_bytes = &uuid.as_bytes()[..];
        stmt.execute_named(&[
            (":uuid", &uuid_bytes),
            (":state", &(ReservationState::Writing as i64))
        ])?;
        info!("reserved {}", uuid);
        Ok(uuid)
    }

    /// Deletes the given recordings from the `recording` and `recording_playback` tables.
    /// Note they are not fully removed from the database; the uuids are transferred to the
    /// `reserved_sample_files` table. The caller should `unlink` the files, then remove the
    /// reservation.
    pub fn delete_recordings(&mut self, rows: &[ListOldestSampleFilesRow]) -> Result<(), Error> {
        let mut del1 = self.tx.prepare_cached(DELETE_RECORDING_PLAYBACK_SQL)?;
        let mut del2 = self.tx.prepare_cached(DELETE_RECORDING_SQL)?;
        let mut insert = self.tx.prepare_cached(INSERT_RESERVATION_SQL)?;

        self.check_must_rollback()?;
        self.must_rollback = true;
        for row in rows {
            let composite_id = &composite_id(row.stream_id, row.recording_id);
            let changes = del1.execute_named(&[(":composite_id", composite_id)])?;
            if changes != 1 {
                return Err(Error::new(format!("no such recording {}/{} (uuid {})",
                                              row.stream_id, row.recording_id, row.uuid)));
            }
            let changes = del2.execute_named(&[(":composite_id", composite_id)])?;
            if changes != 1 {
                return Err(Error::new(format!("no such recording_playback {}/{} (uuid {})",
                                              row.stream_id, row.recording_id, row.uuid)));
            }
            let uuid = &row.uuid.as_bytes()[..];
            insert.execute_named(&[
                (":uuid", &uuid),
                (":state", &(ReservationState::Deleting as i64))
            ])?;
            let m = Transaction::get_mods_by_stream(&mut self.mods_by_stream, row.stream_id);
            m.duration -= row.time.end - row.time.start;
            m.sample_file_bytes -= row.sample_file_bytes as i64;
            adjust_days(row.time.clone(), -1, &mut m.days);
        }
        self.must_rollback = false;
        Ok(())
    }

    /// Marks the given sample file uuid as deleted. Accepts uuids in either `ReservationState`.
    /// This shouldn't be called until the files have been `unlink()`ed and the parent directory
    /// `fsync()`ed.
    pub fn mark_sample_files_deleted(&mut self, uuids: &[Uuid]) -> Result<(), Error> {
        if uuids.is_empty() { return Ok(()); }
        let mut stmt =
            self.tx.prepare_cached("delete from reserved_sample_files where uuid = :uuid;")?;
        for uuid in uuids {
            let uuid_bytes = &uuid.as_bytes()[..];
            let changes = stmt.execute_named(&[(":uuid", &uuid_bytes)])?;
            if changes != 1 {
                return Err(Error::new(format!("no reservation for {}", uuid.hyphenated())));
            }
        }
        Ok(())
    }

    /// Inserts the specified recording.
    /// The sample file uuid must have been previously reserved. (Although this can be bypassed
    /// for testing; see the `bypass_reservation_for_testing` field.)
    pub fn insert_recording(&mut self, r: &RecordingToInsert) -> Result<i32, Error> {
        self.check_must_rollback()?;

        // Sanity checking.
        if r.time.end < r.time.start {
            return Err(Error::new(format!("end time {} must be >= start time {}",
                                          r.time.end, r.time.start)));
        }

        // Unreserve the sample file uuid and insert the recording row.
        // TODO: var used?
        let stream = match self.state.streams_by_id.get_mut(&r.stream_id) {
            None => return Err(Error::new(format!("no such stream id {}", r.stream_id))),
            Some(s) => s,
        };
        let uuid = &r.sample_file_uuid.as_bytes()[..];
        {
            let mut stmt = self.tx.prepare_cached(DELETE_RESERVATION_SQL)?;
            let changes = stmt.execute_named(&[(":uuid", &uuid)])?;
            if changes != 1 && !self.bypass_reservation_for_testing {
                return Err(Error::new(format!("uuid {} is not reserved", r.sample_file_uuid)));
            }
        }
        self.must_rollback = true;
        let m = Transaction::get_mods_by_stream(&mut self.mods_by_stream, r.stream_id);
        let recording_id;
        {
            recording_id = m.new_next_recording_id.unwrap_or(stream.next_recording_id);
            let composite_id = composite_id(r.stream_id, recording_id);
            let mut stmt = self.tx.prepare_cached(INSERT_RECORDING_SQL)?;
            stmt.execute_named(&[
                (":composite_id", &composite_id),
                (":stream_id", &(r.stream_id as i64)),
                (":run_offset", &r.run_offset),
                (":flags", &r.flags),
                (":sample_file_bytes", &r.sample_file_bytes),
                (":start_time_90k", &r.time.start.0),
                (":duration_90k", &(r.time.end.0 - r.time.start.0)),
                (":local_time_delta_90k", &r.local_time_delta.0),
                (":video_samples", &r.video_samples),
                (":video_sync_samples", &r.video_sync_samples),
                (":video_sample_entry_id", &r.video_sample_entry_id),
            ])?;
            m.new_next_recording_id = Some(recording_id + 1);
            let mut stmt = self.tx.prepare_cached(INSERT_RECORDING_PLAYBACK_SQL)?;
            let sha1 = &r.sample_file_sha1[..];
            stmt.execute_named(&[
                (":composite_id", &composite_id),
                (":sample_file_uuid", &uuid),
                (":sample_file_sha1", &sha1),
                (":video_index", &r.video_index),
            ])?;
            let mut stmt = self.tx.prepare_cached(UPDATE_NEXT_RECORDING_ID_SQL)?;
            stmt.execute_named(&[
                (":stream_id", &(r.stream_id as i64)),
                (":next_recording_id", &m.new_next_recording_id),
            ])?;
        }
        self.must_rollback = false;
        m.duration += r.time.end - r.time.start;
        m.sample_file_bytes += r.sample_file_bytes as i64;
        adjust_days(r.time.clone(), 1, &mut m.days);
        Ok(recording_id)
    }

    /// Updates the `record` and `retain_bytes` for the given stream.
    /// Note this just resets the limit in the database; it's the caller's responsibility to ensure
    /// current usage is under the new limit if desired.
    pub fn update_retention(&mut self, stream_id: i32, new_record: bool, new_limit: i64)
                            -> Result<(), Error> {
        if new_limit < 0 {
            return Err(Error::new(format!("can't set limit for stream {} to {}; must be >= 0",
                                          stream_id, new_limit)));
        }
        self.check_must_rollback()?;
        let mut stmt = self.tx.prepare_cached(r#"
            update stream
            set
              record = :record,
              retain_bytes = :retain
            where
              id = :id
        "#)?;
        let changes = stmt.execute_named(&[
            (":record", &new_record),
            (":retain", &new_limit),
            (":id", &stream_id),
        ])?;
        if changes != 1 {
            return Err(Error::new(format!("no such stream {}", stream_id)));
        }
        let m = Transaction::get_mods_by_stream(&mut self.mods_by_stream, stream_id);
        m.new_record = Some(new_record);
        m.new_retain_bytes = Some(new_limit);
        Ok(())
    }

    /// Commits these changes, consuming the Transaction.
    pub fn commit(mut self) -> Result<(), Error> {
        self.check_must_rollback()?;
        self.precommit()?;
        self.tx.commit()?;
        for (&stream_id, m) in &self.mods_by_stream {
            let stream = self.state.streams_by_id.get_mut(&stream_id)
                                                 .expect("modified stream must exist");
            stream.duration += m.duration;
            stream.sample_file_bytes += m.sample_file_bytes;
            for (k, v) in &m.days {
                adjust_day(*k, *v, &mut stream.days);
            }
            stream.range = m.range.clone();
            if let Some(id) = m.new_next_recording_id {
                stream.next_recording_id = id;
            }
            if let Some(r) = m.new_record {
                stream.record = r;
            }
            if let Some(b) = m.new_retain_bytes {
                stream.retain_bytes = b;
            }
        }
        Ok(())
    }

    /// Raises an error if `must_rollback` is true. To be used on commit and in modifications.
    fn check_must_rollback(&self) -> Result<(), Error> {
        if self.must_rollback {
            return Err(Error::new("failing due to previous error".to_owned()));
        }
        Ok(())
    }

    /// Looks up an existing entry in `mods` for a given stream or makes+inserts an identity entry.
    fn get_mods_by_stream(mods: &mut fnv::FnvHashMap<i32, StreamModification>, stream_id: i32)
                          -> &mut StreamModification {
        mods.entry(stream_id).or_insert_with(StreamModification::default)
    }

    /// Fills the `range` of each `StreamModification`. This is done prior to commit so that if the
    /// commit succeeds, there's no possibility that the correct state can't be retrieved.
    fn precommit(&mut self) -> Result<(), Error> {
        // Recompute start and end times for each camera.
        for (&stream_id, m) in &mut self.mods_by_stream {
            // The minimum is straightforward, taking advantage of the start_time_90k index.
            let mut stmt = self.tx.prepare_cached(STREAM_MIN_START_SQL)?;
            let mut rows = stmt.query_named(&[(":stream_id", &stream_id)])?;
            let min_start = match rows.next() {
                Some(row) => recording::Time(row?.get_checked(0)?),
                None => continue,  // no data; leave m.range alone.
            };

            // There was a minimum, so there should be a maximum too. Calculating it is less
            // straightforward because recordings could overlap. All recordings starting in the
            // last MAX_RECORDING_DURATION must be examined in order to take advantage of the
            // start_time_90k index.
            let mut stmt = self.tx.prepare_cached(STREAM_MAX_START_SQL)?;
            let mut rows = stmt.query_named(&[(":stream_id", &stream_id)])?;
            let mut maxes_opt = None;
            while let Some(row) = rows.next() {
                let row = row?;
                let row_start = recording::Time(row.get_checked(0)?);
                let row_duration: i64 = row.get_checked(1)?;
                let row_end = recording::Time(row_start.0 + row_duration);
                let maxes = match maxes_opt {
                    None => row_start .. row_end,
                    Some(Range{start: s, end: e}) => s .. cmp::max(e, row_end),
                };
                if row_start.0 <= maxes.start.0 - recording::MAX_RECORDING_DURATION {
                    break;
                }
                maxes_opt = Some(maxes);
            }
            let max_end = match maxes_opt {
                Some(Range{end: e, ..}) => e,
                None => {
                    return Err(Error::new(format!("missing max for stream {} which had min {}",
                                                  stream_id, min_start)));
                }
            };
            m.range = Some(min_start .. max_end);
        }
        Ok(())
    }
}

struct StreamInserter<'tx> {
    tx: &'tx rusqlite::Transaction<'tx>,
    stmt: rusqlite::Statement<'tx>,
    new_streams: BTreeMap<i32, Stream>,
}

impl<'tx> StreamInserter<'tx> {
    fn new(tx: &'tx rusqlite::Transaction) -> Result<Self, Error> {
        let stmt = tx.prepare(r#"
            insert into stream (camera_id, type, rtsp_path, record, retain_bytes, next_recording_id)
                        values (:camera_id, :type, :rtsp_path, 0, 0, 1)
        "#)?;
        Ok(StreamInserter {
            tx,
            stmt,
            new_streams: BTreeMap::new(),
        })
    }

    fn add(&mut self, camera_id: i32, type_: StreamType, rtsp_path: String) -> Result<(), Error> {
        self.stmt.execute_named(&[
            (":camera_id", &camera_id),
            (":type", &type_.as_str()),
            (":rtsp_path", &rtsp_path)
        ])?;
        let id = self.tx.last_insert_rowid() as i32;
        self.new_streams.insert(id, Stream {
            id,
            type_,
            camera_id,
            rtsp_path,
            retain_bytes: 0,
            range: None,
            sample_file_bytes: 0,
            duration: recording::Duration(0),
            days: BTreeMap::new(),
            record: false,
            next_recording_id: 1,
        });
        Ok(())
    }

    fn streams(self) -> BTreeMap<i32, Stream> { self.new_streams }
}

impl LockedDatabase {
    /// Returns an immutable view of the cameras by id.
    pub fn cameras_by_id(&self) -> &BTreeMap<i32, Camera> { &self.state.cameras_by_id }
    pub fn streams_by_id(&self) -> &BTreeMap<i32, Stream> { &self.state.streams_by_id }

    /// Returns an immutable view of the video sample entries.
    pub fn video_sample_entries(&self) -> btree_map::Values<i32, Arc<VideoSampleEntry>> {
        self.state.video_sample_entries.values()
    }

    /// Starts a transaction for a write operation.
    /// Note transactions are not needed for read operations; this process holds a lock on the
    /// database directory, and the connection is locked within the process, so having a
    /// `LockedDatabase` is sufficient to ensure a consistent view.
    pub fn tx(&mut self) -> Result<Transaction, Error> {
        Ok(Transaction{
            state: &mut self.state,
            mods_by_stream: fnv::FnvHashMap::default(),
            tx: self.conn.transaction()?,
            must_rollback: false,
            bypass_reservation_for_testing: false,
        })
    }

    /// Gets a given camera by uuid.
    pub fn get_camera(&self, uuid: Uuid) -> Option<&Camera> {
        match self.state.cameras_by_uuid.get(&uuid) {
            Some(id) => Some(self.state.cameras_by_id.get(id).expect("uuid->id requires id->cam")),
            None => None,
        }
    }

    /// Lists the specified recordings in ascending order by start time, passing them to a supplied
    /// function. Given that the function is called with the database lock held, it should be quick.
    pub fn list_recordings_by_time<F>(&self, stream_id: i32, desired_time: Range<recording::Time>,
                                      f: F) -> Result<(), Error>
    where F: FnMut(ListRecordingsRow) -> Result<(), Error> {
        let mut stmt = self.conn.prepare_cached(&self.state.list_recordings_by_time_sql)?;
        let rows = stmt.query_named(&[
            (":stream_id", &stream_id),
            (":start_time_90k", &desired_time.start.0),
            (":end_time_90k", &desired_time.end.0)])?;
        self.list_recordings_inner(stream_id, rows, f)
    }

    /// Lists the specified recordigs in ascending order by id.
    pub fn list_recordings_by_id<F>(&self, stream_id: i32, desired_ids: Range<i32>, f: F)
                                    -> Result<(), Error>
    where F: FnMut(ListRecordingsRow) -> Result<(), Error> {
        let mut stmt = self.conn.prepare_cached(LIST_RECORDINGS_BY_ID_SQL)?;
        let rows = stmt.query_named(&[
            (":start", &composite_id(stream_id, desired_ids.start)),
            (":end", &composite_id(stream_id, desired_ids.end)),
        ])?;
        self.list_recordings_inner(stream_id, rows, f)
    }

    fn list_recordings_inner<F>(&self, stream_id: i32, mut rows: rusqlite::Rows, mut f: F)
                                -> Result<(), Error>
    where F: FnMut(ListRecordingsRow) -> Result<(), Error> {
        while let Some(row) = rows.next() {
            let row = row?;
            let id = row.get_checked::<_, i64>(0)? as i32;  // drop top bits of composite_id.
            let vse_id = row.get_checked(8)?;
            let video_sample_entry = match self.state.video_sample_entries.get(&vse_id) {
                Some(v) => v,
                None => {
                    return Err(Error::new(format!(
                        "recording {}/{} references nonexistent video_sample_entry {}",
                        stream_id, id, vse_id)));
                },
            };
            let out = ListRecordingsRow{
                stream_id,
                id,
                run_offset: row.get_checked(1)?,
                flags: row.get_checked(2)?,
                start: recording::Time(row.get_checked(3)?),
                duration_90k: row.get_checked(4)?,
                sample_file_bytes: row.get_checked(5)?,
                video_samples: row.get_checked(6)?,
                video_sync_samples: row.get_checked(7)?,
                video_sample_entry: video_sample_entry.clone(),
            };
            f(out)?;
        }
        Ok(())
    }

    /// Calls `list_recordings_by_time` and aggregates consecutive recordings.
    /// Rows are given to the callback in arbitrary order. Callers which care about ordering
    /// should do their own sorting.
    pub fn list_aggregated_recordings<F>(&self, stream_id: i32,
                                         desired_time: Range<recording::Time>,
                                         forced_split: recording::Duration,
                                         mut f: F) -> Result<(), Error>
    where F: FnMut(&ListAggregatedRecordingsRow) -> Result<(), Error> {
        // Iterate, maintaining a map from a recording_id to the aggregated row for the latest
        // batch of recordings from the run starting at that id. Runs can be split into multiple
        // batches for a few reasons:
        //
        // * forced split (when exceeding a duration limit)
        // * a missing id (one that was deleted out of order)
        // * video_sample_entry mismatch (if the parameters changed during a RTSP session)
        //
        // This iteration works because in a run, the start_time+duration of recording id r
        // is equal to the start_time of recording id r+1. Thus ascending times guarantees
        // ascending ids within a run. (Different runs, however, can be arbitrarily interleaved if
        // their timestamps overlap. Tracking all active runs prevents that interleaving from
        // causing problems.)
        let mut aggs: BTreeMap<i32, ListAggregatedRecordingsRow> = BTreeMap::new();
        self.list_recordings_by_time(stream_id, desired_time, |row| {
            let run_start_id = row.id - row.run_offset;
            let needs_flush = if let Some(a) = aggs.get(&run_start_id) {
                let new_dur = a.time.end - a.time.start +
                              recording::Duration(row.duration_90k as i64);
                a.ids.end != row.id || row.video_sample_entry.id != a.video_sample_entry.id ||
                   new_dur >= forced_split
            } else {
                false
            };
            if needs_flush {
                let a = aggs.remove(&run_start_id).expect("needs_flush when agg is None");
                f(&a)?;
            }
            let need_insert = if let Some(ref mut a) = aggs.get_mut(&run_start_id) {
                if a.time.end != row.start {
                    return Err(Error::new(format!(
                        "stream {} recording {} ends at {}; {} starts at {}; expected same",
                        stream_id, a.ids.end - 1, a.time.end, row.id, row.start)));
                }
                a.time.end.0 += row.duration_90k as i64;
                a.ids.end = row.id + 1;
                a.video_samples += row.video_samples as i64;
                a.video_sync_samples += row.video_sync_samples as i64;
                a.sample_file_bytes += row.sample_file_bytes as i64;
                false
            } else {
                true
            };
            if need_insert {
                aggs.insert(run_start_id, ListAggregatedRecordingsRow{
                        time: row.start ..  recording::Time(row.start.0 + row.duration_90k as i64),
                        ids: row.id .. row.id+1,
                        video_samples: row.video_samples as i64,
                        video_sync_samples: row.video_sync_samples as i64,
                        sample_file_bytes: row.sample_file_bytes as i64,
                        video_sample_entry: row.video_sample_entry,
                        stream_id,
                        run_start_id: row.id - row.run_offset,
                        flags: row.flags,
                });
            };
            Ok(())
        })?;
        for a in aggs.values() {
            f(a)?;
        }
        Ok(())
    }

    /// Calls `f` with a single `recording_playback` row.
    /// Note the lock is held for the duration of `f`.
    /// This uses a LRU cache to reduce the number of retrievals from the database.
    pub fn with_recording_playback<F, R>(&self, stream_id: i32, recording_id: i32, f: F)
                                      -> Result<R, Error>
    where F: FnOnce(&RecordingPlayback) -> Result<R, Error> {
        let composite_id = composite_id(stream_id, recording_id);
        let mut cache = self.state.playback_cache.borrow_mut();
        if let Some(r) = cache.get_mut(&composite_id) {
            trace!("cache hit for recording {}/{}", stream_id, recording_id);
            return f(&RecordingPlayback::new(r));
        }
        trace!("cache miss for recording {}/{}", stream_id, recording_id);
        let mut stmt = self.conn.prepare_cached(GET_RECORDING_PLAYBACK_SQL)?;
        let mut rows = stmt.query_named(&[(":composite_id", &composite_id)])?;
        if let Some(row) = rows.next() {
            let row = row?;
            let uuid: FromSqlUuid = row.get_checked(0)?;
            let data = {
                let mut data: PlaybackData = row.get_checked(1)?;
                data.0[0..16].copy_from_slice(uuid.0.as_bytes());
                data.0
            };
            let result = f(&RecordingPlayback::new(&data));
            cache.insert(composite_id, data);
            return result;
        }
        Err(Error::new(format!("no such recording {}/{}", stream_id, recording_id)))
    }

    /// Lists all reserved sample files.
    pub fn list_reserved_sample_files(&self) -> Result<Vec<Uuid>, Error> {
        let mut reserved = Vec::new();
        let mut stmt = self.conn.prepare_cached("select uuid from reserved_sample_files;")?;
        let mut rows = stmt.query_named(&[])?;
        while let Some(row) = rows.next() {
            let row = row?;
            let uuid: FromSqlUuid = row.get_checked(0)?;
            reserved.push(uuid.0);
        }
        Ok(reserved)
    }

    /// Lists the oldest sample files (to delete to free room).
    /// `f` should return true as long as further rows are desired.
    pub fn list_oldest_sample_files<F>(&self, stream_id: i32, mut f: F) -> Result<(), Error>
    where F: FnMut(ListOldestSampleFilesRow) -> bool {
        let mut stmt = self.conn.prepare_cached(LIST_OLDEST_SAMPLE_FILES_SQL)?;
        let mut rows = stmt.query_named(&[
            (":start", &composite_id(stream_id, 0)),
            (":end", &composite_id(stream_id + 1, 0)),
        ])?;
        while let Some(row) = rows.next() {
            let row = row?;
            let start = recording::Time(row.get_checked(2)?);
            let duration = recording::Duration(row.get_checked(3)?);
            let composite_id: i64 = row.get_checked(0)?;
            let uuid: FromSqlUuid = row.get_checked(1)?;
            let should_continue = f(ListOldestSampleFilesRow{
                recording_id: composite_id as i32,
                stream_id: (composite_id >> 32) as i32,
                uuid: uuid.0,
                time: start .. start + duration,
                sample_file_bytes: row.get_checked(4)?,
            });
            if !should_continue {
                break;
            }
        }
        Ok(())
    }

    /// Initializes the video_sample_entries. To be called during construction.
    fn init_video_sample_entries(&mut self) -> Result<(), Error> {
        info!("Loading video sample entries");
        let mut stmt = self.conn.prepare(r#"
            select
                id,
                sha1,
                width,
                height,
                data
            from
                video_sample_entry
        "#)?;
        let mut rows = stmt.query(&[])?;
        while let Some(row) = rows.next() {
            let row = row?;
            let id = row.get_checked(0)?;
            let mut sha1 = [0u8; 20];
            let sha1_vec: Vec<u8> = row.get_checked(1)?;
            if sha1_vec.len() != 20 {
                return Err(Error::new(format!(
                    "video sample entry id {} has sha1 {} of wrong length",
                    id, sha1_vec.len())));
            }
            sha1.copy_from_slice(&sha1_vec);
            let data: Vec<u8> = row.get_checked(4)?;

            // TODO: store this in the database rather than have codec-specific dispatch logic here.
            let rfc6381_codec = h264::rfc6381_codec_from_sample_entry(&data)?;

            self.state.video_sample_entries.insert(id, Arc::new(VideoSampleEntry {
                id: id as i32,
                width: row.get_checked::<_, i32>(2)? as u16,
                height: row.get_checked::<_, i32>(3)? as u16,
                sha1,
                data,
                rfc6381_codec,
            }));
        }
        info!("Loaded {} video sample entries",
              self.state.video_sample_entries.len());
        Ok(())
    }

    /// Initializes the cameras, but not their matching recordings.
    /// To be called during construction.
    fn init_cameras(&mut self) -> Result<(), Error> {
        info!("Loading cameras");
        let mut stmt = self.conn.prepare(r#"
            select
              id,
              uuid,
              short_name,
              description,
              host,
              username,
              password
            from
              camera;
        "#)?;
        let mut rows = stmt.query(&[])?;
        while let Some(row) = rows.next() {
            let row = row?;
            let id = row.get_checked(0)?;
            let uuid: FromSqlUuid = row.get_checked(1)?;
            self.state.cameras_by_id.insert(id, Camera {
                id: id,
                uuid: uuid.0,
                short_name: row.get_checked(2)?,
                description: row.get_checked(3)?,
                host: row.get_checked(4)?,
                username: row.get_checked(5)?,
                password: row.get_checked(6)?,
                streams: Default::default(),
            });
            self.state.cameras_by_uuid.insert(uuid.0, id);
        }
        info!("Loaded {} cameras", self.state.cameras_by_id.len());
        Ok(())
    }

    /// Initializes the streams, but not their matching recordings.
    /// To be called during construction.
    fn init_streams(&mut self) -> Result<(), Error> {
        info!("Loading streams");
        let mut stmt = self.conn.prepare(r#"
            select
              id,
              type,
              camera_id,
              rtsp_path,
              retain_bytes,
              next_recording_id,
              record
            from
              stream;
        "#)?;
        let mut rows = stmt.query(&[])?;
        while let Some(row) = rows.next() {
            let row = row?;
            let id = row.get_checked(0)?;
            let type_: String = row.get_checked(1)?;
            let type_ = StreamType::parse(&type_).ok_or_else(
                || Error::new(format!("no such stream type {}", type_)))?;
            let camera_id = row.get_checked(2)?;
            self.state.streams_by_id.insert(id, Stream {
                id,
                type_,
                camera_id,
                rtsp_path: row.get_checked(3)?,
                retain_bytes: row.get_checked(4)?,
                range: None,
                sample_file_bytes: 0,
                duration: recording::Duration(0),
                days: BTreeMap::new(),
                next_recording_id: row.get_checked(5)?,
                record: row.get_checked(6)?,
            });
            let c = self.state.cameras_by_id.get_mut(&camera_id)
                                            .ok_or_else(|| Error::new("missing camera".to_owned()))?;
            c.streams[type_.index()] = Some(id);
        }
        info!("Loaded {} streams", self.state.streams_by_id.len());
        Ok(())
    }

    /// Inserts the specified video sample entry if absent.
    /// On success, returns the id of a new or existing row.
    pub fn insert_video_sample_entry(&mut self, w: u16, h: u16, data: Vec<u8>,
                                     rfc6381_codec: String) -> Result<i32, Error> {
        let sha1 = hash::hash(hash::MessageDigest::sha1(), &data)?;
        let mut sha1_bytes = [0u8; 20];
        sha1_bytes.copy_from_slice(&sha1);

        // Check if it already exists.
        // There shouldn't be too many entries, so it's fine to enumerate everything.
        for (&id, v) in &self.state.video_sample_entries {
            if v.sha1 == sha1_bytes {
                // The width and height should match given that they're also specified within data
                // and thus included in the just-compared hash.
                if v.width != w || v.height != h {
                    return Err(Error::new(format!("database entry for {:?} is {}x{}, not {}x{}",
                                                  &sha1[..], v.width, v.height, w, h)));
                }
                return Ok(id);
            }
        }

        let mut stmt = self.conn.prepare_cached(INSERT_VIDEO_SAMPLE_ENTRY_SQL)?;
        stmt.execute_named(&[
            (":sha1", &&sha1_bytes[..]),
            (":width", &(w as i64)),
            (":height", &(h as i64)),
            (":data", &data),
        ])?;

        let id = self.conn.last_insert_rowid() as i32;
        self.state.video_sample_entries.insert(id, Arc::new(VideoSampleEntry {
            id: id,
            width: w,
            height: h,
            sha1: sha1_bytes,
            data: data,
            rfc6381_codec,
        }));

        Ok(id)
    }

    /// Adds a camera.
    pub fn add_camera(&mut self, mut camera: CameraChange) -> Result<i32, Error> {
        let uuid = Uuid::new_v4();
        let uuid_bytes = &uuid.as_bytes()[..];
        let tx = self.conn.transaction()?;
        let mut new_streams;
        let camera_id;
        {
            let mut stmt = tx.prepare_cached(r#"
                insert into camera (uuid, short_name, description, host, username, password)
                            values (:uuid, :short_name, :description, :host, :username, :password)
            "#)?;
            stmt.execute_named(&[
                (":uuid", &uuid_bytes),
                (":short_name", &camera.short_name),
                (":description", &camera.description),
                (":host", &camera.host),
                (":username", &camera.username),
                (":password", &camera.password),
            ])?;
            camera_id = tx.last_insert_rowid() as i32;
            let mut inserter = StreamInserter::new(&tx)?;
            for (i, ref mut rtsp_path) in camera.rtsp_paths.iter_mut().enumerate() {
                if rtsp_path.is_empty() { continue; }
                inserter.add(camera_id, StreamType::from_index(i).unwrap(),
                             mem::replace(rtsp_path, String::new()))?;
            }
            new_streams = inserter.streams();
        }
        tx.commit()?;
        let mut streams = [None, None];
        for (&id, s) in &new_streams {
            streams[s.type_.index()] = Some(id);
        }
        self.state.streams_by_id.append(&mut new_streams);
        self.state.cameras_by_id.insert(camera_id, Camera {
            id: camera_id,
            uuid,
            short_name: camera.short_name,
            description: camera.description,
            host: camera.host,
            username: camera.username,
            password: camera.password,
            streams,
        });
        self.state.cameras_by_uuid.insert(uuid, camera_id);
        Ok(camera_id)
    }

    /// Updates a camera.
    pub fn update_camera(&mut self, camera_id: i32, mut camera: CameraChange) -> Result<(), Error> {
        let tx = self.conn.transaction()?;
        let mut new_streams;
        let mut stream_rtsp_changes = BTreeMap::new();
        {
            let mut stream_ids = [None; 2];
            let mut stream_update_stmt = tx.prepare_cached(r#"
                update stream set
                    rtsp_path = :rtsp_path
                where
                    id = :id
            "#)?;
            for (&stream_id, stream) in &self.state.streams_by_id {
                if stream.camera_id != camera_id {
                    continue;
                }
                stream_ids[stream.type_.index()] = Some(stream_id);
                let p = mem::replace(&mut camera.rtsp_paths[stream.type_.index()], String::new());
                let rows = stream_update_stmt.execute_named(&[
                    (":id", &stream_id),
                    (":rtsp_path", &p),
                ])?;
                if rows != 1 {
                    return Err(Error::new(format!("Stream {} missing from database",
                                                  stream_id)));
                }
                stream_rtsp_changes.insert(stream_id, p);
            }
            let mut inserter = StreamInserter::new(&tx)?;
            for (index, id) in stream_ids.iter().enumerate() {
                if id.is_none() && !camera.rtsp_paths[index].is_empty() {
                    inserter.add(camera_id, StreamType::from_index(index).unwrap(),
                                 mem::replace(&mut camera.rtsp_paths[index], String::new()))?;
                }
            }
            new_streams = inserter.streams();
            let mut stmt = tx.prepare_cached(r#"
                update camera set
                    short_name = :short_name,
                    description = :description,
                    host = :host,
                    username = :username,
                    password = :password
                where
                    id = :id
            "#)?;
            let rows = stmt.execute_named(&[
                (":id", &camera_id),
                (":short_name", &camera.short_name),
                (":description", &camera.description),
                (":host", &camera.host),
                (":username", &camera.username),
                (":password", &camera.password),
            ])?;
            if rows != 1 {
                return Err(Error::new(format!("Camera {} missing from database", camera_id)));
            }
        }
        tx.commit()?;
        let c = self.state.cameras_by_id.get_mut(&camera_id).unwrap();
        c.short_name = camera.short_name;
        c.description = camera.description;
        c.host = camera.host;
        c.username = camera.username;
        c.password = camera.password;
        for (&id, s) in &new_streams {
            c.streams[s.type_.index()] = Some(id);
        }
        self.state.streams_by_id.append(&mut new_streams);
        for (id, p) in &mut stream_rtsp_changes {
            let mut s = self.state.streams_by_id.get_mut(id)
                            .ok_or_else(|| Error::new(format!("stream {} missing", id)))?;
            mem::swap(&mut s.rtsp_path, p);
        }
        Ok(())
    }

    /// Deletes a camera and its streams. The camera must have no recordings.
    pub fn delete_camera(&mut self, id: i32) -> Result<(), Error> {
        let uuid = self.state.cameras_by_id.get(&id)
                       .map(|c| c.uuid)
                       .ok_or_else(|| Error::new(format!("No such camera {} to remove", id)))?;
        let mut streams_to_delete = Vec::new();
        let tx = self.conn.transaction()?;
        {
            let mut stream_stmt = tx.prepare_cached(r"delete from stream where id = :id")?;
            for (stream_id, stream) in &self.state.streams_by_id {
                if stream.camera_id != id { continue };
                if stream.range.is_some() {
                    return Err(Error::new(format!("Can't remove camera {}; has recordings.", id)));
                }
                let rows = stream_stmt.execute_named(&[(":id", stream_id)])?;
                if rows != 1 {
                    return Err(Error::new(format!("Stream {} missing from database", id)));
                }
                streams_to_delete.push(*stream_id);
            }
            let mut cam_stmt = tx.prepare_cached(r"delete from camera where id = :id")?;
            let rows = cam_stmt.execute_named(&[(":id", &id)])?;
            if rows != 1 {
                return Err(Error::new(format!("Camera {} missing from database", id)));
            }
        }
        tx.commit()?;
        for id in streams_to_delete {
            self.state.streams_by_id.remove(&id);
        }
        self.state.cameras_by_uuid.remove(&uuid);
        return Ok(())
    }
}

/// Gets the schema version from the given database connection.
/// A fully initialized database will return `Ok(Some(version))` where `version` is an integer that
/// can be compared to `EXPECTED_VERSION`. An empty database will return `Ok(None)`. A partially
/// initialized database (in particular, one without a version row) will return some error.
pub fn get_schema_version(conn: &rusqlite::Connection) -> Result<Option<i32>, Error> {
    let ver_tables: i32 = conn.query_row_and_then(
        "select count(*) from sqlite_master where name = 'version'",
        &[], |row| row.get_checked(0))?;
    if ver_tables == 0 {
        return Ok(None);
    }
    Ok(Some(conn.query_row_and_then("select max(id) from version", &[], |row| row.get_checked(0))?))
}

/// The recording database. Abstracts away SQLite queries. Also maintains in-memory state
/// (loaded on startup, and updated on successful commit) to avoid expensive scans over the
/// recording table on common queries.
#[derive(Debug)]
pub struct Database(Mutex<LockedDatabase>);

impl Database {
    /// Creates the database from a caller-supplied SQLite connection.
    pub fn new(conn: rusqlite::Connection) -> Result<Database, Error> {
        conn.execute("pragma foreign_keys = on", &[])?;
        let list_recordings_by_time_sql = format!(r#"
            select
                recording.composite_id,
                recording.run_offset,
                recording.flags,
                recording.start_time_90k,
                recording.duration_90k,
                recording.sample_file_bytes,
                recording.video_samples,
                recording.video_sync_samples,
                recording.video_sample_entry_id
            from
                recording
            where
                stream_id = :stream_id and
                recording.start_time_90k > :start_time_90k - {} and
                recording.start_time_90k < :end_time_90k and
                recording.start_time_90k + recording.duration_90k > :start_time_90k
            order by
                recording.start_time_90k
        "#, recording::MAX_RECORDING_DURATION);
        {
            let ver = get_schema_version(&conn)?.ok_or_else(|| Error::new(
                    "no such table: version. \
                    \
                    If you are starting from an \
                    empty database, see README.md to complete the \
                    installation. If you are starting from a database \
                    that predates schema versioning, see guide/schema.md."
                    .to_owned()))?;
            if ver < EXPECTED_VERSION {
                return Err(Error::new(format!(
                            "Database schema version {} is too old (expected {}); \
                            see upgrade instructions in guide/upgrade.md.",
                            ver, EXPECTED_VERSION)));
            } else if ver > EXPECTED_VERSION {
                return Err(Error::new(format!(
                            "Database schema version {} is too new (expected {}); \
                            must use a newer binary to match.", ver,
                            EXPECTED_VERSION)));

            }
        }
        let db = Database(Mutex::new(LockedDatabase{
            conn: conn,
            state: State {
                cameras_by_id: BTreeMap::new(),
                cameras_by_uuid: BTreeMap::new(),
                streams_by_id: BTreeMap::new(),
                video_sample_entries: BTreeMap::new(),
                playback_cache: RefCell::new(LruCache::with_hasher(1024, Default::default())),
                list_recordings_by_time_sql: list_recordings_by_time_sql,
            },
        }));
        {
            let l = &mut *db.lock();
            l.init_video_sample_entries().annotate_err("init_video_sample_entries")?;
            l.init_cameras().annotate_err("init_cameras")?;
            l.init_streams().annotate_err("init_streams")?;
            for (&stream_id, ref mut stream) in &mut l.state.streams_by_id {
                // TODO: we could use one thread per stream if we had multiple db conns.
                let camera = l.state.cameras_by_id.get(&stream.camera_id).unwrap();
                init_recordings(&mut l.conn, stream_id, camera, stream)
                    .annotate_err("init_recordings")?;
            }
        }
        Ok(db)
    }

    /// Locks the database; the returned reference is the only way to perform (read or write)
    /// operations.
    pub fn lock(&self) -> MutexGuard<LockedDatabase> { self.0.lock() }

    /// For testing. Closes the database and return the connection. This allows verification that
    /// a newly opened database is in an acceptable state.
    #[cfg(test)]
    fn close(self) -> rusqlite::Connection {
        self.0.into_inner().conn
    }
}

#[cfg(test)]
mod tests {
    use core::cmp::Ord;
    use recording::{self, TIME_UNITS_PER_SEC};
    use rusqlite::Connection;
    use std::collections::BTreeMap;
    use std::error::Error as E;
    use std::fmt::Debug;
    use testutil;
    use super::*;
    use super::adjust_days;  // non-public.
    use uuid::Uuid;

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        let schema = include_str!("schema.sql");
        conn.execute_batch(schema).unwrap();
        conn
    }

    fn assert_no_recordings(db: &Database, uuid: Uuid) {
        let mut rows = 0;
        let mut camera_id = -1;
        {
            let db = db.lock();
            for row in db.cameras_by_id().values() {
                rows += 1;
                camera_id = row.id;
                assert_eq!(uuid, row.uuid);
                assert_eq!("test-camera", row.host);
                assert_eq!("foo", row.username);
                assert_eq!("bar", row.password);
                //assert_eq!("/main", row.main_rtsp_path);
                //assert_eq!("/sub", row.sub_rtsp_path);
                //assert_eq!(42, row.retain_bytes);
                //assert_eq!(None, row.range);
                //assert_eq!(recording::Duration(0), row.duration);
                //assert_eq!(0, row.sample_file_bytes);
            }
        }
        assert_eq!(1, rows);

        let stream_id = camera_id;  // TODO
        rows = 0;
        {
            let db = db.lock();
            let all_time = recording::Time(i64::min_value()) .. recording::Time(i64::max_value());
            db.list_recordings_by_time(stream_id, all_time, |_row| {
                rows += 1;
                Ok(())
            }).unwrap();
        }
        assert_eq!(0, rows);
    }

    fn assert_single_recording(db: &Database, stream_id: i32, r: &RecordingToInsert) {
        {
            let db = db.lock();
            let stream = db.streams_by_id().get(&stream_id).unwrap();
            assert_eq!(Some(r.time.clone()), stream.range);
            assert_eq!(r.sample_file_bytes as i64, stream.sample_file_bytes);
            assert_eq!(r.time.end - r.time.start, stream.duration);
            db.cameras_by_id().get(&stream.camera_id).unwrap();
        }

        // TODO(slamb): test that the days logic works correctly.

        let mut rows = 0;
        let mut recording_id = -1;
        {
            let db = db.lock();
            let all_time = recording::Time(i64::min_value()) .. recording::Time(i64::max_value());
            db.list_recordings_by_time(stream_id, all_time, |row| {
                rows += 1;
                recording_id = row.id;
                assert_eq!(r.time,
                           row.start .. row.start + recording::Duration(row.duration_90k as i64));
                assert_eq!(r.video_samples, row.video_samples);
                assert_eq!(r.video_sync_samples, row.video_sync_samples);
                assert_eq!(r.sample_file_bytes, row.sample_file_bytes);
                assert_eq!(row.video_sample_entry.rfc6381_codec, "avc1.4d0029");
                Ok(())
            }).unwrap();
        }
        assert_eq!(1, rows);

        rows = 0;
        db.lock().list_oldest_sample_files(stream_id, |row| {
            rows += 1;
            assert_eq!(recording_id, row.recording_id);
            assert_eq!(r.sample_file_uuid, row.uuid);
            assert_eq!(r.time, row.time);
            assert_eq!(r.sample_file_bytes, row.sample_file_bytes);
            true
        }).unwrap();
        assert_eq!(1, rows);

        // TODO: list_aggregated_recordings.
        // TODO: with_recording_playback.
    }

    fn assert_unsorted_eq<T>(mut a: Vec<T>, mut b: Vec<T>)
    where T: Debug + Ord {
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    #[test]
    fn test_adjust_days() {
        testutil::init();
        let mut m = BTreeMap::new();

        // Create a day.
        let test_time = recording::Time(130647162600000i64);  // 2015-12-31 23:59:00 (Pacific).
        let one_min = recording::Duration(60 * TIME_UNITS_PER_SEC);
        let two_min = recording::Duration(2 * 60 * TIME_UNITS_PER_SEC);
        let three_min = recording::Duration(3 * 60 * TIME_UNITS_PER_SEC);
        let four_min = recording::Duration(4 * 60 * TIME_UNITS_PER_SEC);
        let test_day1 = &StreamDayKey(*b"2015-12-31");
        let test_day2 = &StreamDayKey(*b"2016-01-01");
        adjust_days(test_time .. test_time + one_min, 1, &mut m);
        assert_eq!(1, m.len());
        assert_eq!(Some(&StreamDayValue{recordings: 1, duration: one_min}), m.get(test_day1));

        // Add to a day.
        adjust_days(test_time .. test_time + one_min, 1, &mut m);
        assert_eq!(1, m.len());
        assert_eq!(Some(&StreamDayValue{recordings: 2, duration: two_min}), m.get(test_day1));

        // Subtract from a day.
        adjust_days(test_time .. test_time + one_min, -1, &mut m);
        assert_eq!(1, m.len());
        assert_eq!(Some(&StreamDayValue{recordings: 1, duration: one_min}), m.get(test_day1));

        // Remove a day.
        adjust_days(test_time .. test_time + one_min, -1, &mut m);
        assert_eq!(0, m.len());

        // Create two days.
        adjust_days(test_time .. test_time + three_min, 1, &mut m);
        assert_eq!(2, m.len());
        assert_eq!(Some(&StreamDayValue{recordings: 1, duration: one_min}), m.get(test_day1));
        assert_eq!(Some(&StreamDayValue{recordings: 1, duration: two_min}), m.get(test_day2));

        // Add to two days.
        adjust_days(test_time .. test_time + three_min, 1, &mut m);
        assert_eq!(2, m.len());
        assert_eq!(Some(&StreamDayValue{recordings: 2, duration: two_min}), m.get(test_day1));
        assert_eq!(Some(&StreamDayValue{recordings: 2, duration: four_min}), m.get(test_day2));

        // Subtract from two days.
        adjust_days(test_time .. test_time + three_min, -1, &mut m);
        assert_eq!(2, m.len());
        assert_eq!(Some(&StreamDayValue{recordings: 1, duration: one_min}), m.get(test_day1));
        assert_eq!(Some(&StreamDayValue{recordings: 1, duration: two_min}), m.get(test_day2));

        // Remove two days.
        adjust_days(test_time .. test_time + three_min, -1, &mut m);
        assert_eq!(0, m.len());
    }

    #[test]
    fn test_day_bounds() {
        testutil::init();
        assert_eq!(StreamDayKey(*b"2017-10-10").bounds(),  // normal day (24 hrs)
                   recording::Time(135685692000000) .. recording::Time(135693468000000));
        assert_eq!(StreamDayKey(*b"2017-03-12").bounds(),  // spring forward (23 hrs)
                   recording::Time(134037504000000) .. recording::Time(134044956000000));
        assert_eq!(StreamDayKey(*b"2017-11-05").bounds(),  // fall back (25 hrs)
                   recording::Time(135887868000000) .. recording::Time(135895968000000));
    }

    #[test]
    fn test_no_version() {
        testutil::init();
        let e = Database::new(Connection::open_in_memory().unwrap()).unwrap_err();
        assert!(e.description().starts_with("no such table: version"));
    }

    #[test]
    fn test_version_too_old() {
        testutil::init();
        let c = setup_conn();
        c.execute_batch("delete from version; insert into version values (1, 0, '');").unwrap();
        let e = Database::new(c).unwrap_err();
        assert!(e.description().starts_with(
                "Database schema version 1 is too old (expected 2)"), "got: {:?}",
                e.description());
    }

    #[test]
    fn test_version_too_new() {
        testutil::init();
        let c = setup_conn();
        c.execute_batch("delete from version; insert into version values (3, 0, '');").unwrap();
        let e = Database::new(c).unwrap_err();
        assert!(e.description().starts_with(
                "Database schema version 3 is too new (expected 2)"), "got: {:?}", e.description());
    }

    /// Basic test of running some queries on a fresh database.
    #[test]
    fn test_fresh_db() {
        testutil::init();
        let conn = setup_conn();
        let db = Database::new(conn).unwrap();
        let db = db.lock();
        assert_eq!(0, db.cameras_by_id().values().count());
    }

    /// Basic test of the full lifecycle of recording. Does not exercise error cases.
    #[test]
    fn test_full_lifecycle() {
        testutil::init();
        let conn = setup_conn();
        let db = Database::new(conn).unwrap();
        let camera_id = { db.lock() }.add_camera(CameraChange {
            short_name: "testcam".to_owned(),
            description: "".to_owned(),
            host: "test-camera".to_owned(),
            username: "foo".to_owned(),
            password: "bar".to_owned(),
            rtsp_paths: [
                "/main".to_owned(),
                "/sub".to_owned(),
            ],
        }).unwrap();
        {
            let mut l = db.lock();
            let stream_id = l.cameras_by_id().get(&camera_id).unwrap().streams[0].unwrap();
            let mut tx = l.tx().unwrap();
            tx.update_retention(stream_id, true, 42).unwrap();
            tx.commit().unwrap();
        }
        let camera_uuid = { db.lock().cameras_by_id().get(&camera_id).unwrap().uuid };
        assert_no_recordings(&db, camera_uuid);

        // Closing and reopening the database should present the same contents.
        let conn = db.close();
        let db = Database::new(conn).unwrap();
        assert_no_recordings(&db, camera_uuid);

        assert_eq!(db.lock().list_reserved_sample_files().unwrap(), &[]);

        let (uuid_to_use, uuid_to_keep);
        {
            let mut db = db.lock();
            let mut tx = db.tx().unwrap();
            uuid_to_use = tx.reserve_sample_file().unwrap();
            uuid_to_keep = tx.reserve_sample_file().unwrap();
            tx.commit().unwrap();
        }

        assert_unsorted_eq(db.lock().list_reserved_sample_files().unwrap(),
                           vec![uuid_to_use, uuid_to_keep]);

        let vse_id = db.lock().insert_video_sample_entry(
            1920, 1080, include_bytes!("testdata/avc1").to_vec(),
            "avc1.4d0029".to_owned()).unwrap();
        assert!(vse_id > 0, "vse_id = {}", vse_id);

        // Inserting a recording should succeed and remove its uuid from the reserved table.
        let start = recording::Time(1430006400 * TIME_UNITS_PER_SEC);
        let stream_id = camera_id;  // TODO
        let recording = RecordingToInsert {
            stream_id,
            sample_file_bytes: 42,
            run_offset: 0,
            flags: 0,
            time: start .. start + recording::Duration(TIME_UNITS_PER_SEC),
            local_time_delta: recording::Duration(0),
            video_samples: 1,
            video_sync_samples: 1,
            video_sample_entry_id: vse_id,
            sample_file_uuid: uuid_to_use,
            video_index: [0u8; 100].to_vec(),
            sample_file_sha1: [0u8; 20],
        };
        {
            let mut db = db.lock();
            let mut tx = db.tx().unwrap();
            tx.insert_recording(&recording).unwrap();
            tx.commit().unwrap();
        }
        assert_unsorted_eq(db.lock().list_reserved_sample_files().unwrap(),
                           vec![uuid_to_keep]);

        // Queries should return the correct result (with caches update on insert).
        assert_single_recording(&db, stream_id, &recording);

        // Queries on a fresh database should return the correct result (with caches populated from
        // existing database contents rather than built on insert).
        let conn = db.close();
        let db = Database::new(conn).unwrap();
        assert_single_recording(&db, stream_id, &recording);

        // Deleting a recording should succeed, update the min/max times, and re-reserve the uuid.
        {
            let mut db = db.lock();
            let mut v = Vec::new();
            db.list_oldest_sample_files(stream_id, |r| { v.push(r); true }).unwrap();
            assert_eq!(1, v.len());
            let mut tx = db.tx().unwrap();
            tx.delete_recordings(&v).unwrap();
            tx.commit().unwrap();
        }
        assert_no_recordings(&db, camera_uuid);
        assert_unsorted_eq(db.lock().list_reserved_sample_files().unwrap(),
                           vec![uuid_to_use, uuid_to_keep]);
    }

    #[test]
    fn test_drop_tx() {
        testutil::init();
        let conn = setup_conn();
        let db = Database::new(conn).unwrap();
        let mut db = db.lock();
        {
            let mut tx = db.tx().unwrap();
            tx.reserve_sample_file().unwrap();
            // drop tx without committing.
        }

        // The dropped tx should have done nothing.
        assert_eq!(db.list_reserved_sample_files().unwrap(), &[]);

        // Following transactions should succeed.
        let uuid;
        {
            let mut tx = db.tx().unwrap();
            uuid = tx.reserve_sample_file().unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(db.list_reserved_sample_files().unwrap(), &[uuid]);
    }
}
