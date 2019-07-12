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
//!   * adding and removing recordings done during normal operations use a batch interface.
//!     A list of mutations is built up in-memory and occasionally flushed to reduce SSD write
//!     cycles.

use base::clock::{self, Clocks};
use crate::auth;
use crate::dir;
use crate::raw;
use crate::recording::{self, TIME_UNITS_PER_SEC};
use crate::schema;
use crate::signal;
use failure::{Error, bail, format_err};
use fnv::{FnvHashMap, FnvHashSet};
use itertools::Itertools;
use log::{error, info, trace};
use lru_cache::LruCache;
use openssl::hash;
use parking_lot::{Mutex,MutexGuard};
use protobuf::prelude::MessageField;
use rusqlite::types::ToSql;
use smallvec::SmallVec;
use std::collections::{BTreeMap, VecDeque};
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
pub const EXPECTED_VERSION: i32 = 5;

const GET_RECORDING_PLAYBACK_SQL: &'static str = r#"
    select
      video_index
    from
      recording_playback
    where
      composite_id = :composite_id
"#;

const INSERT_VIDEO_SAMPLE_ENTRY_SQL: &'static str = r#"
    insert into video_sample_entry (sha1,  width,  height,  rfc6381_codec, data)
                            values (:sha1, :width, :height, :rfc6381_codec, :data)
"#;

const UPDATE_NEXT_RECORDING_ID_SQL: &'static str =
    "update stream set next_recording_id = :next_recording_id where id = :stream_id";

pub struct FromSqlUuid(pub Uuid);

impl rusqlite::types::FromSql for FromSqlUuid {
    fn column_result(value: rusqlite::types::ValueRef) -> rusqlite::types::FromSqlResult<Self> {
        let uuid = Uuid::from_slice(value.as_blob()?)
            .map_err(|e| rusqlite::types::FromSqlError::Other(Box::new(e)))?;
        Ok(FromSqlUuid(uuid))
    }
}

struct VideoIndex(Box<[u8]>);

impl rusqlite::types::FromSql for VideoIndex {
    fn column_result(value: rusqlite::types::ValueRef) -> rusqlite::types::FromSqlResult<Self> {
        Ok(VideoIndex(value.as_blob()?.to_vec().into_boxed_slice()))
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
    pub video_sample_entry_id: i32,

    pub id: CompositeId,

    /// This is a recording::Duration, but a single recording's duration fits into an i32.
    pub duration_90k: i32,
    pub video_samples: i32,
    pub video_sync_samples: i32,
    pub sample_file_bytes: i32,
    pub run_offset: i32,
    pub open_id: u32,
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
    pub video_sample_entry_id: i32,
    pub stream_id: i32,
    pub run_start_id: i32,
    pub open_id: u32,
    pub first_uncommitted: Option<i32>,
    pub growing: bool,
}

impl ListAggregatedRecordingsRow {
    fn from(row: ListRecordingsRow) -> Self {
        let recording_id = row.id.recording();
        let uncommitted = (row.flags & RecordingFlags::Uncommitted as i32) != 0;
        let growing = (row.flags & RecordingFlags::Growing as i32) != 0;
        ListAggregatedRecordingsRow {
            time: row.start ..  recording::Time(row.start.0 + row.duration_90k as i64),
            ids: recording_id .. recording_id+1,
            video_samples: row.video_samples as i64,
            video_sync_samples: row.video_sync_samples as i64,
            sample_file_bytes: row.sample_file_bytes as i64,
            video_sample_entry_id: row.video_sample_entry_id,
            stream_id: row.id.stream(),
            run_start_id: recording_id - row.run_offset,
            open_id: row.open_id,
            first_uncommitted: if uncommitted { Some(recording_id) } else { None },
            growing,
        }
    }
}

/// Select fields from the `recordings_playback` table. Retrieve with `with_recording_playback`.
#[derive(Debug)]
pub struct RecordingPlayback<'a> {
    pub video_index: &'a [u8],
}

/// Bitmask in the `flags` field in the `recordings` table; see `schema.sql`.
pub enum RecordingFlags {
    TrailingZero = 1,

    // These values (starting from high bit on down) are never written to the database.
    Growing = 1 << 30,
    Uncommitted = 1 << 31,
}

/// A recording to pass to `insert_recording`.
#[derive(Clone, Debug, Default)]
pub struct RecordingToInsert {
    pub run_offset: i32,
    pub flags: i32,
    pub sample_file_bytes: i32,
    pub start: recording::Time,
    pub duration_90k: i32,  // a recording::Duration, but guaranteed to fit in i32.
    pub local_time_delta: recording::Duration,
    pub video_samples: i32,
    pub video_sync_samples: i32,
    pub video_sample_entry_id: i32,
    pub video_index: Vec<u8>,
    pub sample_file_sha1: [u8; 20],
}

impl RecordingToInsert {
    fn to_list_row(&self, id: CompositeId, open_id: u32) -> ListRecordingsRow {
        ListRecordingsRow {
            start: self.start,
            video_sample_entry_id: self.video_sample_entry_id,
            id,
            duration_90k: self.duration_90k,
            video_samples: self.video_samples,
            video_sync_samples: self.video_sync_samples,
            sample_file_bytes: self.sample_file_bytes,
            run_offset: self.run_offset,
            open_id,
            flags: self.flags | RecordingFlags::Uncommitted as i32,
        }
    }
}


/// A row used in `raw::list_oldest_recordings` and `db::delete_oldest_recordings`.
#[derive(Copy, Clone, Debug)]
pub(crate) struct ListOldestRecordingsRow {
    pub id: CompositeId,
    pub start: recording::Time,
    pub duration: i32,
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

#[derive(Debug)]
pub struct SampleFileDir {
    pub id: i32,
    pub path: String,
    pub uuid: Uuid,
    dir: Option<Arc<dir::SampleFileDir>>,
    last_complete_open: Option<Open>,

    /// ids which are in the `garbage` database table (rather than `recording`) as of last commit
    /// but may still exist on disk. These can't be safely removed from the database yet.
    pub(crate) garbage_needs_unlink: FnvHashSet<CompositeId>,

    /// ids which are in the `garbage` database table and are guaranteed to no longer exist on
    /// disk (have been unlinked and the dir has been synced). These may be removed from the
    /// database on next flush. Mutually exclusive with `garbage_needs_unlink`.
    pub(crate) garbage_unlinked: Vec<CompositeId>,
}

impl SampleFileDir {
    /// Returns a cloned copy of the directory, or Err if closed.
    ///
    /// Use `LockedDatabase::open_sample_file_dirs` prior to calling this method.
    pub fn get(&self) -> Result<Arc<dir::SampleFileDir>, Error> {
        Ok(self.dir
               .as_ref()
               .ok_or_else(|| format_err!("sample file dir {} is closed", self.id))?
               .clone())
    }

    /// Returns expected existing metadata when opening this directory.
    fn meta(&self, db_uuid: &Uuid) -> schema::DirMeta {
        let mut meta = schema::DirMeta::default();
        meta.db_uuid.extend_from_slice(&db_uuid.as_bytes()[..]);
        meta.dir_uuid.extend_from_slice(&self.uuid.as_bytes()[..]);
        if let Some(o) = self.last_complete_open {
            let open = meta.last_complete_open.mut_message();
            open.id = o.id;
            open.uuid.extend_from_slice(&o.uuid.as_bytes()[..]);
        }
        meta
    }
}

pub use crate::auth::Request;
pub use crate::auth::RawSessionId;
pub use crate::auth::Session;
pub use crate::auth::User;
pub use crate::auth::UserChange;

/// In-memory state about a camera.
#[derive(Debug)]
pub struct Camera {
    pub id: i32,
    pub uuid: Uuid,
    pub short_name: String,
    pub description: String,
    pub onvif_host: String,
    pub username: String,
    pub password: String,
    pub streams: [Option<i32>; 2],
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
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

impl ::std::fmt::Display for StreamType {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> Result<(), ::std::fmt::Error> {
        f.write_str(self.as_str())
    }
}

pub const ALL_STREAM_TYPES: [StreamType; 2] = [StreamType::MAIN, StreamType::SUB];

pub struct Stream {
    pub id: i32,
    pub camera_id: i32,
    pub sample_file_dir_id: Option<i32>,
    pub type_: StreamType,
    pub rtsp_url: String,
    pub retain_bytes: i64,
    pub flush_if_sec: i64,

    /// The time range of recorded data associated with this stream (minimum start time and maximum
    /// end time). `None` iff there are no recordings for this camera.
    pub range: Option<Range<recording::Time>>,
    pub sample_file_bytes: i64,

    /// On flush, delete the following recordings (move them to the `garbage` table, to be
    /// collected later). Note they must be the oldest recordings. The later collection involves
    /// the syncer unlinking the files on disk and syncing the directory then enqueueing for
    /// another following flush removal from the `garbage` table.
    to_delete: Vec<ListOldestRecordingsRow>,

    /// The total bytes to delete with the next flush.
    pub bytes_to_delete: i64,

    /// The total bytes to add with the next flush. (`mark_synced` has already been called on these
    /// recordings.)
    pub bytes_to_add: i64,

    /// The total duration of recorded data. This may not be `range.end - range.start` due to
    /// gaps and overlap.
    pub duration: recording::Duration,

    /// Mapping of calendar day (in the server's time zone) to a summary of recordings on that day.
    pub days: BTreeMap<StreamDayKey, StreamDayValue>,
    pub record: bool,

    /// The `next_recording_id` currently committed to the database.
    pub(crate) next_recording_id: i32,

    /// The recordings which have been added via `LockedDatabase::add_recording` but have yet to
    /// committed to the database.
    ///
    /// `uncommitted[i]` uses sample filename `CompositeId::new(id, next_recording_id + 1)`;
    /// `next_recording_id` should be advanced when one is committed to maintain this invariant.
    ///
    /// TODO: alter the serving path to show these just as if they were already committed.
    uncommitted: VecDeque<Arc<Mutex<RecordingToInsert>>>,

    /// The number of recordings in `uncommitted` which are synced and ready to commit.
    synced_recordings: usize,

    on_live_segment: Vec<Box<dyn FnMut(LiveSegment) -> bool + Send>>,
}

/// Bounds of a single keyframe and the frames dependent on it.
/// This is used for live stream recordings. The stream id should already be known to the
/// subscriber.
#[derive(Clone, Debug)]
pub struct LiveSegment {
    pub recording: i32,

    /// The pts, relative to the start of the recording, of the start and end of this live segment,
    /// in 90kHz units.
    pub off_90k: Range<i32>,
}

#[derive(Clone, Debug, Default)]
pub struct StreamChange {
    pub sample_file_dir_id: Option<i32>,
    pub rtsp_url: String,
    pub record: bool,
    pub flush_if_sec: i64,
}

/// Information about a camera, used by `add_camera` and `update_camera`.
#[derive(Clone, Debug)]
pub struct CameraChange {
    pub short_name: String,
    pub description: String,
    pub onvif_host: String,
    pub username: String,
    pub password: String,

    /// `StreamType t` is represented by `streams[t.index()]`. A default StreamChange will
    /// correspond to no stream in the database, provided there are no existing recordings for that
    /// stream.
    pub streams: [StreamChange; 2],
}

/// Adds non-zero `delta` to the day represented by `day` in the map `m`.
/// Inserts a map entry if absent; removes the entry if it has 0 entries on exit.
fn adjust_day(day: StreamDayKey, delta: StreamDayValue,
              m: &mut BTreeMap<StreamDayKey, StreamDayValue>) {
    use ::std::collections::btree_map::Entry;
    match m.entry(day) {
        Entry::Vacant(e) => { e.insert(delta); },
        Entry::Occupied(mut e) => {
            let v = e.get_mut();
            v.recordings += delta.recordings;
            v.duration += delta.duration;
            if v.recordings == 0 {
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
    let first_day_delta = StreamDayValue {
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
    let second_day_delta = StreamDayValue {
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
    while let Some(row) = rows.next()? {
        let start = recording::Time(row.get(0)?);
        let duration = recording::Duration(row.get(1)?);
        let bytes = row.get(2)?;
        stream.add_recording(start .. start + duration, bytes);
        i += 1;
    }
    info!("Loaded {} recordings for camera {} stream {:?}", i, camera.short_name, stream.type_);
    Ok(())
}

pub struct LockedDatabase {
    conn: rusqlite::Connection,
    uuid: Uuid,
    flush_count: usize,

    /// If the database is open in read-write mode, the information about the current Open row.
    pub open: Option<Open>,

    /// The monotonic time when the database was opened (whether in read-write mode or read-only
    /// mode).
    open_monotonic: recording::Time,

    auth: auth::State,
    signal: signal::State,

    sample_file_dirs_by_id: BTreeMap<i32, SampleFileDir>,
    cameras_by_id: BTreeMap<i32, Camera>,
    streams_by_id: BTreeMap<i32, Stream>,
    cameras_by_uuid: BTreeMap<Uuid, i32>,  // values are ids.
    video_sample_entries_by_id: BTreeMap<i32, Arc<VideoSampleEntry>>,
    video_index_cache: RefCell<LruCache<i64, Box<[u8]>, fnv::FnvBuildHasher>>,
    on_flush: Vec<Box<dyn Fn() + Send>>,
}

/// Represents a row of the `open` database table.
#[derive(Copy, Clone, Debug)]
pub struct Open {
    pub id: u32,
    pub(crate) uuid: Uuid,
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct CompositeId(pub i64);

impl CompositeId {
    pub fn new(stream_id: i32, recording_id: i32) -> Self {
        CompositeId((stream_id as i64) << 32 | recording_id as i64)
    }

    pub fn stream(self) -> i32 { (self.0 >> 32) as i32 }
    pub fn recording(self) -> i32 { self.0 as i32 }
}

impl ::std::fmt::Display for CompositeId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> Result<(), ::std::fmt::Error> {
        write!(f, "{}/{}", self.stream(), self.recording())
    }
}

/// Inserts, updates, or removes streams in the `State` object to match a set of `StreamChange`
/// structs.
struct StreamStateChanger {
    sids: [Option<i32>; 2],
    streams: Vec<(i32, Option<(i32, StreamType, StreamChange)>)>,
}

impl StreamStateChanger {
    /// Performs the database updates (guarded by the given transaction) and returns the state
    /// change to be applied on successful commit.
    fn new(tx: &rusqlite::Transaction, camera_id: i32, existing: Option<&Camera>,
           streams_by_id: &BTreeMap<i32, Stream>, change: &mut CameraChange)
           -> Result<Self, Error> {
        let mut sids = [None; 2];
        let mut streams = Vec::with_capacity(2);
        let existing_streams = existing.map(|e| e.streams).unwrap_or_default();
        for (i, ref mut sc) in change.streams.iter_mut().enumerate() {
            let type_ = StreamType::from_index(i).unwrap();
            let mut have_data = false;
            if let Some(sid) = existing_streams[i] {
                let s = streams_by_id.get(&sid).unwrap();
                if s.range.is_some() {
                    have_data = true;
                    if let (Some(d), false) = (s.sample_file_dir_id,
                                               s.sample_file_dir_id == sc.sample_file_dir_id) {
                        bail!("can't change sample_file_dir_id {:?}->{:?} for non-empty stream {}",
                              d, sc.sample_file_dir_id, sid);
                    }
                }
                if !have_data && sc.rtsp_url.is_empty() && sc.sample_file_dir_id.is_none() &&
                   !sc.record {
                    // Delete stream.
                    let mut stmt = tx.prepare_cached(r#"
                        delete from stream where id = ?
                    "#)?;
                    if stmt.execute(&[&sid])? != 1 {
                        bail!("missing stream {}", sid);
                    }
                    streams.push((sid, None));
                } else {
                    // Update stream.
                    let mut stmt = tx.prepare_cached(r#"
                        update stream set
                            rtsp_url = :rtsp_url,
                            record = :record,
                            flush_if_sec = :flush_if_sec,
                            sample_file_dir_id = :sample_file_dir_id
                        where
                            id = :id
                    "#)?;
                    let rows = stmt.execute_named(&[
                        (":rtsp_url", &sc.rtsp_url),
                        (":record", &sc.record),
                        (":flush_if_sec", &sc.flush_if_sec),
                        (":sample_file_dir_id", &sc.sample_file_dir_id),
                        (":id", &sid),
                    ])?;
                    if rows != 1 {
                        bail!("missing stream {}", sid);
                    }
                    sids[i] = Some(sid);
                    let sc = mem::replace(*sc, StreamChange::default());
                    streams.push((sid, Some((camera_id, type_, sc))));
                }
            } else {
                if sc.rtsp_url.is_empty() && sc.sample_file_dir_id.is_none() && !sc.record {
                    // Do nothing; there is no record and we want to keep it that way.
                    continue;
                }
                // Insert stream.
                let mut stmt = tx.prepare_cached(r#"
                    insert into stream (camera_id,  sample_file_dir_id,  type,  rtsp_url,  record,
                                        retain_bytes, flush_if_sec,  next_recording_id)
                                values (:camera_id, :sample_file_dir_id, :type, :rtsp_url, :record,
                                        0,            :flush_if_sec, 1)
                "#)?;
                stmt.execute_named(&[
                    (":camera_id", &camera_id),
                    (":sample_file_dir_id", &sc.sample_file_dir_id),
                    (":type", &type_.as_str()),
                    (":rtsp_url", &sc.rtsp_url),
                    (":record", &sc.record),
                    (":flush_if_sec", &sc.flush_if_sec),
                ])?;
                let id = tx.last_insert_rowid() as i32;
                sids[i] = Some(id);
                let sc = mem::replace(*sc, StreamChange::default());
                streams.push((id, Some((camera_id, type_, sc))));
            }
        }
        Ok(StreamStateChanger {
            sids,
            streams,
        })
    }

    /// Applies the change to the given `streams_by_id`. The caller is expected to set
    /// `Camera::streams` to the return value.
    fn apply(mut self, streams_by_id: &mut BTreeMap<i32, Stream>) -> [Option<i32>; 2] {
        for (id, stream) in self.streams.drain(..) {
            use ::std::collections::btree_map::Entry;
            match (streams_by_id.entry(id), stream) {
                (Entry::Vacant(e), Some((camera_id, type_, mut sc))) => {
                    e.insert(Stream {
                        id,
                        type_,
                        camera_id,
                        sample_file_dir_id: sc.sample_file_dir_id,
                        rtsp_url: mem::replace(&mut sc.rtsp_url, String::new()),
                        retain_bytes: 0,
                        flush_if_sec: sc.flush_if_sec,
                        range: None,
                        sample_file_bytes: 0,
                        to_delete: Vec::new(),
                        bytes_to_delete: 0,
                        bytes_to_add: 0,
                        duration: recording::Duration(0),
                        days: BTreeMap::new(),
                        record: sc.record,
                        next_recording_id: 1,
                        uncommitted: VecDeque::new(),
                        synced_recordings: 0,
                        on_live_segment: Vec::new(),
                    });
                },
                (Entry::Vacant(_), None) => {},
                (Entry::Occupied(e), Some((_, _, sc))) => {
                    let e = e.into_mut();
                    e.sample_file_dir_id = sc.sample_file_dir_id;
                    e.rtsp_url = sc.rtsp_url;
                    e.record = sc.record;
                    e.flush_if_sec = sc.flush_if_sec;
                },
                (Entry::Occupied(e), None) => { e.remove(); },
            };
        }
        self.sids
    }
}

/// A retention change as expected by `LockedDatabase::update_retention`.
pub struct RetentionChange {
    pub stream_id: i32,
    pub new_record: bool,
    pub new_limit: i64,
}

impl LockedDatabase {
    /// Returns an immutable view of the cameras by id.
    pub fn cameras_by_id(&self) -> &BTreeMap<i32, Camera> { &self.cameras_by_id }
    pub fn sample_file_dirs_by_id(&self) -> &BTreeMap<i32, SampleFileDir> {
        &self.sample_file_dirs_by_id
    }

    /// Returns the number of completed database flushes since startup.
    pub fn flushes(&self) -> usize { self.flush_count }

    /// Adds a placeholder for an uncommitted recording.
    /// The caller should write samples and fill the returned `RecordingToInsert` as it goes
    /// (noting that while holding the lock, it should not perform I/O or acquire the database
    /// lock). Then it should sync to permanent storage and call `mark_synced`. The data will
    /// be written to the database on the next `flush`.
    pub(crate) fn add_recording(&mut self, stream_id: i32, r: RecordingToInsert)
                             -> Result<(CompositeId, Arc<Mutex<RecordingToInsert>>), Error> {
        let stream = match self.streams_by_id.get_mut(&stream_id) {
            None => bail!("no such stream {}", stream_id),
            Some(s) => s,
        };
        let id = CompositeId::new(stream_id,
                                  stream.next_recording_id + (stream.uncommitted.len() as i32));
        let recording = Arc::new(Mutex::new(r));
        stream.uncommitted.push_back(Arc::clone(&recording));
        Ok((id, recording))
    }

    /// Marks the given uncomitted recording as synced and ready to flush.
    /// This must be the next unsynced recording.
    pub(crate) fn mark_synced(&mut self, id: CompositeId) -> Result<(), Error> {
        let stream = match self.streams_by_id.get_mut(&id.stream()) {
            None => bail!("no stream for recording {}", id),
            Some(s) => s,
        };
        let next_unsynced = stream.next_recording_id + (stream.synced_recordings as i32);
        if id.recording() != next_unsynced {
            bail!("can't sync {} when next unsynced recording is {} (next unflushed is {})",
                  id, next_unsynced, stream.next_recording_id);
        }
        if stream.synced_recordings == stream.uncommitted.len() {
            bail!("can't sync un-added recording {}", id);
        }
        let l = stream.uncommitted[stream.synced_recordings].lock();
        stream.bytes_to_add += l.sample_file_bytes as i64;
        stream.synced_recordings += 1;
        Ok(())
    }

    pub(crate) fn delete_garbage(&mut self, dir_id: i32, ids: &mut Vec<CompositeId>)
                                 -> Result<(), Error> {
        let dir = match self.sample_file_dirs_by_id.get_mut(&dir_id) {
            None => bail!("no such dir {}", dir_id),
            Some(d) => d,
        };
        dir.garbage_unlinked.reserve(ids.len());
        ids.retain(|id| {
            if !dir.garbage_needs_unlink.remove(id) {
                return true;
            }
            dir.garbage_unlinked.push(*id);
            false
        });
        if !ids.is_empty() {
            bail!("delete_garbage with non-garbage ids {:?}", &ids[..]);
        }
        Ok(())
    }

    /// Registers a callback to run on every live segment immediately after it's recorded.
    /// The callback is run with the database lock held, so it must not call back into the database
    /// or block. The callback should return false to unregister.
    pub fn watch_live(&mut self, stream_id: i32, cb: Box<dyn FnMut(LiveSegment) -> bool + Send>)
                      -> Result<(), Error> {
        let s = match self.streams_by_id.get_mut(&stream_id) {
            None => bail!("no such stream {}", stream_id),
            Some(s) => s,
        };
        s.on_live_segment.push(cb);
        Ok(())
    }

    /// Clears all watches on all streams.
    /// Normally watches are self-cleaning: when a segment is sent, the callback returns false if
    /// it is no longer interested (typically because hyper has just noticed the client is no
    /// longer connected). This doesn't work when the system is shutting down and nothing more is
    /// sent, though.
    pub fn clear_watches(&mut self) {
        for (_, s) in &mut self.streams_by_id {
            s.on_live_segment.clear();
        }
    }

    pub(crate) fn send_live_segment(&mut self, stream: i32, l: LiveSegment) -> Result<(), Error> {
        let s = match self.streams_by_id.get_mut(&stream) {
            None => bail!("no such stream {}", stream),
            Some(s) => s,
        };
        use odds::vec::VecExt;
        s.on_live_segment.retain_mut(|cb| cb(l.clone()));
        Ok(())
    }

    /// Helper for `DatabaseGuard::flush()` and `Database::drop()`.
    ///
    /// The public API is in `DatabaseGuard::flush()`; it supplies the `Clocks` to this function.
    fn flush<C: Clocks>(&mut self, clocks: &C, reason: &str) -> Result<(), Error> {
        let o = match self.open.as_ref() {
            None => bail!("database is read-only"),
            Some(o) => o,
        };
        let tx = self.conn.transaction()?;
        let mut new_ranges = FnvHashMap::with_capacity_and_hasher(self.streams_by_id.len(),
                                                                  Default::default());
        {
            let mut stmt = tx.prepare_cached(UPDATE_NEXT_RECORDING_ID_SQL)?;
            for (&stream_id, s) in &self.streams_by_id {
                // Process additions.
                for i in 0..s.synced_recordings {
                    let l = s.uncommitted[i].lock();
                    raw::insert_recording(
                        &tx, o, CompositeId::new(stream_id, s.next_recording_id + i as i32), &l)?;
                }
                if s.synced_recordings > 0 {
                    new_ranges.entry(stream_id).or_insert(None);
                    stmt.execute_named(&[
                        (":stream_id", &stream_id),
                        (":next_recording_id", &(s.next_recording_id + s.synced_recordings as i32)),
                    ])?;
                }

                // Process deletions.
                if let Some(l) = s.to_delete.last() {
                    new_ranges.entry(stream_id).or_insert(None);
                    let dir = match s.sample_file_dir_id {
                        None => bail!("stream {} has no directory!", stream_id),
                        Some(d) => d,
                    };

                    // raw::delete_recordings does a bulk transfer of a range from recording to
                    // garbage, rather than operating on each element of to_delete. This is
                    // guaranteed to give the same result because to_delete is guaranteed to be the
                    // oldest recordings for the stream.
                    let start = CompositeId::new(stream_id, 0);
                    let end = CompositeId(l.id.0 + 1);
                    let n = raw::delete_recordings(&tx, dir, start .. end)? as usize;
                    if n != s.to_delete.len() {
                        bail!("Found {} rows in {} .. {}, expected {}: {:?}",
                              n, start, end, s.to_delete.len(), &s.to_delete);
                    }
                }
            }
        }
        for dir in self.sample_file_dirs_by_id.values() {
            raw::mark_sample_files_deleted(&tx, &dir.garbage_unlinked)?;
        }
        for (&stream_id, r) in &mut new_ranges {
            *r = raw::get_range(&tx, stream_id)?;
        }
        {
            let mut stmt = tx.prepare_cached(
                r"update open set duration_90k = ?, end_time_90k = ? where id = ?")?;
            let rows = stmt.execute(&[
                &(recording::Time::new(clocks.monotonic()) - self.open_monotonic).0 as &dyn ToSql,
                &recording::Time::new(clocks.realtime()).0,
                &o.id,
            ])?;
            if rows != 1 {
                bail!("unable to find current open {}", o.id);
            }
        }
        self.auth.flush(&tx)?;
        self.signal.flush(&tx)?;
        tx.commit()?;

        // Process delete_garbage.
        let mut gced = SmallVec::<[CompositeId; 8]>::new();
        for dir in self.sample_file_dirs_by_id.values_mut() {
            gced.extend(dir.garbage_unlinked.drain(..));
        }

        let mut added = SmallVec::<[CompositeId; 8]>::new();
        let mut deleted = SmallVec::<[CompositeId; 8]>::new();
        for (stream_id, new_range) in new_ranges.drain() {
            let s = self.streams_by_id.get_mut(&stream_id).unwrap();
            let d = self.sample_file_dirs_by_id.get_mut(&s.sample_file_dir_id.unwrap()).unwrap();

            // Process delete_oldest_recordings.
            s.sample_file_bytes -= s.bytes_to_delete;
            s.bytes_to_delete = 0;
            deleted.reserve(s.to_delete.len());
            for row in s.to_delete.drain(..) {
                deleted.push(row.id);
                d.garbage_needs_unlink.insert(row.id);
                let d = recording::Duration(row.duration as i64);
                s.duration -= d;
                adjust_days(row.start .. row.start + d, -1, &mut s.days);
            }

            // Process add_recordings.
            s.bytes_to_add = 0;
            added.reserve(s.synced_recordings);
            for _ in 0..s.synced_recordings {
                let u = s.uncommitted.pop_front().unwrap();
                added.push(CompositeId::new(stream_id, s.next_recording_id));
                s.next_recording_id += 1;
                let l = u.lock();
                let end = l.start + recording::Duration(l.duration_90k as i64);
                s.add_recording(l.start .. end, l.sample_file_bytes);
            }
            s.synced_recordings = 0;

            // Fix the range.
            s.range = new_range;
        }
        self.auth.post_flush();
        self.signal.post_flush();
        self.flush_count += 1;
        info!("Flush {} (why: {}): added {} recordings ({}), deleted {} ({}), marked {} ({}) GCed.",
              self.flush_count, reason, added.len(), added.iter().join(", "), deleted.len(),
              deleted.iter().join(", "), gced.len(), gced.iter().join(", "));
        for cb in &self.on_flush {
            cb();
        }
        Ok(())
    }

    /// Sets a watcher which will receive an (empty) event on successful flush.
    /// The lock will be held while this is run, so it should not do any I/O.
    pub(crate) fn on_flush(&mut self, run: Box<dyn Fn() + Send>) {
        self.on_flush.push(run);
    }

    // TODO: find a cleaner way to do this. Seems weird for src/cmds/run.rs to clear the on flush
    // handlers given that it didn't add them.
    pub fn clear_on_flush(&mut self) {
        self.on_flush.clear();
     }

    /// Opens the given sample file directories.
    ///
    /// `ids` is implicitly de-duplicated.
    ///
    /// When the database is in read-only mode, this simply opens all the directories after
    /// locking and verifying their metadata matches the database state. In read-write mode, it
    /// performs a single database transaction to update metadata for all dirs, then performs a like
    /// update to the directories' on-disk metadata.
    ///
    /// Note this violates the principle of never accessing disk while holding the database lock.
    /// Currently this only happens at startup (or during configuration), so this isn't a problem
    /// in practice.
    pub fn open_sample_file_dirs(&mut self, ids: &[i32]) -> Result<(), Error> {
        let mut in_progress = FnvHashMap::with_capacity_and_hasher(ids.len(), Default::default());
        for &id in ids {
            let e = in_progress.entry(id);
            use ::std::collections::hash_map::Entry;
            let e = match e {
                Entry::Occupied(_) => continue,  // suppress duplicate.
                Entry::Vacant(e) => e,
            };
            let dir = self.sample_file_dirs_by_id
                          .get_mut(&id)
                          .ok_or_else(|| format_err!("no such dir {}", id))?;
            if dir.dir.is_some() { continue }
            let mut meta = dir.meta(&self.uuid);
            if let Some(o) = self.open.as_ref() {
                let open = meta.in_progress_open.mut_message();
                open.id = o.id;
                open.uuid.extend_from_slice(&o.uuid.as_bytes()[..]);
            }
            let d = dir::SampleFileDir::open(&dir.path, &meta)?;
            if self.open.is_none() {  // read-only mode; it's already fully opened.
                dir.dir = Some(d);
            } else {  // read-write mode; there are more steps to do.
                e.insert((meta, d));
            }
        }

        let o = match self.open.as_ref() {
            None => return Ok(()),  // read-only mode; all done.
            Some(o) => o,
        };

        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(r#"
                update sample_file_dir set last_complete_open_id = ? where id = ?
            "#)?;
            for &id in in_progress.keys() {
                if stmt.execute(&[&o.id as &dyn ToSql, &id])? != 1 {
                    bail!("unable to update dir {}", id);
                }
            }
        }
        tx.commit()?;

        for (id, (mut meta, d)) in in_progress.drain() {
            let dir = self.sample_file_dirs_by_id.get_mut(&id).unwrap();
            meta.last_complete_open.clear();
            mem::swap(&mut meta.last_complete_open, &mut meta.in_progress_open);
            d.write_meta(&meta)?;
            dir.dir = Some(d);
        }

        Ok(())
    }

    pub fn streams_by_id(&self) -> &BTreeMap<i32, Stream> { &self.streams_by_id }

    /// Returns an immutable view of the video sample entries.
    pub fn video_sample_entries_by_id(&self) -> &BTreeMap<i32, Arc<VideoSampleEntry>> {
        &self.video_sample_entries_by_id
    }

    /// Gets a given camera by uuid.
    pub fn get_camera(&self, uuid: Uuid) -> Option<&Camera> {
        match self.cameras_by_uuid.get(&uuid) {
            Some(id) => Some(self.cameras_by_id.get(id).expect("uuid->id requires id->cam")),
            None => None,
        }
    }

    /// Lists the specified recordings, passing them to a supplied function. Given that the
    /// function is called with the database lock held, it should be quick.
    ///
    /// Note that at present, the returned recordings are _not_ completely ordered by start time.
    /// Uncommitted recordings are returned id order after the others.
    pub fn list_recordings_by_time(
        &self, stream_id: i32, desired_time: Range<recording::Time>,
        f: &mut dyn FnMut(ListRecordingsRow) -> Result<(), Error>) -> Result<(), Error> {
        let s = match self.streams_by_id.get(&stream_id) {
            None => bail!("no such stream {}", stream_id),
            Some(s) => s,
        };
        raw::list_recordings_by_time(&self.conn, stream_id, desired_time.clone(), f)?;
        for (i, u) in s.uncommitted.iter().enumerate() {
            let row = {
                let l = u.lock();
                if l.video_samples > 0 {
                    let end = l.start + recording::Duration(l.duration_90k as i64);
                    if l.start > desired_time.end || end < desired_time.start {
                        continue;  // there's no overlap with the requested range.
                    }
                    l.to_list_row(CompositeId::new(stream_id, s.next_recording_id + i as i32),
                                  self.open.unwrap().id)
                } else {
                    continue;
                }
            };
            f(row)?;
        }
        Ok(())
    }

    /// Lists the specified recordings in ascending order by id.
    pub fn list_recordings_by_id(
        &self, stream_id: i32, desired_ids: Range<i32>,
        f: &mut dyn FnMut(ListRecordingsRow) -> Result<(), Error>) -> Result<(), Error> {
        let s = match self.streams_by_id.get(&stream_id) {
            None => bail!("no such stream {}", stream_id),
            Some(s) => s,
        };
        if desired_ids.start < s.next_recording_id {
            raw::list_recordings_by_id(&self.conn, stream_id, desired_ids.clone(), f)?;
        }
        if desired_ids.end > s.next_recording_id {
            let start = cmp::max(0, desired_ids.start - s.next_recording_id) as usize;
            let end = cmp::min((desired_ids.end - s.next_recording_id) as usize,
                               s.uncommitted.len());
            for i in start .. end {
                let row = {
                    let l = s.uncommitted[i].lock();
                    if l.video_samples > 0 {
                        l.to_list_row(CompositeId::new(stream_id, s.next_recording_id + i as i32),
                                      self.open.unwrap().id)
                    } else {
                        continue;
                    }
                };
                f(row)?;
            }
        }
        Ok(())
    }

    /// Calls `list_recordings_by_time` and aggregates consecutive recordings.
    /// Rows are given to the callback in arbitrary order. Callers which care about ordering
    /// should do their own sorting.
    pub fn list_aggregated_recordings(
        &self, stream_id: i32, desired_time: Range<recording::Time>,
        forced_split: recording::Duration,
        f: &mut dyn FnMut(&ListAggregatedRecordingsRow) -> Result<(), Error>)
        -> Result<(), Error> {
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
        // causing problems.) list_recordings_by_time also returns uncommitted recordings in
        // ascending order by id, and after any committed recordings with lower ids.
        let mut aggs: BTreeMap<i32, ListAggregatedRecordingsRow> = BTreeMap::new();
        self.list_recordings_by_time(stream_id, desired_time, &mut |row| {
            let recording_id = row.id.recording();
            let run_start_id = recording_id - row.run_offset;
            let uncommitted = (row.flags & RecordingFlags::Uncommitted as i32) != 0;
            let growing = (row.flags & RecordingFlags::Growing as i32) != 0;
            use std::collections::btree_map::Entry;
            match aggs.entry(run_start_id) {
                Entry::Occupied(mut e) => {
                    let a = e.get_mut();
                    let new_dur = a.time.end - a.time.start +
                                  recording::Duration(row.duration_90k as i64);
                    let needs_flush =
                        a.ids.end != recording_id ||
                        row.video_sample_entry_id != a.video_sample_entry_id ||
                        new_dur >= forced_split;
                    if needs_flush {  // flush then start a new entry.
                        f(a)?;
                        *a = ListAggregatedRecordingsRow::from(row);
                    } else {  // append.
                        if a.time.end != row.start {
                            bail!("stream {} recording {} ends at {} but {} starts at {}",
                                  stream_id, a.ids.end - 1, a.time.end, row.id, row.start);
                        }
                        if a.open_id != row.open_id {
                            bail!("stream {} recording {} has open id {} but {} has {}",
                                  stream_id, a.ids.end - 1, a.open_id, row.id, row.open_id);
                        }
                        a.time.end.0 += row.duration_90k as i64;
                        a.ids.end = recording_id + 1;
                        a.video_samples += row.video_samples as i64;
                        a.video_sync_samples += row.video_sync_samples as i64;
                        a.sample_file_bytes += row.sample_file_bytes as i64;
                        if uncommitted {
                            a.first_uncommitted = a.first_uncommitted.or(Some(recording_id));
                        }
                        a.growing = growing;
                    }
                },
                Entry::Vacant(e) => { e.insert(ListAggregatedRecordingsRow::from(row)); },
            }
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
    pub fn with_recording_playback<R>(&self, id: CompositeId,
                                      f: &mut dyn FnMut(&RecordingPlayback) -> Result<R, Error>)
                                      -> Result<R, Error> {
        // Check for uncommitted path.
        let s = self.streams_by_id
                    .get(&id.stream())
                    .ok_or_else(|| format_err!("no stream for {}", id))?;
        if s.next_recording_id <= id.recording() {
            let i = id.recording() - s.next_recording_id;
            if i as usize >= s.uncommitted.len() {
                bail!("no such recording {}; latest committed is {}, latest is {}",
                      id, s.next_recording_id, s.next_recording_id + s.uncommitted.len() as i32);
            }
            let l = s.uncommitted[i as usize].lock();
            return f(&RecordingPlayback { video_index: &l.video_index });
        }

        // Committed path.
        let mut cache = self.video_index_cache.borrow_mut();
        if let Some(video_index) = cache.get_mut(&id.0) {
            trace!("cache hit for recording {}", id);
            return f(&RecordingPlayback { video_index });
        }
        trace!("cache miss for recording {}", id);
        let mut stmt = self.conn.prepare_cached(GET_RECORDING_PLAYBACK_SQL)?;
        let mut rows = stmt.query_named(&[(":composite_id", &id.0)])?;
        if let Some(row) = rows.next()? {
            let video_index: VideoIndex = row.get(0)?;
            let result = f(&RecordingPlayback { video_index: &video_index.0[..] });
            cache.insert(id.0, video_index.0);
            return result;
        }
        Err(format_err!("no such recording {}", id))
    }

    /// Deletes the oldest recordings that aren't already queued for deletion.
    /// `f` should return true for each row that should be deleted.
    pub(crate) fn delete_oldest_recordings(
        &mut self, stream_id: i32, f: &mut dyn FnMut(&ListOldestRecordingsRow) -> bool)
        -> Result<(), Error> {
        let s = match self.streams_by_id.get_mut(&stream_id) {
            None => bail!("no stream {}", stream_id),
            Some(s) => s,
        };
        let end = match s.to_delete.last() {
            None => 0,
            Some(row) => row.id.recording() + 1,
        };
        raw::list_oldest_recordings(&self.conn, CompositeId::new(stream_id, end), &mut |r| {
            if f(&r) {
                s.to_delete.push(r);
                s.bytes_to_delete += r.sample_file_bytes as i64;
                return true;
            }
            false
        })
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
                rfc6381_codec,
                data
            from
                video_sample_entry
        "#)?;
        let mut rows = stmt.query(&[] as &[&dyn ToSql])?;
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            let mut sha1 = [0u8; 20];
            let sha1_vec: Vec<u8> = row.get(1)?;
            if sha1_vec.len() != 20 {
                bail!("video sample entry id {} has sha1 {} of wrong length", id, sha1_vec.len());
            }
            sha1.copy_from_slice(&sha1_vec);
            let data: Vec<u8> = row.get(5)?;

            self.video_sample_entries_by_id.insert(id, Arc::new(VideoSampleEntry {
                id: id as i32,
                width: row.get::<_, i32>(2)? as u16,
                height: row.get::<_, i32>(3)? as u16,
                sha1,
                data,
                rfc6381_codec: row.get(4)?,
            }));
        }
        info!("Loaded {} video sample entries",
              self.video_sample_entries_by_id.len());
        Ok(())
    }

    /// Initializes the sample file dirs.
    /// To be called during construction.
    fn init_sample_file_dirs(&mut self) -> Result<(), Error> {
        info!("Loading sample file dirs");
        let mut stmt = self.conn.prepare(r#"
            select
              d.id,
              d.path,
              d.uuid,
              d.last_complete_open_id,
              o.uuid
            from
              sample_file_dir d left join open o on (d.last_complete_open_id = o.id);
        "#)?;
        let mut rows = stmt.query(&[] as &[&dyn ToSql])?;
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            let dir_uuid: FromSqlUuid = row.get(2)?;
            let open_id: Option<u32> = row.get(3)?;
            let open_uuid: Option<FromSqlUuid> = row.get(4)?;
            let last_complete_open = match (open_id, open_uuid) {
                (Some(id), Some(uuid)) => Some(Open { id, uuid: uuid.0, }),
                (None, None) => None,
                _ => bail!("open table missing id {}", id),
            };
            self.sample_file_dirs_by_id.insert(id, SampleFileDir {
                id,
                uuid: dir_uuid.0,
                path: row.get(1)?,
                dir: None,
                last_complete_open,
                garbage_needs_unlink: raw::list_garbage(&self.conn, id)?,
                garbage_unlinked: Vec::new(),
            });
        }
        info!("Loaded {} sample file dirs", self.sample_file_dirs_by_id.len());
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
              onvif_host,
              username,
              password
            from
              camera;
        "#)?;
        let mut rows = stmt.query(&[] as &[&dyn ToSql])?;
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            let uuid: FromSqlUuid = row.get(1)?;
            self.cameras_by_id.insert(id, Camera {
                id: id,
                uuid: uuid.0,
                short_name: row.get(2)?,
                description: row.get(3)?,
                onvif_host: row.get(4)?,
                username: row.get(5)?,
                password: row.get(6)?,
                streams: Default::default(),
            });
            self.cameras_by_uuid.insert(uuid.0, id);
        }
        info!("Loaded {} cameras", self.cameras_by_id.len());
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
              sample_file_dir_id,
              rtsp_url,
              retain_bytes,
              flush_if_sec,
              next_recording_id,
              record
            from
              stream;
        "#)?;
        let mut rows = stmt.query(&[] as &[&dyn ToSql])?;
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            let type_: String = row.get(1)?;
            let type_ = StreamType::parse(&type_).ok_or_else(
                || format_err!("no such stream type {}", type_))?;
            let camera_id = row.get(2)?;
            let c = self
                        .cameras_by_id
                        .get_mut(&camera_id)
                        .ok_or_else(|| format_err!("missing camera {} for stream {}",
                                                   camera_id, id))?;
            let flush_if_sec = row.get(6)?;
            self.streams_by_id.insert(id, Stream {
                id,
                type_,
                camera_id,
                sample_file_dir_id: row.get(3)?,
                rtsp_url: row.get(4)?,
                retain_bytes: row.get(5)?,
                flush_if_sec,
                range: None,
                sample_file_bytes: 0,
                to_delete: Vec::new(),
                bytes_to_delete: 0,
                bytes_to_add: 0,
                duration: recording::Duration(0),
                days: BTreeMap::new(),
                next_recording_id: row.get(7)?,
                record: row.get(8)?,
                uncommitted: VecDeque::new(),
                synced_recordings: 0,
                on_live_segment: Vec::new(),
            });
            c.streams[type_.index()] = Some(id);
        }
        info!("Loaded {} streams", self.streams_by_id.len());
        Ok(())
    }

    /// Inserts the specified video sample entry if absent.
    /// On success, returns the id of a new or existing row.
    pub fn insert_video_sample_entry(&mut self, width: u16, height: u16, data: Vec<u8>,
                                     rfc6381_codec: String) -> Result<i32, Error> {
        let sha1 = hash::hash(hash::MessageDigest::sha1(), &data)?;
        let mut sha1_bytes = [0u8; 20];
        sha1_bytes.copy_from_slice(&sha1);

        // Check if it already exists.
        // There shouldn't be too many entries, so it's fine to enumerate everything.
        for (&id, v) in &self.video_sample_entries_by_id {
            if v.sha1 == sha1_bytes {
                // The width and height should match given that they're also specified within data
                // and thus included in the just-compared hash.
                if v.width != width || v.height != height {
                    bail!("database entry for {:?} is {}x{}, not {}x{}",
                          &sha1[..], v.width, v.height, width, height);
                }
                return Ok(id);
            }
        }

        let mut stmt = self.conn.prepare_cached(INSERT_VIDEO_SAMPLE_ENTRY_SQL)?;
        stmt.execute_named(&[
            (":sha1", &&sha1_bytes[..]),
            (":width", &(width as i64)),
            (":height", &(height as i64)),
            (":rfc6381_codec", &rfc6381_codec),
            (":data", &data),
        ])?;

        let id = self.conn.last_insert_rowid() as i32;
        self.video_sample_entries_by_id.insert(id, Arc::new(VideoSampleEntry {
            id,
            width,
            height,
            sha1: sha1_bytes,
            data,
            rfc6381_codec,
        }));

        Ok(id)
    }

    pub fn add_sample_file_dir(&mut self, path: String) -> Result<i32, Error> {
        let mut meta = schema::DirMeta::default();
        let uuid = Uuid::new_v4();
        let uuid_bytes = &uuid.as_bytes()[..];
        let o = self.open
                    .as_ref()
                    .ok_or_else(|| format_err!("database is read-only"))?;

        // Populate meta.
        {
            meta.db_uuid.extend_from_slice(&self.uuid.as_bytes()[..]);
            meta.dir_uuid.extend_from_slice(uuid_bytes);
            let open = meta.in_progress_open.mut_message();
            open.id = o.id;
            open.uuid.extend_from_slice(&o.uuid.as_bytes()[..]);
        }

        let dir = dir::SampleFileDir::create(&path, &meta)?;
        self.conn.execute(r#"
            insert into sample_file_dir (path, uuid, last_complete_open_id)
                                 values (?,    ?,    ?)
        "#, &[&path as &dyn ToSql, &uuid_bytes, &o.id])?;
        let id = self.conn.last_insert_rowid() as i32;
        use ::std::collections::btree_map::Entry;
        let e = self.sample_file_dirs_by_id.entry(id);
        let d = match e {
            Entry::Vacant(e) => e.insert(SampleFileDir {
                id,
                path,
                uuid,
                dir: Some(dir),
                last_complete_open: None,
                garbage_needs_unlink: FnvHashSet::default(),
                garbage_unlinked: Vec::new(),
            }),
            Entry::Occupied(_) => Err(format_err!("duplicate sample file dir id {}", id))?,
        };
        d.last_complete_open = Some(*o);
        mem::swap(&mut meta.last_complete_open, &mut meta.in_progress_open);
        d.dir.as_ref().unwrap().write_meta(&meta)?;
        Ok(id)
    }

    pub fn delete_sample_file_dir(&mut self, dir_id: i32) -> Result<(), Error> {
        for (&id, s) in self.streams_by_id.iter() {
            if s.sample_file_dir_id == Some(dir_id) {
                bail!("can't delete dir referenced by stream {}", id);
            }
        }
        let mut d = match self.sample_file_dirs_by_id.entry(dir_id) {
            ::std::collections::btree_map::Entry::Occupied(e) => e,
            _ => bail!("no such dir {} to remove", dir_id),
        };
        if !d.get().garbage_needs_unlink.is_empty() || !d.get().garbage_unlinked.is_empty() {
            bail!("must collect garbage before deleting directory {}", d.get().path);
        }
        let dir = match d.get_mut().dir.take() {
            None => dir::SampleFileDir::open(&d.get().path, &d.get().meta(&self.uuid))?,
            Some(arc) => match Arc::strong_count(&arc) {
                1 => {
                    d.get_mut().dir = Some(arc);  // put it back.
                    bail!("can't delete in-use directory {}", dir_id);
                },
                _ => arc,
            },
        };
        if !dir.is_empty()? {
            bail!("Can't delete sample file directory {} which still has files", &d.get().path);
        }
        let mut meta = d.get().meta(&self.uuid);
        meta.in_progress_open = mem::replace(&mut meta.last_complete_open,
                                             ::protobuf::SingularPtrField::none());
        dir.write_meta(&meta)?;
        if self.conn.execute("delete from sample_file_dir where id = ?", &[&dir_id])? != 1 {
            bail!("missing database row for dir {}", dir_id);
        }
        d.remove_entry();
        Ok(())
    }

    /// Adds a camera.
    pub fn add_camera(&mut self, mut camera: CameraChange) -> Result<i32, Error> {
        let uuid = Uuid::new_v4();
        let uuid_bytes = &uuid.as_bytes()[..];
        let tx = self.conn.transaction()?;
        let streams;
        let camera_id;
        {
            let mut stmt = tx.prepare_cached(r#"
                insert into camera (uuid,  short_name,  description,  onvif_host,  username,
                                    password)
                            values (:uuid, :short_name, :description, :onvif_host, :username,
                                    :password)
            "#)?;
            stmt.execute_named(&[
                (":uuid", &uuid_bytes),
                (":short_name", &camera.short_name),
                (":description", &camera.description),
                (":onvif_host", &camera.onvif_host),
                (":username", &camera.username),
                (":password", &camera.password),
            ])?;
            camera_id = tx.last_insert_rowid() as i32;
            streams = StreamStateChanger::new(&tx, camera_id, None, &self.streams_by_id,
                                         &mut camera)?;
        }
        tx.commit()?;
        let streams = streams.apply(&mut self.streams_by_id);
        self.cameras_by_id.insert(camera_id, Camera {
            id: camera_id,
            uuid,
            short_name: camera.short_name,
            description: camera.description,
            onvif_host: camera.onvif_host,
            username: camera.username,
            password: camera.password,
            streams,
        });
        self.cameras_by_uuid.insert(uuid, camera_id);
        Ok(camera_id)
    }

    /// Updates a camera.
    pub fn update_camera(&mut self, camera_id: i32, mut camera: CameraChange) -> Result<(), Error> {
        let tx = self.conn.transaction()?;
        let streams;
        let c = self
                    .cameras_by_id
                    .get_mut(&camera_id)
                    .ok_or_else(|| format_err!("no such camera {}", camera_id))?;
        {
            streams = StreamStateChanger::new(&tx, camera_id, Some(c), &self.streams_by_id,
                                         &mut camera)?;
            let mut stmt = tx.prepare_cached(r#"
                update camera set
                    short_name = :short_name,
                    description = :description,
                    onvif_host = :onvif_host,
                    username = :username,
                    password = :password
                where
                    id = :id
            "#)?;
            let rows = stmt.execute_named(&[
                (":id", &camera_id),
                (":short_name", &camera.short_name),
                (":description", &camera.description),
                (":onvif_host", &camera.onvif_host),
                (":username", &camera.username),
                (":password", &camera.password),
            ])?;
            if rows != 1 {
                bail!("Camera {} missing from database", camera_id);
            }
        }
        tx.commit()?;
        c.short_name = camera.short_name;
        c.description = camera.description;
        c.onvif_host = camera.onvif_host;
        c.username = camera.username;
        c.password = camera.password;
        c.streams = streams.apply(&mut self.streams_by_id);
        Ok(())
    }

    /// Deletes a camera and its streams. The camera must have no recordings.
    pub fn delete_camera(&mut self, id: i32) -> Result<(), Error> {
        let uuid = self.cameras_by_id.get(&id)
                       .map(|c| c.uuid)
                       .ok_or_else(|| format_err!("No such camera {} to remove", id))?;
        let mut streams_to_delete = Vec::new();
        let tx = self.conn.transaction()?;
        {
            let mut stream_stmt = tx.prepare_cached(r"delete from stream where id = :id")?;
            for (stream_id, stream) in &self.streams_by_id {
                if stream.camera_id != id { continue };
                if stream.range.is_some() {
                    bail!("Can't remove camera {}; has recordings.", id);
                }
                let rows = stream_stmt.execute_named(&[(":id", stream_id)])?;
                if rows != 1 {
                    bail!("Stream {} missing from database", id);
                }
                streams_to_delete.push(*stream_id);
            }
            let mut cam_stmt = tx.prepare_cached(r"delete from camera where id = :id")?;
            let rows = cam_stmt.execute_named(&[(":id", &id)])?;
            if rows != 1 {
                bail!("Camera {} missing from database", id);
            }
        }
        tx.commit()?;
        for id in streams_to_delete {
            self.streams_by_id.remove(&id);
        }
        self.cameras_by_id.remove(&id);
        self.cameras_by_uuid.remove(&uuid);
        return Ok(())
    }

    pub fn update_retention(&mut self, changes: &[RetentionChange]) -> Result<(), Error> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(r#"
                update stream
                set
                  record = :record,
                  retain_bytes = :retain
                where
                  id = :id
            "#)?;
            for c in changes {
                if c.new_limit < 0 {
                    bail!("can't set limit for stream {} to {}; must be >= 0",
                          c.stream_id, c.new_limit);
                }
                let rows = stmt.execute_named(&[
                    (":record", &c.new_record),
                    (":retain", &c.new_limit),
                    (":id", &c.stream_id),
                ])?;
                if rows != 1 {
                    bail!("no such stream {}", c.stream_id);
                }
            }
        }
        tx.commit()?;
        for c in changes {
            let s = self.streams_by_id.get_mut(&c.stream_id).expect("stream in db but not state");
            s.record = c.new_record;
            s.retain_bytes = c.new_limit;
        }
        Ok(())
    }

    // ---- auth ----

    pub fn users_by_id(&self) -> &BTreeMap<i32, User> { self.auth.users_by_id() }

    pub fn apply_user_change(&mut self, change: UserChange) -> Result<&User, Error> {
        self.auth.apply(&self.conn, change)
    }

    pub fn delete_user(&mut self, id: i32) -> Result<(), Error> {
        self.auth.delete_user(&mut self.conn, id)
    }

    pub fn get_user(&self, username: &str) -> Option<&User> {
        self.auth.get_user(username)
    }

    pub fn login_by_password(&mut self, req: auth::Request, username: &str, password: String,
                             domain: Option<Vec<u8>>, session_flags: i32)
                             -> Result<(RawSessionId, &Session), Error> {
        self.auth.login_by_password(&self.conn, req, username, password, domain, session_flags)
    }

    pub fn make_session(&mut self, creation: Request, uid: i32,
                        domain: Option<Vec<u8>>, flags: i32, permissions: schema::Permissions)
                        -> Result<(RawSessionId, &Session), Error> {
        self.auth.make_session(&self.conn, creation, uid, domain, flags, permissions)
    }

    pub fn authenticate_session(&mut self, req: auth::Request, sid: &auth::SessionHash)
                                -> Result<(&auth::Session, &User), Error> {
        self.auth.authenticate_session(&self.conn, req, sid)
    }

    pub fn revoke_session(&mut self, reason: auth::RevocationReason, detail: Option<String>,
                          req: auth::Request, hash: &auth::SessionHash) -> Result<(), Error> {
        self.auth.revoke_session(&self.conn, reason, detail, req, hash)
    }

    // ---- signal ----

    pub fn signals_by_id(&self) -> &BTreeMap<u32, signal::Signal> { self.signal.signals_by_id() }
    pub fn signal_types_by_uuid(&self) -> &FnvHashMap<Uuid, signal::Type> {
        self.signal.types_by_uuid()
    }
    pub fn list_changes_by_time(
        &self, desired_time: Range<recording::Time>, f: &mut dyn FnMut(&signal::ListStateChangesRow)) {
        self.signal.list_changes_by_time(desired_time, f)
    }
    pub fn update_signals(
        &mut self, when: Range<recording::Time>, signals: &[u32], states: &[u16])
        -> Result<(), base::Error> {
        self.signal.update_signals(when, signals, states)
    }
}

/// Initializes a database.
/// Note this doesn't set journal options, so that it can be used on in-memory databases for
/// test code.
pub fn init(conn: &mut rusqlite::Connection) -> Result<(), Error> {
    conn.execute("pragma foreign_keys = on", &[] as &[&dyn ToSql])?;
    conn.execute("pragma fullfsync = on", &[] as &[&dyn ToSql])?;
    conn.execute("pragma synchronous = 2", &[] as &[&dyn ToSql])?;
    let tx = conn.transaction()?;
    tx.execute_batch(include_str!("schema.sql"))?;
    {
        let uuid = ::uuid::Uuid::new_v4();
        let uuid_bytes = &uuid.as_bytes()[..];
        tx.execute("insert into meta (uuid) values (?)", &[&uuid_bytes])?;
    }
    tx.commit()?;
    Ok(())
}

/// Gets the schema version from the given database connection.
/// A fully initialized database will return `Ok(Some(version))` where `version` is an integer that
/// can be compared to `EXPECTED_VERSION`. An empty database will return `Ok(None)`. A partially
/// initialized database (in particular, one without a version row) will return some error.
pub fn get_schema_version(conn: &rusqlite::Connection) -> Result<Option<i32>, Error> {
    let ver_tables: i32 = conn.query_row_and_then(
        "select count(*) from sqlite_master where name = 'version'",
        &[] as &[&dyn ToSql], |row| row.get(0))?;
    if ver_tables == 0 {
        return Ok(None);
    }
    Ok(Some(conn.query_row_and_then("select max(id) from version", &[] as &[&dyn ToSql],
                                    |row| row.get(0))?))
}

/// The recording database. Abstracts away SQLite queries. Also maintains in-memory state
/// (loaded on startup, and updated on successful commit) to avoid expensive scans over the
/// recording table on common queries.
pub struct Database<C: Clocks + Clone = clock::RealClocks> {
    /// This is wrapped in an `Option` to allow the `Drop` implementation and `close` to coexist.
    db: Option<Mutex<LockedDatabase>>,

    /// This is kept separately from the `LockedDatabase` to allow the `lock()` operation itself to
    /// access it. It doesn't need a `Mutex` anyway; it's `Sync`, and all operations work on
    /// `&self`.
    clocks: C,
}

impl<C: Clocks + Clone> Drop for Database<C> {
    fn drop(&mut self) {
        if ::std::thread::panicking() {
            return;  // don't flush while panicking.
        }
        if let Some(m) = self.db.take() {
            if let Err(e) = m.into_inner().flush(&self.clocks, "drop") {
                error!("Final database flush failed: {}", e);
            }
        }
    }
}

// Helpers for Database::lock(). Closures don't implement Fn.
fn acquisition() -> &'static str { "database lock acquisition" }
fn operation() -> &'static str { "database operation" }

impl<C: Clocks + Clone> Database<C> {
    /// Creates the database from a caller-supplied SQLite connection.
    pub fn new(clocks: C, conn: rusqlite::Connection,
               read_write: bool) -> Result<Database<C>, Error> {
        conn.execute("pragma foreign_keys = on", &[] as &[&dyn ToSql])?;
        conn.execute("pragma fullfsync = on", &[] as &[&dyn ToSql])?;
        conn.execute("pragma synchronous = 2", &[] as &[&dyn ToSql])?;
        {
            let ver = get_schema_version(&conn)?.ok_or_else(|| format_err!(
                    "no such table: version. \
                    \
                    If you are starting from an \
                    empty database, see README.md to complete the \
                    installation. If you are starting from a database \
                    that predates schema versioning, see guide/schema.md."))?;
            if ver < EXPECTED_VERSION {
                bail!("Database schema version {} is too old (expected {}); \
                       see upgrade instructions in guide/upgrade.md.",
                      ver, EXPECTED_VERSION);
            } else if ver > EXPECTED_VERSION {
                bail!("Database schema version {} is too new (expected {}); \
                       must use a newer binary to match.", ver,
                      EXPECTED_VERSION);

            }
        }

        // Note: the meta check comes after the version check to improve the error message when
        // trying to open a version 0 or version 1 database (which lacked the meta table).
        let uuid = raw::get_db_uuid(&conn)?;
        let open_monotonic = recording::Time::new(clocks.monotonic());
        let open = if read_write {
            let real = recording::Time::new(clocks.realtime());
            let mut stmt = conn.prepare(" insert into open (uuid, start_time_90k) values (?, ?)")?;
            let uuid = Uuid::new_v4();
            let uuid_bytes = &uuid.as_bytes()[..];
            stmt.execute(&[&uuid_bytes as &dyn ToSql, &real.0])?;
            Some(Open {
                id: conn.last_insert_rowid() as u32,
                uuid,
            })
        } else { None };
        let auth = auth::State::init(&conn)?;
        let signal = signal::State::init(&conn)?;
        let db = Database {
            db: Some(Mutex::new(LockedDatabase {
                conn,
                uuid,
                flush_count: 0,
                open,
                open_monotonic,
                auth,
                signal,
                sample_file_dirs_by_id: BTreeMap::new(),
                cameras_by_id: BTreeMap::new(),
                cameras_by_uuid: BTreeMap::new(),
                streams_by_id: BTreeMap::new(),
                video_sample_entries_by_id: BTreeMap::new(),
                video_index_cache: RefCell::new(LruCache::with_hasher(1024, Default::default())),
                on_flush: Vec::new(),
            })),
            clocks,
        };
        {
            let l = &mut *db.lock();
            l.init_video_sample_entries()?;
            l.init_sample_file_dirs()?;
            l.init_cameras()?;
            l.init_streams()?;
            for (&stream_id, ref mut stream) in &mut l.streams_by_id {
                // TODO: we could use one thread per stream if we had multiple db conns.
                let camera = l.cameras_by_id.get(&stream.camera_id).unwrap();
                init_recordings(&mut l.conn, stream_id, camera, stream)?;
            }
        }
        Ok(db)
    }

    #[inline(always)]
    pub fn clocks(&self) -> C { self.clocks.clone() }

    /// Locks the database; the returned reference is the only way to perform (read or write)
    /// operations.
    pub fn lock(&self) -> DatabaseGuard<C> {
        let timer = clock::TimerGuard::new(&self.clocks, acquisition);
        let db = self.db.as_ref().unwrap().lock();
        drop(timer);
        let _timer = clock::TimerGuard::<C, &'static str, fn() -> &'static str>::new(
            &self.clocks, operation);
        DatabaseGuard {
            clocks: &self.clocks,
            db,
            _timer,
        }
    }

    /// For testing: closes the database (without flushing) and returns the connection.
    /// This allows verification that a newly opened database is in an acceptable state.
    #[cfg(test)]
    fn close(mut self) -> rusqlite::Connection {
        self.db.take().unwrap().into_inner().conn
    }
}

pub struct DatabaseGuard<'db, C: Clocks> {
    clocks: &'db C,
    db: MutexGuard<'db, LockedDatabase>,
    _timer: clock::TimerGuard<'db, C, &'static str, fn() -> &'static str>,
}

impl<'db, C: Clocks + Clone> DatabaseGuard<'db, C> {
    /// Tries to flush unwritten changes from the stream directories.
    ///
    ///    * commits any recordings added with `add_recording` that have since been marked as
    ///      synced.
    ///    * moves old recordings to the garbage table as requested by `delete_oldest_recordings`.
    ///    * removes entries from the garbage table as requested by `mark_sample_files_deleted`.
    ///
    /// On success, for each affected sample file directory with a flush watcher set, sends a
    /// `Flush` event.
    pub(crate) fn flush(&mut self, reason: &str) -> Result<(), Error> {
        self.db.flush(self.clocks, reason)
    }
}

impl<'db, C: Clocks + Clone> ::std::ops::Deref for DatabaseGuard<'db, C> {
    type Target = LockedDatabase;
    fn deref(&self) -> &LockedDatabase { &*self.db }
}

impl<'db, C: Clocks + Clone> ::std::ops::DerefMut for DatabaseGuard<'db, C> {
    fn deref_mut(&mut self) -> &mut LockedDatabase { &mut *self.db }
}

#[cfg(test)]
mod tests {
    use base::clock;
    use crate::recording::{self, TIME_UNITS_PER_SEC};
    use rusqlite::Connection;
    use std::collections::BTreeMap;
    use crate::testutil;
    use super::*;
    use super::adjust_days;  // non-public.
    use uuid::Uuid;

    fn setup_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        super::init(&mut conn).unwrap();
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
                assert_eq!("test-camera", row.onvif_host);
                assert_eq!("foo", row.username);
                assert_eq!("bar", row.password);
                //assert_eq!("/main", row.main_rtsp_url);
                //assert_eq!("/sub", row.sub_rtsp_url);
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
            db.list_recordings_by_time(stream_id, all_time, &mut |_row| {
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
            let dur = recording::Duration(r.duration_90k as i64);
            assert_eq!(Some(r.start .. r.start + dur), stream.range);
            assert_eq!(r.sample_file_bytes as i64, stream.sample_file_bytes);
            assert_eq!(dur, stream.duration);
            db.cameras_by_id().get(&stream.camera_id).unwrap();
        }

        // TODO(slamb): test that the days logic works correctly.

        let mut rows = 0;
        let mut recording_id = None;
        {
            let db = db.lock();
            let all_time = recording::Time(i64::min_value()) .. recording::Time(i64::max_value());
            db.list_recordings_by_time(stream_id, all_time, &mut |row| {
                rows += 1;
                recording_id = Some(row.id);
                assert_eq!(r.start, row.start);
                assert_eq!(r.duration_90k, row.duration_90k);
                assert_eq!(r.video_samples, row.video_samples);
                assert_eq!(r.video_sync_samples, row.video_sync_samples);
                assert_eq!(r.sample_file_bytes, row.sample_file_bytes);
                let vse = db.video_sample_entries_by_id().get(&row.video_sample_entry_id).unwrap();
                assert_eq!(vse.rfc6381_codec, "avc1.4d0029");
                Ok(())
            }).unwrap();
        }
        assert_eq!(1, rows);

        rows = 0;
        raw::list_oldest_recordings(&db.lock().conn, CompositeId::new(stream_id, 0), &mut |row| {
            rows += 1;
            assert_eq!(recording_id, Some(row.id));
            assert_eq!(r.start, row.start);
            assert_eq!(r.duration_90k, row.duration);
            assert_eq!(r.sample_file_bytes, row.sample_file_bytes);
            true
        }).unwrap();
        assert_eq!(1, rows);

        // TODO: list_aggregated_recordings.
        // TODO: with_recording_playback.
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
    fn test_no_meta_or_version() {
        testutil::init();
        let e = Database::new(clock::RealClocks {}, Connection::open_in_memory().unwrap(),
                              false).err().unwrap();
        assert!(e.to_string().starts_with("no such table"), "{}", e);
    }

    #[test]
    fn test_version_too_old() {
        testutil::init();
        let c = setup_conn();
        c.execute_batch("delete from version; insert into version values (4, 0, '');").unwrap();
        let e = Database::new(clock::RealClocks {}, c, false).err().unwrap();
        assert!(e.to_string().starts_with(
                "Database schema version 4 is too old (expected 5)"), "got: {:?}", e);
    }

    #[test]
    fn test_version_too_new() {
        testutil::init();
        let c = setup_conn();
        c.execute_batch("delete from version; insert into version values (6, 0, '');").unwrap();
        let e = Database::new(clock::RealClocks {}, c, false).err().unwrap();
        assert!(e.to_string().starts_with(
                "Database schema version 6 is too new (expected 5)"), "got: {:?}", e);
    }

    /// Basic test of running some queries on a fresh database.
    #[test]
    fn test_fresh_db() {
        testutil::init();
        let conn = setup_conn();
        let db = Database::new(clock::RealClocks {}, conn, true).unwrap();
        let db = db.lock();
        assert_eq!(0, db.cameras_by_id().values().count());
    }

    /// Basic test of the full lifecycle of recording. Does not exercise error cases.
    #[test]
    fn test_full_lifecycle() {
        testutil::init();
        let conn = setup_conn();
        let db = Database::new(clock::RealClocks {}, conn, true).unwrap();
        let tmpdir = tempdir::TempDir::new("moonfire-nvr-test").unwrap();
        let path = tmpdir.path().to_str().unwrap().to_owned();
        let sample_file_dir_id = { db.lock() }.add_sample_file_dir(path).unwrap();
        let mut c = CameraChange {
            short_name: "testcam".to_owned(),
            description: "".to_owned(),
            onvif_host: "test-camera".to_owned(),
            username: "foo".to_owned(),
            password: "bar".to_owned(),
            streams: [
                StreamChange {
                    sample_file_dir_id: Some(sample_file_dir_id),
                    rtsp_url: "rtsp://test-camera/main".to_owned(),
                    record: false,
                    flush_if_sec: 1,
                },
                StreamChange {
                    sample_file_dir_id: Some(sample_file_dir_id),
                    rtsp_url: "rtsp://test-camera/sub".to_owned(),
                    record: true,
                    flush_if_sec: 1,
                },
            ],
        };
        let camera_id = db.lock().add_camera(c.clone()).unwrap();
        let (main_stream_id, sub_stream_id);
        {
            let mut l = db.lock();
            {
                let c = l.cameras_by_id().get(&camera_id).unwrap();
                main_stream_id = c.streams[0].unwrap();
                sub_stream_id = c.streams[1].unwrap();
            }
            l.update_retention(&[super::RetentionChange {
                stream_id: main_stream_id,
                new_record: true,
                new_limit: 42,
            }]).unwrap();
            {
                let main = l.streams_by_id().get(&main_stream_id).unwrap();
                assert!(main.record);
                assert_eq!(main.retain_bytes, 42);
                assert_eq!(main.flush_if_sec, 1);
            }

            assert_eq!(l.streams_by_id().get(&sub_stream_id).unwrap().flush_if_sec, 1);
            c.streams[1].flush_if_sec = 2;
            l.update_camera(camera_id, c).unwrap();
            assert_eq!(l.streams_by_id().get(&sub_stream_id).unwrap().flush_if_sec, 2);
        }
        let camera_uuid = { db.lock().cameras_by_id().get(&camera_id).unwrap().uuid };
        assert_no_recordings(&db, camera_uuid);

        // Closing and reopening the database should present the same contents.
        let conn = db.close();
        let db = Database::new(clock::RealClocks {}, conn, true).unwrap();
        assert_eq!(db.lock().streams_by_id().get(&sub_stream_id).unwrap().flush_if_sec, 2);
        assert_no_recordings(&db, camera_uuid);

        // TODO: assert_eq!(db.lock().list_garbage(sample_file_dir_id).unwrap(), &[]);

        let vse_id = db.lock().insert_video_sample_entry(
            1920, 1080, include_bytes!("testdata/avc1").to_vec(),
            "avc1.4d0029".to_owned()).unwrap();
        assert!(vse_id > 0, "vse_id = {}", vse_id);

        // Inserting a recording should succeed and advance the next recording id.
        let start = recording::Time(1430006400 * TIME_UNITS_PER_SEC);
        let recording = RecordingToInsert {
            sample_file_bytes: 42,
            run_offset: 0,
            flags: 0,
            start,
            duration_90k: TIME_UNITS_PER_SEC as i32,
            local_time_delta: recording::Duration(0),
            video_samples: 1,
            video_sync_samples: 1,
            video_sample_entry_id: vse_id,
            video_index: [0u8; 100].to_vec(),
            sample_file_sha1: [0u8; 20],
        };
        let id = {
            let mut db = db.lock();
            let (id, _) = db.add_recording(main_stream_id, recording.clone()).unwrap();
            db.mark_synced(id).unwrap();
            db.flush("add test").unwrap();
            id
        };
        assert_eq!(db.lock().streams_by_id().get(&main_stream_id).unwrap().next_recording_id, 2);

        // Queries should return the correct result (with caches update on insert).
        assert_single_recording(&db, main_stream_id, &recording);

        // Queries on a fresh database should return the correct result (with caches populated from
        // existing database contents rather than built on insert).
        let conn = db.close();
        let db = Database::new(clock::RealClocks {}, conn, true).unwrap();
        assert_single_recording(&db, main_stream_id, &recording);

        // Deleting a recording should succeed, update the min/max times, and mark it as garbage.
        {
            let mut db = db.lock();
            let mut n = 0;
            db.delete_oldest_recordings(main_stream_id, &mut |_| { n += 1; true }).unwrap();
            assert_eq!(n, 1);
            {
                let s = db.streams_by_id().get(&main_stream_id).unwrap();
                assert_eq!(s.sample_file_bytes, 42);
                assert_eq!(s.bytes_to_delete, 42);
            }
            n = 0;

            // A second run
            db.delete_oldest_recordings(main_stream_id, &mut |_| { n += 1; true }).unwrap();
            assert_eq!(n, 0);
            assert_eq!(db.streams_by_id().get(&main_stream_id).unwrap().bytes_to_delete, 42);
            db.flush("delete test").unwrap();
            let s = db.streams_by_id().get(&main_stream_id).unwrap();
            assert_eq!(s.sample_file_bytes, 0);
            assert_eq!(s.bytes_to_delete, 0);
        }
        assert_no_recordings(&db, camera_uuid);
        let g: Vec<_> = db.lock()
                          .sample_file_dirs_by_id()
                          .get(&sample_file_dir_id)
                          .unwrap()
                          .garbage_needs_unlink
                          .iter()
                          .map(|&id| id)
                          .collect();
        assert_eq!(&g, &[id]);
        let g: Vec<_> = db.lock()
                          .sample_file_dirs_by_id()
                          .get(&sample_file_dir_id)
                          .unwrap()
                          .garbage_unlinked
                          .iter()
                          .map(|&id| id)
                          .collect();
        assert_eq!(&g, &[]);
    }
}
