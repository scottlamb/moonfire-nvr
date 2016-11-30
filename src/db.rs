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
//!     SSD write samples.

// Suppress false positive warnings caused by using the word SQLite in a docstring.
// clippy thinks this is an identifier which should be enclosed in backticks.
#![allow(doc_markdown)]

use error::Error;
use fnv;
use lru_cache::LruCache;
use openssl::crypto::hash;
use recording::{self, TIME_UNITS_PER_SEC};
use rusqlite;
use serde::ser::{Serialize, Serializer};
use std::collections::BTreeMap;
use std::cell::RefCell;
use std::cmp;
use std::io::Write;
use std::ops::Range;
use std::str;
use std::string::String;
use std::sync::{Arc,Mutex,MutexGuard};
use std::vec::Vec;
use time;
use uuid::Uuid;

const GET_RECORDING_SQL: &'static str =
    "select sample_file_uuid, video_index from recording where id = :id";

const DELETE_RESERVATION_SQL: &'static str =
    "delete from reserved_sample_files where uuid = :uuid";

const INSERT_RESERVATION_SQL: &'static str = r#"
    insert into reserved_sample_files (uuid,  state)
                               values (:uuid, :state);
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
                            values (:sha1, :width, :height, :data);
"#;

const INSERT_RECORDING_SQL: &'static str = r#"
    insert into recording (camera_id, sample_file_bytes, start_time_90k,
                           duration_90k, local_time_delta_90k, video_samples,
                           video_sync_samples, video_sample_entry_id,
                           sample_file_uuid, sample_file_sha1, video_index)
                   values (:camera_id, :sample_file_bytes, :start_time_90k,
                           :duration_90k, :local_time_delta_90k,
                           :video_samples, :video_sync_samples,
                           :video_sample_entry_id, :sample_file_uuid,
                           :sample_file_sha1, :video_index);
"#;

const LIST_OLDEST_SAMPLE_FILES_SQL: &'static str = r#"
    select
      id,
      sample_file_uuid,
      start_time_90k,
      duration_90k,
      sample_file_bytes
    from
      recording
    where
      camera_id = :camera_id
    order by
      start_time_90k
"#;

const DELETE_RECORDING_SQL: &'static str = r#"
    delete from recording where id = :recording_id;
"#;

const CAMERA_MIN_START_SQL: &'static str = r#"
    select
      start_time_90k
    from
      recording
    where
      camera_id = :camera_id
    order by start_time_90k limit 1;
"#;

const CAMERA_MAX_START_SQL: &'static str = r#"
    select
      start_time_90k,
      duration_90k
    from
      recording
    where
      camera_id = :camera_id
    order by start_time_90k desc;
"#;

/// A concrete box derived from a ISO/IEC 14496-12 section 8.5.2 VisualSampleEntry box. Describes
/// the codec, width, height, etc.
#[derive(Debug)]
pub struct VideoSampleEntry {
    pub id: i32,
    pub width: u16,
    pub height: u16,
    pub sha1: [u8; 20],
    pub data: Vec<u8>,
}

/// A row used in `list_recordings`.
#[derive(Debug)]
pub struct ListCameraRecordingsRow {
    pub id: i64,
    pub start: recording::Time,

    /// This is a recording::Duration, but a single recording's duration fits into an i32.
    pub duration_90k: i32,
    pub video_samples: i32,
    pub video_sync_samples: i32,
    pub sample_file_bytes: i32,
    pub video_sample_entry: Arc<VideoSampleEntry>,
}

/// A row used in `list_aggregated_recordings`.
#[derive(Debug)]
pub struct ListAggregatedRecordingsRow {
    pub range: Range<recording::Time>,
    pub video_samples: i64,
    pub video_sync_samples: i64,
    pub sample_file_bytes: i64,
    pub video_sample_entry: Arc<VideoSampleEntry>,
}

/// Extra data about a recording, beyond what is returned by ListCameraRecordingsRow.
/// Retrieve with `get_recording`.
#[derive(Debug)]
pub struct ExtraRecording {
    pub sample_file_uuid: Uuid,
    pub video_index: Vec<u8>
}

/// A recording to pass to `insert_recording`.
#[derive(Debug)]
pub struct RecordingToInsert {
    pub camera_id: i32,
    pub sample_file_bytes: i32,
    pub time: Range<recording::Time>,
    pub local_time: recording::Time,
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
    pub camera_id: i32,
    pub recording_id: i64,
    pub time: Range<recording::Time>,
    pub sample_file_bytes: i32,
}

/// A calendar day in `YYYY-mm-dd` format.
#[derive(Copy, Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CameraDayKey([u8; 10]);

impl CameraDayKey {
    fn new(tm: time::Tm) -> Result<Self, Error> {
        let mut s = CameraDayKey([0u8; 10]);
        write!(&mut s.0[..], "{}", tm.strftime("%Y-%m-%d")?)?;
        Ok(s)
    }
}

impl Serialize for CameraDayKey {
    /// Serializes as a string, not as the default bytes.
    /// serde_json will only allow string keys for objects.
    fn serialize<S>(&self, serializer: &mut S) -> Result<(), S::Error> where S: Serializer {
        serializer.serialize_str(str::from_utf8(&self.0[..]).expect("days are always UTF-8"))
    }
}

/// In-memory state about a particular camera on a particular day.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CameraDayValue {
    /// The number of recordings that overlap with this day. Note that `adjust_day` automatically
    /// prunes days with 0 recordings.
    pub recordings: i64,

    /// The total duration recorded on this day. This can be 0; because frames' durations are taken
    /// from the time of the next frame, a recording that ends unexpectedly after a single frame
    /// will have 0 duration of that frame and thus the whole recording.
    pub duration: recording::Duration,
}

/// In-memory state about a camera.
#[derive(Debug, Serialize)]
pub struct Camera {
    pub id: i32,
    pub uuid: Uuid,
    pub short_name: String,
    pub description: String,
    pub host: String,
    pub username: String,
    pub password: String,
    pub main_rtsp_path: String,
    pub sub_rtsp_path: String,
    pub retain_bytes: i64,

    /// The time range of recorded data associated with this camera (minimum start time and maximum
    /// end time). `None` iff there are no recordings for this camera.
    #[serde(skip_serializing)]
    pub range: Option<Range<recording::Time>>,
    pub sample_file_bytes: i64,

    /// The total duration of recorded data. This may not be `range.end - range.start` due to
    /// gaps and overlap.
    pub duration: recording::Duration,

    /// Mapping of calendar day (in the server's time zone) to a summary of recordings on that day.
    pub days: BTreeMap<CameraDayKey, CameraDayValue>,
}

/// Adds `delta` to the day represented by `day` in the map `m`.
/// Inserts a map entry if absent; removes the entry if it has 0 entries on exit.
fn adjust_day(day: CameraDayKey, delta: CameraDayValue,
              m: &mut BTreeMap<CameraDayKey, CameraDayValue>) {
    enum Do {
        Insert,
        Remove,
        Nothing
    };
    let what_to_do = match m.get_mut(&day) {
        None => {
            Do::Insert
        },
        Some(ref mut v) => {
            v.recordings += delta.recordings;
            v.duration += delta.duration;
            if v.recordings == 0 { Do::Remove } else { Do::Nothing }
        },
    };
    match what_to_do {
        Do::Insert => { m.insert(day, delta); },
        Do::Remove => { m.remove(&day); },
        Do::Nothing => {},
    }
}

/// Adjusts the day map `m` to reflect the range of the given recording.
/// Note that the specified range may span two days. It will never span more because the maximum
/// length of a recording entry is less than a day (even a 23-hour "spring forward" day).
///
/// This function swallows/logs date formatting errors because they shouldn't happen and there's
/// not much that can be done about them. (The database operation has already gone through.)
fn adjust_days(r: Range<recording::Time>, sign: i64,
               m: &mut BTreeMap<CameraDayKey, CameraDayValue>) {
    // Find first day key.
    let mut my_tm = time::at(time::Timespec{sec: r.start.unix_seconds(), nsec: 0});
    let day = match CameraDayKey::new(my_tm) {
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
    let first_day_delta = CameraDayValue{
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
    let day = match CameraDayKey::new(my_tm) {
        Ok(d) => d,
        Err(ref e) => {
            error!("Unable to fill second day key from {:?}: {}; will ignore.", my_tm, e);
            return;
        }
    };
    let second_day_delta = CameraDayValue{
        recordings: sign,
        duration: recording::Duration(sign * (r.end.0 - boundary_90k)),
    };
    adjust_day(day, second_day_delta, m);
}

impl Camera {
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

/// Gets a uuid from the given SQLite row and column index.
fn get_uuid<I: rusqlite::RowIndex>(row: &rusqlite::Row, i: I) -> Result<Uuid, Error> {
    // TODO: avoid this extra allocation+copy into a Vec<u8>.
    // See <https://github.com/jgallagher/rusqlite/issues/158>.
    Ok(Uuid::from_bytes(row.get_checked::<_, Vec<u8>>(i)?.as_slice())?)
}

/// Initializes the recordings associated with the given camera.
fn init_recordings(conn: &mut rusqlite::Connection, camera_id: i32, camera: &mut Camera)
    -> Result<(), Error> {
    info!("Loading recordings for camera {}", camera.short_name);
    let mut stmt = conn.prepare(r#"
        select
          recording.start_time_90k,
          recording.duration_90k,
          recording.sample_file_bytes
        from
          recording
        where
          camera_id = :camera_id
    "#)?;
    let mut rows = stmt.query_named(&[(":camera_id", &camera_id)])?;
    let mut i = 0;
    while let Some(row) = rows.next() {
        let row = row?;
        let start = recording::Time(row.get_checked(0)?);
        let duration = recording::Duration(row.get_checked(1)?);
        let bytes = row.get_checked(2)?;
        camera.add_recording(start .. start + duration, bytes);
        i += 1;
    }
    info!("Loaded {} recordings for camera {}", i, camera.short_name);
    Ok(())
}

pub struct LockedDatabase {
    conn: rusqlite::Connection,
    state: State,
}

/// In-memory state from the database.
/// This is separated out of `LockedDatabase` so that `Transaction` can mutably borrow `state`
/// while its underlying `rusqlite::Transaction` is borrowing `conn`.
struct State {
    cameras_by_id: BTreeMap<i32, Camera>,
    cameras_by_uuid: BTreeMap<Uuid, i32>,
    video_sample_entries: BTreeMap<i32, Arc<VideoSampleEntry>>,
    list_recordings_sql: String,
    recording_cache: RefCell<LruCache<i64, Arc<ExtraRecording>, fnv::FnvBuildHasher>>,
}

/// A high-level transaction. This manages the SQLite transaction and the matching modification to
/// be applied to the in-memory state on successful commit.
pub struct Transaction<'a> {
    state: &'a mut State,
    mods_by_camera: fnv::FnvHashMap<i32, CameraModification>,
    tx: rusqlite::Transaction<'a>,

    /// True if due to an earlier error the transaction must be rolled back rather than committed.
    /// Insert and delete are two-part, requiring a delete from the `reserve_sample_files` table
    /// and an insert to the `recording` table (or vice versa). If the latter half fails, the
    /// former should be aborted as well. We could use savepoints (nested transactions) for this,
    /// but for simplicity we just require the entire transaction be rolled back.
    must_rollback: bool,

    /// Normally sample file uuids must be reserved prior to a recording being inserted.
    /// It's convenient in benchmarks though to allow the same segment to be inserted into the
    /// database many times, so this safety check can be disabled.
    pub bypass_reservation_for_testing: bool,
}

/// A modification to be done to a `Camera` after a `Transaction` is committed.
struct CameraModification {
    /// Add this to `camera.duration`. Thus, positive values indicate a net addition;
    /// negative values indicate a net subtraction.
    duration: recording::Duration,

    /// Add this to `camera.sample_file_bytes`.
    sample_file_bytes: i64,

    /// Add this to `camera.days`.
    days: BTreeMap<CameraDayKey, CameraDayValue>,

    /// Reset the Camera range to this value. This should be populated immediately prior to the
    /// commit.
    range: Option<Range<recording::Time>>,
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

    /// Deletes the given recordings from the `recording` table.
    /// Note they are not fully removed from the database; the uuids are transferred to the
    /// `reserved_sample_files` table. The caller should `unlink` the files, then remove the
    /// reservation.
    pub fn delete_recordings(&mut self, rows: &[ListOldestSampleFilesRow]) -> Result<(), Error> {
        let mut del = self.tx.prepare_cached(DELETE_RECORDING_SQL)?;
        let mut insert = self.tx.prepare_cached(INSERT_RESERVATION_SQL)?;

        self.check_must_rollback()?;
        self.must_rollback = true;
        for row in rows {
            let changes = del.execute_named(&[(":recording_id", &row.recording_id)])?;
            if changes != 1 {
                return Err(Error::new(format!("no such recording {} (camera {}, uuid {})",
                                              row.recording_id, row.camera_id, row.uuid)));
            }
            let uuid = &row.uuid.as_bytes()[..];
            insert.execute_named(&[
                (":uuid", &uuid),
                (":state", &(ReservationState::Deleting as i64))
            ])?;
            let mut m = Transaction::get_mods_by_camera(&mut self.mods_by_camera, row.camera_id);
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
    pub fn insert_recording(&mut self, r: &RecordingToInsert) -> Result<(), Error> {
        self.check_must_rollback()?;

        // Sanity checking.
        if r.time.end < r.time.start {
            return Err(Error::new(format!("end time {} must be >= start time {}",
                                          r.time.end, r.time.start)));
        }

        // Unreserve the sample file uuid and insert the recording row.
        if self.state.cameras_by_id.get_mut(&r.camera_id).is_none() {
            return Err(Error::new(format!("no such camera id {}", r.camera_id)));
        }
        let uuid = &r.sample_file_uuid.as_bytes()[..];
        {
            let mut stmt = self.tx.prepare_cached(DELETE_RESERVATION_SQL)?;
            let changes = stmt.execute_named(&[(":uuid", &uuid)])?;
            if changes != 1 && !self.bypass_reservation_for_testing {
                return Err(Error::new(format!("uuid {} is not reserved", r.sample_file_uuid)));
            }
        }
        self.must_rollback = true;
        {
            let mut stmt = self.tx.prepare_cached(INSERT_RECORDING_SQL)?;
            let sha1 = &r.sample_file_sha1[..];
            stmt.execute_named(&[
                (":camera_id", &(r.camera_id as i64)),
                (":sample_file_bytes", &r.sample_file_bytes),
                (":start_time_90k", &r.time.start.0),
                (":duration_90k", &(r.time.end.0 - r.time.start.0)),
                (":local_time_delta_90k", &(r.local_time.0 - r.time.start.0)),
                (":video_samples", &r.video_samples),
                (":video_sync_samples", &r.video_sync_samples),
                (":video_sample_entry_id", &r.video_sample_entry_id),
                (":sample_file_uuid", &uuid),
                (":sample_file_sha1", &sha1),
                (":video_index", &r.video_index),
            ])?;
        }
        self.must_rollback = false;
        let mut m = Transaction::get_mods_by_camera(&mut self.mods_by_camera, r.camera_id);
        m.duration += r.time.end - r.time.start;
        m.sample_file_bytes += r.sample_file_bytes as i64;
        adjust_days(r.time.clone(), 1, &mut m.days);
        Ok(())
    }

    /// Commits these changes, consuming the Transaction.
    pub fn commit(mut self) -> Result<(), Error> {
        self.check_must_rollback()?;
        self.precommit()?;
        self.tx.commit()?;
        for (&camera_id, m) in &self.mods_by_camera {
            let mut camera = self.state.cameras_by_id.get_mut(&camera_id)
                                 .expect("modified camera must exist");
            camera.duration += m.duration;
            camera.sample_file_bytes += m.sample_file_bytes;
            for (k, v) in &m.days {
                adjust_day(*k, *v, &mut camera.days);
            }
            camera.range = m.range.clone();
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

    /// Looks up an existing entry in `mods` for a given camera or makes+inserts an identity entry.
    fn get_mods_by_camera(mods: &mut fnv::FnvHashMap<i32, CameraModification>, camera_id: i32)
                          -> &mut CameraModification {
        mods.entry(camera_id).or_insert_with(|| {
            CameraModification{
                duration: recording::Duration(0),
                sample_file_bytes: 0,
                range: None,
                days: BTreeMap::new(),
            }
        })
    }

    /// Fills the `range` of each `CameraModification`. This is done prior to commit so that if the
    /// commit succeeds, there's no possibility that the correct state can't be retrieved.
    fn precommit(&mut self) -> Result<(), Error> {
        // Recompute start and end times for each camera.
        for (&camera_id, m) in &mut self.mods_by_camera {
            // The minimum is straightforward, taking advantage of the start_time_90k index.
            let mut stmt = self.tx.prepare_cached(CAMERA_MIN_START_SQL)?;
            let mut rows = stmt.query_named(&[(":camera_id", &camera_id)])?;
            let min_start = match rows.next() {
                Some(row) => recording::Time(row?.get_checked(0)?),
                None => continue,  // no data; leave m.range alone.
            };

            // There was a minimum, so there should be a maximum too. Calculating it is less
            // straightforward because recordings could overlap. All recordings starting in the
            // last MAX_RECORDING_DURATION must be examined in order to take advantage of the
            // start_time_90k index.
            let mut stmt = self.tx.prepare_cached(CAMERA_MAX_START_SQL)?;
            let mut rows = stmt.query_named(&[(":camera_id", &camera_id)])?;
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
                    return Err(Error::new(format!("missing max for camera {} which had min {}",
                                                  camera_id, min_start)));
                }
            };
            m.range = Some(min_start .. max_end);
        }
        Ok(())
    }
}

impl LockedDatabase {
    /// Returns an immutable view of the cameras by id.
    pub fn cameras_by_id(&self) -> &BTreeMap<i32, Camera> { &self.state.cameras_by_id }

    /// Starts a transaction for a write operation.
    /// Note transactions are not needed for read operations; this process holds a lock on the
    /// database directory, and the connection is locked within the process, so having a
    /// `LockedDatabase` is sufficient to ensure a consistent view.
    pub fn tx(&mut self) -> Result<Transaction, Error> {
        Ok(Transaction{
            state: &mut self.state,
            mods_by_camera: fnv::FnvHashMap::default(),
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

    /// Lists the specified recordings in ascending order, passing them to a supplied function.
    /// Given that the function is called with the database lock held, it should be quick.
    pub fn list_recordings<F>(&self, camera_id: i32, desired_time: &Range<recording::Time>,
                              mut f: F) -> Result<(), Error>
    where F: FnMut(ListCameraRecordingsRow) -> Result<(), Error> {
        let mut stmt = self.conn.prepare_cached(&self.state.list_recordings_sql)?;
        let mut rows = stmt.query_named(&[
            (":camera_id", &camera_id),
            (":start_time_90k", &desired_time.start.0),
            (":end_time_90k", &desired_time.end.0)])?;
        while let Some(row) = rows.next() {
            let row = row?;
            let id = row.get_checked(0)?;
            let vse_id = row.get_checked(6)?;
            let video_sample_entry = match self.state.video_sample_entries.get(&vse_id) {
                Some(v) => v,
                None => {
                    return Err(Error::new(format!(
                        "recording {} references nonexistent video_sample_entry {}", id, vse_id)));
                },
            };
            let out = ListCameraRecordingsRow{
                id: id,
                start: recording::Time(row.get_checked(1)?),
                duration_90k: row.get_checked(2)?,
                sample_file_bytes: row.get_checked(3)?,
                video_samples: row.get_checked(4)?,
                video_sync_samples: row.get_checked(5)?,
                video_sample_entry: video_sample_entry.clone(),
            };
            f(out)?;
        }
        Ok(())
    }

    /// Convenience method which calls `list_recordings` and aggregates consecutive recordings.
    pub fn list_aggregated_recordings<F>(&self, camera_id: i32,
                                         desired_time: &Range<recording::Time>,
                                         forced_split: recording::Duration,
                                         mut f: F) -> Result<(), Error>
    where F: FnMut(ListAggregatedRecordingsRow) -> Result<(), Error> {
        let mut agg: Option<ListAggregatedRecordingsRow> = None;
        self.list_recordings(camera_id, desired_time, |row| {
            let needs_flush = if let Some(ref a) = agg {
                let new_dur = a.range.end - a.range.start +
                              recording::Duration(row.duration_90k as i64);
                a.range.end != row.start ||
                   row.video_sample_entry.id != a.video_sample_entry.id || new_dur >= forced_split
            } else {
                false
            };
            if needs_flush {
                let a = agg.take().expect("needs_flush when agg is none");
                f(a)?;
            }
            match agg {
                None => {
                    agg = Some(ListAggregatedRecordingsRow{
                        range: row.start ..  recording::Time(row.start.0 + row.duration_90k as i64),
                        video_samples: row.video_samples as i64,
                        video_sync_samples: row.video_sync_samples as i64,
                        sample_file_bytes: row.sample_file_bytes as i64,
                        video_sample_entry: row.video_sample_entry,
                    });
                },
                Some(ref mut a) => {
                    a.range.end.0 += row.duration_90k as i64;
                    a.video_samples += row.video_samples as i64;
                    a.video_sync_samples += row.video_sync_samples as i64;
                    a.sample_file_bytes += row.sample_file_bytes as i64;
                }
            };
            Ok(())
        })?;
        if let Some(a) = agg {
            f(a)?;
        }
        Ok(())
    }

    /// Gets extra data about a single recording.
    /// This uses a LRU cache to reduce the number of retrievals from the database.
    pub fn get_recording(&self, recording_id: i64)
        -> Result<Arc<ExtraRecording>, Error> {
        let mut cache = self.state.recording_cache.borrow_mut();
        if let Some(r) = cache.get_mut(&recording_id) {
            debug!("cache hit for recording {}", recording_id);
            return Ok(r.clone());
        }
        debug!("cache miss for recording {}", recording_id);
        let mut stmt = self.conn.prepare_cached(GET_RECORDING_SQL)?;
        let mut rows = stmt.query_named(&[(":id", &recording_id)])?;
        if let Some(row) = rows.next() {
            let row = row?;
            let r = Arc::new(ExtraRecording{
                sample_file_uuid: get_uuid(&row, 0)?,
                video_index: row.get_checked(1)?,
            });
            cache.insert(recording_id, r.clone());
            return Ok(r);
        }
        Err(Error::new(format!("no such recording {}", recording_id)))
    }

    /// Lists all reserved sample files.
    pub fn list_reserved_sample_files(&self) -> Result<Vec<Uuid>, Error> {
        let mut reserved = Vec::new();
        let mut stmt = self.conn.prepare_cached("select uuid from reserved_sample_files;")?;
        let mut rows = stmt.query_named(&[])?;
        while let Some(row) = rows.next() {
            let row = row?;
            reserved.push(get_uuid(&row, 0)?);
        }
        Ok(reserved)
    }

    /// Lists the oldest sample files (to delete to free room).
    /// `f` should return true as long as further rows are desired.
    pub fn list_oldest_sample_files<F>(&self, camera_id: i32, mut f: F) -> Result<(), Error>
    where F: FnMut(ListOldestSampleFilesRow) -> bool {
        let mut stmt = self.conn.prepare_cached(LIST_OLDEST_SAMPLE_FILES_SQL)?;
        let mut rows = stmt.query_named(&[(":camera_id", &(camera_id as i64))])?;
        while let Some(row) = rows.next() {
            let row = row?;
            let start = recording::Time(row.get_checked(2)?);
            let duration = recording::Duration(row.get_checked(3)?);
            let should_continue = f(ListOldestSampleFilesRow{
                recording_id: row.get_checked(0)?,
                uuid: get_uuid(&row, 1)?,
                camera_id: camera_id,
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
            self.state.video_sample_entries.insert(id, Arc::new(VideoSampleEntry{
                id: id as i32,
                width: row.get_checked::<_, i32>(2)? as u16,
                height: row.get_checked::<_, i32>(3)? as u16,
                sha1: sha1,
                data: row.get_checked(4)?,
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
              camera.id,
              camera.uuid,
              camera.short_name,
              camera.description,
              camera.host,
              camera.username,
              camera.password,
              camera.main_rtsp_path,
              camera.sub_rtsp_path,
              camera.retain_bytes
            from
              camera;
        "#)?;
        let mut rows = stmt.query(&[])?;
        while let Some(row) = rows.next() {
            let row = row?;
            let id = row.get_checked(0)?;
            let uuid = get_uuid(&row, 1)?;
            self.state.cameras_by_id.insert(id, Camera{
                id: id,
                uuid: uuid,
                short_name: row.get_checked(2)?,
                description: row.get_checked(3)?,
                host: row.get_checked(4)?,
                username: row.get_checked(5)?,
                password: row.get_checked(6)?,
                main_rtsp_path: row.get_checked(7)?,
                sub_rtsp_path: row.get_checked(8)?,
                retain_bytes: row.get_checked(9)?,
                range: None,
                sample_file_bytes: 0,
                duration: recording::Duration(0),
                days: BTreeMap::new(),
            });
            self.state.cameras_by_uuid.insert(uuid, id);
        }
        info!("Loaded {} cameras", self.state.cameras_by_id.len());
        Ok(())
    }

    /// Inserts the specified video sample entry if absent.
    /// On success, returns the id of a new or existing row.
    pub fn insert_video_sample_entry(&mut self, w: u16, h: u16, data: &[u8]) -> Result<i32, Error> {
        let sha1 = hash::hash(hash::Type::SHA1, data)?;
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
            (":sha1", &sha1),
            (":width", &(w as i64)),
            (":height", &(h as i64)),
            (":data", &data),
        ])?;

        let id = self.conn.last_insert_rowid() as i32;
        self.state.video_sample_entries.insert(id, Arc::new(VideoSampleEntry{
            id: id,
            width: w,
            height: h,
            sha1: sha1_bytes,
            data: data.to_vec(),
        }));

        Ok(id)
    }
}

/// The recording database. Abstracts away SQLite queries. Also maintains in-memory state
/// (loaded on startup, and updated on successful commit) to avoid expensive scans over the
/// recording table on common queries.
pub struct Database(Mutex<LockedDatabase>);

impl Database {
    /// Creates the database from a caller-supplied SQLite connection.
    pub fn new(conn: rusqlite::Connection) -> Result<Database, Error> {
        let list_recordings_sql = format!(r#"
            select
                recording.id,
                recording.start_time_90k,
                recording.duration_90k,
                recording.sample_file_bytes,
                recording.video_samples,
                recording.video_sync_samples,
                recording.video_sample_entry_id
            from
                recording
            where
                camera_id = :camera_id and
                recording.start_time_90k > :start_time_90k - {} and
                recording.start_time_90k < :end_time_90k and
                recording.start_time_90k + recording.duration_90k > :start_time_90k
            order by
                recording.start_time_90k
        "#, recording::MAX_RECORDING_DURATION);
        let db = Database(Mutex::new(LockedDatabase{
            conn: conn,
            state: State{
                cameras_by_id: BTreeMap::new(),
                cameras_by_uuid: BTreeMap::new(),
                video_sample_entries: BTreeMap::new(),
                recording_cache: RefCell::new(LruCache::with_hasher(1024, Default::default())),
                list_recordings_sql: list_recordings_sql,
            },
        }));
        {
            let mut l = &mut *db.0.lock().unwrap();
            l.init_video_sample_entries().map_err(Error::annotator("init_video_sample_entries"))?;
            l.init_cameras().map_err(Error::annotator("init_cameras"))?;
            for (&camera_id, ref mut camera) in &mut l.state.cameras_by_id {
                // TODO: we could use one thread per camera if we had multiple db conns.
                init_recordings(&mut l.conn, camera_id, camera)
                    .map_err(Error::annotator("init_recordings"))?;
            }
        }
        Ok(db)
    }

    /// Locks the database; the returned reference is the only way to perform (read or write)
    /// operations.
    pub fn lock(&self) -> MutexGuard<LockedDatabase> { self.0.lock().unwrap() }

    /// For testing. Closes the database and return the connection. This allows verification that
    /// a newly opened database is in an acceptable state.
    #[cfg(test)]
    fn close(self) -> rusqlite::Connection {
        self.0.into_inner().unwrap().conn
    }
}

#[cfg(test)]
mod tests {
    extern crate test;

    use core::cmp::Ord;
    use recording::{self, TIME_UNITS_PER_SEC};
    use rusqlite::Connection;
    use std::collections::BTreeMap;
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

    fn setup_camera(conn: &Connection, uuid: Uuid, short_name: &str) -> i32 {
        let uuid_bytes = &uuid.as_bytes()[..];
        conn.execute_named(r#"
            insert into camera (uuid,  short_name,  description,  host,  username,  password,
                                main_rtsp_path,  sub_rtsp_path,  retain_bytes)
                        values (:uuid, :short_name, :description, :host, :username, :password,
                                :main_rtsp_path, :sub_rtsp_path, :retain_bytes)
        "#, &[
            (":uuid", &uuid_bytes),
            (":short_name", &short_name),
            (":description", &""),
            (":host", &"test-camera"),
            (":username", &"foo"),
            (":password", &"bar"),
            (":main_rtsp_path", &"/main"),
            (":sub_rtsp_path", &"/sub"),
            (":retain_bytes", &42i64),
        ]).unwrap();
        conn.last_insert_rowid() as i32
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
                assert_eq!("/main", row.main_rtsp_path);
                assert_eq!("/sub", row.sub_rtsp_path);
                assert_eq!(42, row.retain_bytes);
                assert_eq!(None, row.range);
                assert_eq!(recording::Duration(0), row.duration);
                assert_eq!(0, row.sample_file_bytes);
            }
        }
        assert_eq!(1, rows);

        rows = 0;
        {
            let db = db.lock();
            let all_time = recording::Time(i64::min_value()) .. recording::Time(i64::max_value());
            db.list_recordings(camera_id, &all_time, |_row| {
                rows += 1;
                Ok(())
            }).unwrap();
        }
        assert_eq!(0, rows);
    }

    fn assert_single_recording(db: &Database, camera_uuid: Uuid, r: &RecordingToInsert) {
        let mut rows = 0;
        let mut camera_id = -1;
        {
            let db = db.lock();
            for row in db.cameras_by_id().values() {
                rows += 1;
                camera_id = row.id;
                assert_eq!(camera_uuid, row.uuid);
                assert_eq!(Some(r.time.clone()), row.range);
                assert_eq!(r.sample_file_bytes as i64, row.sample_file_bytes);
                assert_eq!(r.time.end - r.time.start, row.duration);
            }
        }
        assert_eq!(1, rows);

        // TODO(slamb): test that the days logic works correctly.

        rows = 0;
        let mut recording_id = -1;
        {
            let db = db.lock();
            let all_time = recording::Time(i64::min_value()) .. recording::Time(i64::max_value());
            db.list_recordings(camera_id, &all_time, |row| {
                rows += 1;
                recording_id = row.id;
                assert_eq!(r.time,
                           row.start .. row.start + recording::Duration(row.duration_90k as i64));
                assert_eq!(r.video_samples, row.video_samples);
                assert_eq!(r.video_sync_samples, row.video_sync_samples);
                assert_eq!(r.sample_file_bytes, row.sample_file_bytes);
                Ok(())
            }).unwrap();
        }
        assert_eq!(1, rows);

        rows = 0;
        db.lock().list_oldest_sample_files(camera_id, |row| {
            rows += 1;
            assert_eq!(recording_id, row.recording_id);
            assert_eq!(r.sample_file_uuid, row.uuid);
            assert_eq!(r.time, row.time);
            assert_eq!(r.sample_file_bytes, row.sample_file_bytes);
            true
        }).unwrap();
        assert_eq!(1, rows);

        // TODO: get_recording.
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
        let test_day1 = &CameraDayKey(*b"2015-12-31");
        let test_day2 = &CameraDayKey(*b"2016-01-01");
        adjust_days(test_time .. test_time + one_min, 1, &mut m);
        assert_eq!(1, m.len());
        assert_eq!(Some(&CameraDayValue{recordings: 1, duration: one_min}), m.get(test_day1));

        // Add to a day.
        adjust_days(test_time .. test_time + one_min, 1, &mut m);
        assert_eq!(1, m.len());
        assert_eq!(Some(&CameraDayValue{recordings: 2, duration: two_min}), m.get(test_day1));

        // Subtract from a day.
        adjust_days(test_time .. test_time + one_min, -1, &mut m);
        assert_eq!(1, m.len());
        assert_eq!(Some(&CameraDayValue{recordings: 1, duration: one_min}), m.get(test_day1));

        // Remove a day.
        adjust_days(test_time .. test_time + one_min, -1, &mut m);
        assert_eq!(0, m.len());

        // Create two days.
        adjust_days(test_time .. test_time + three_min, 1, &mut m);
        assert_eq!(2, m.len());
        assert_eq!(Some(&CameraDayValue{recordings: 1, duration: one_min}), m.get(test_day1));
        assert_eq!(Some(&CameraDayValue{recordings: 1, duration: two_min}), m.get(test_day2));

        // Add to two days.
        adjust_days(test_time .. test_time + three_min, 1, &mut m);
        assert_eq!(2, m.len());
        assert_eq!(Some(&CameraDayValue{recordings: 2, duration: two_min}), m.get(test_day1));
        assert_eq!(Some(&CameraDayValue{recordings: 2, duration: four_min}), m.get(test_day2));

        // Subtract from two days.
        adjust_days(test_time .. test_time + three_min, -1, &mut m);
        assert_eq!(2, m.len());
        assert_eq!(Some(&CameraDayValue{recordings: 1, duration: one_min}), m.get(test_day1));
        assert_eq!(Some(&CameraDayValue{recordings: 1, duration: two_min}), m.get(test_day2));

        // Remove two days.
        adjust_days(test_time .. test_time + three_min, -1, &mut m);
        assert_eq!(0, m.len());
    }

    /// Basic test of running some queries on an empty database.
    #[test]
    fn test_empty_db() {
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
        let camera_uuid = Uuid::new_v4();
        let camera_id = setup_camera(&conn, camera_uuid, "testcam");
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

        let vse_id = db.lock().insert_video_sample_entry(768, 512, &[0u8; 100]).unwrap();
        assert!(vse_id > 0, "vse_id = {}", vse_id);

        // Inserting a recording should succeed and remove its uuid from the reserved table.
        let start = recording::Time(1430006400 * TIME_UNITS_PER_SEC);
        let recording = RecordingToInsert{
            camera_id: camera_id,
            sample_file_bytes: 42,
            time: start .. start + recording::Duration(TIME_UNITS_PER_SEC),
            local_time: start,
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
        assert_single_recording(&db, camera_uuid, &recording);

        // Queries on a fresh database should return the correct result (with caches populated from
        // existing database contents rather than built on insert).
        let conn = db.close();
        let db = Database::new(conn).unwrap();
        assert_single_recording(&db, camera_uuid, &recording);

        // Deleting a recording should succeed, update the min/max times, and re-reserve the uuid.
        {
            let mut db = db.lock();
            let mut v = Vec::new();
            db.list_oldest_sample_files(camera_id, |r| { v.push(r); true }).unwrap();
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
