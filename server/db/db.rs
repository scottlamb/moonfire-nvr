// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Database access logic for the Moonfire NVR SQLite schema.
//!
//! The SQLite schema includes everything except the actual video samples (see the `dir` module
//! for management of those). See `schema.sql` for a more detailed description.
//!
//! The [`Database`] struct caches data in RAM, making the assumption that only one process is
//! accessing the database at a time. Performance and efficiency notes:
//!
//! *   several query operations here feature row callbacks. The callback is invoked with
//!     the database lock. Thus, the callback shouldn't perform long-running operations.
//!
//! *   startup may be slow, as it scans the entire index for the recording table. This seems
//!     acceptable.
//!
//! *   the operations used for web file serving should return results with acceptable latency.
//!
//! *   however, the database lock may be held for longer than is acceptable for
//!     the critical path of recording frames. The caller should preallocate sample file uuids
//!     and such to avoid database operations in these paths.
//!
//! *   adding and removing recordings done during normal operations use a batch interface.
//!     A list of mutations is built up in-memory and occasionally flushed to reduce SSD write
//!     cycles.

use crate::auth;
use crate::days;
use crate::dir;
use crate::json::SampleFileDirConfig;
use crate::raw;
use crate::recording;
use crate::schema;
use crate::signal;
use base::clock::{self, Clocks};
use base::strutil::encode_size;
use base::{bail, err, Error};
use base::{FastHashMap, FastHashSet};
use hashlink::LinkedHashMap;
use itertools::Itertools;
use rusqlite::{named_params, params};
use smallvec::SmallVec;
use std::cell::RefCell;
use std::cmp;
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::mem;
use std::ops::Range;
use std::path::PathBuf;
use std::str;
use std::string::String;
use std::sync::Arc;
use std::sync::{Mutex, MutexGuard};
use std::vec::Vec;
use tracing::warn;
use tracing::{error, info, trace};
use uuid::Uuid;

/// Expected schema version. See `guide/schema.md` for more information.
pub const EXPECTED_SCHEMA_VERSION: i32 = 7;

/// Length of the video index cache.
/// The actual data structure is one bigger than this because we insert before we remove.
/// Make it one less than a power of two so that the data structure's size is efficient.
const VIDEO_INDEX_CACHE_LEN: usize = 1023;

const GET_RECORDING_PLAYBACK_SQL: &str = r#"
    select
      video_index
    from
      recording_playback
    where
      composite_id = :composite_id
"#;

const INSERT_VIDEO_SAMPLE_ENTRY_SQL: &str = r#"
    insert into video_sample_entry (width,  height,  pasp_h_spacing,  pasp_v_spacing,
                                    rfc6381_codec, data)
                            values (:width, :height, :pasp_h_spacing, :pasp_v_spacing,
                                    :rfc6381_codec, :data)
"#;

const UPDATE_STREAM_COUNTERS_SQL: &str = r#"
    update stream
    set cum_recordings = :cum_recordings,
        cum_media_duration_90k = :cum_media_duration_90k,
        cum_runs = :cum_runs
    where id = :stream_id
"#;

/// The size of a filesystem block, to use in disk space accounting.
/// This should really be obtained by a stat call on the sample file directory in question,
/// but that requires some refactoring. See
/// [#89](https://github.com/scottlamb/moonfire-nvr/issues/89). We might be able to get away with
/// this hardcoded value for a while.
const ASSUMED_BLOCK_SIZE_BYTES: i64 = 4096;

/// Rounds a file size up to the next multiple of the block size.
/// This is useful in representing the actual amount of filesystem space used.
pub(crate) fn round_up(bytes: i64) -> i64 {
    let blk = ASSUMED_BLOCK_SIZE_BYTES;
    (bytes + blk - 1) / blk * blk
}

/// A wrapper around `Uuid` which implements `FromSql` and `ToSql`.
pub struct SqlUuid(pub Uuid);

impl rusqlite::types::FromSql for SqlUuid {
    fn column_result(value: rusqlite::types::ValueRef) -> rusqlite::types::FromSqlResult<Self> {
        let uuid = Uuid::from_slice(value.as_blob()?)
            .map_err(|e| rusqlite::types::FromSqlError::Other(Box::new(e)))?;
        Ok(SqlUuid(uuid))
    }
}

impl rusqlite::types::ToSql for SqlUuid {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.0.as_bytes()[..].into())
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
    pub id: i32,

    // Fields matching VideoSampleEntryToInsert below.
    pub data: Vec<u8>,
    pub rfc6381_codec: String,
    pub width: u16,
    pub height: u16,
    pub pasp_h_spacing: u16,
    pub pasp_v_spacing: u16,
}

impl VideoSampleEntry {
    /// Returns the aspect ratio as a minimized ratio.
    pub fn aspect(&self) -> num_rational::Ratio<u32> {
        num_rational::Ratio::new(
            u32::from(self.width) * u32::from(self.pasp_h_spacing),
            u32::from(self.height) * u32::from(self.pasp_v_spacing),
        )
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct VideoSampleEntryToInsert {
    pub data: Vec<u8>,
    pub rfc6381_codec: String,
    pub width: u16,
    pub height: u16,
    pub pasp_h_spacing: u16,
    pub pasp_v_spacing: u16,
}

impl std::fmt::Debug for VideoSampleEntryToInsert {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        use pretty_hex::PrettyHex;
        f.debug_struct("VideoSampleEntryToInsert")
            .field("data", &self.data.hex_dump())
            .field("rfc6381_codec", &self.rfc6381_codec)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pasp_h_spacing", &self.pasp_h_spacing)
            .field("pasp_v_spacing", &self.pasp_v_spacing)
            .finish()
    }
}

/// A row used in `list_recordings_by_time` and `list_recordings_by_id`.
#[derive(Clone, Debug)]
pub struct ListRecordingsRow {
    pub start: recording::Time,
    pub video_sample_entry_id: i32,

    pub id: CompositeId,

    /// This is a recording::Duration, but a single recording's duration fits into an i32.
    pub wall_duration_90k: i32,
    pub media_duration_90k: i32,
    pub video_samples: i32,
    pub video_sync_samples: i32,
    pub sample_file_bytes: i32,
    pub run_offset: i32,
    pub open_id: u32,
    pub flags: i32,

    /// This is populated by `list_recordings_by_id` but not `list_recordings_by_time`.
    /// (It's not included in the `recording_cover` index, so adding it to
    /// `list_recordings_by_time` would be inefficient.)
    pub prev_media_duration_and_runs: Option<(recording::Duration, i32)>,
    pub end_reason: Option<String>,
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
    pub has_trailing_zero: bool,
    pub end_reason: Option<String>,
}

impl ListAggregatedRecordingsRow {
    fn from(row: ListRecordingsRow) -> Self {
        let recording_id = row.id.recording();
        let uncommitted = (row.flags & RecordingFlags::Uncommitted as i32) != 0;
        let growing = (row.flags & RecordingFlags::Growing as i32) != 0;
        ListAggregatedRecordingsRow {
            time: row.start..recording::Time(row.start.0 + row.wall_duration_90k as i64),
            ids: recording_id..recording_id + 1,
            video_samples: row.video_samples as i64,
            video_sync_samples: row.video_sync_samples as i64,
            sample_file_bytes: row.sample_file_bytes as i64,
            video_sample_entry_id: row.video_sample_entry_id,
            stream_id: row.id.stream(),
            run_start_id: recording_id - row.run_offset,
            open_id: row.open_id,
            first_uncommitted: if uncommitted {
                Some(recording_id)
            } else {
                None
            },
            growing,
            has_trailing_zero: (row.flags & RecordingFlags::TrailingZero as i32) != 0,
            end_reason: row.end_reason,
        }
    }
}

/// Select fields from the `recordings_playback` table. Retrieve with `with_recording_playback`.
#[derive(Debug)]
pub struct RecordingPlayback<'a> {
    pub video_index: &'a [u8],
}

/// Bitmask in the `flags` field in the `recordings` table; see `schema.sql`.
#[repr(u32)]
pub enum RecordingFlags {
    TrailingZero = 1,

    // These values (starting from high bit on down) are never written to the database.
    Growing = 1 << 30,
    Uncommitted = 1 << 31,
}

/// A recording to pass to `LockedDatabase::add_recording` and `raw::insert_recording`.
#[derive(Clone, Debug, Default)]
pub struct RecordingToInsert {
    pub run_offset: i32,
    pub flags: i32,
    pub sample_file_bytes: i32,
    pub start: recording::Time,

    /// Filled in by `add_recording`.
    pub prev_media_duration: recording::Duration,

    /// Filled in by `add_recording`.
    pub prev_runs: i32,

    pub wall_duration_90k: i32, // a recording::Duration, but guaranteed to fit in i32.
    pub media_duration_90k: i32,
    pub local_time_delta: recording::Duration,
    pub video_samples: i32,
    pub video_sync_samples: i32,
    pub video_sample_entry_id: i32,
    pub video_index: Vec<u8>,
    pub sample_file_blake3: Option<[u8; 32]>,
    pub end_reason: Option<String>,
}

impl RecordingToInsert {
    fn to_list_row(&self, id: CompositeId, open_id: u32) -> ListRecordingsRow {
        ListRecordingsRow {
            start: self.start,
            video_sample_entry_id: self.video_sample_entry_id,
            id,
            wall_duration_90k: self.wall_duration_90k,
            media_duration_90k: self.media_duration_90k,
            video_samples: self.video_samples,
            video_sync_samples: self.video_sync_samples,
            sample_file_bytes: self.sample_file_bytes,
            run_offset: self.run_offset,
            open_id,
            flags: self.flags | RecordingFlags::Uncommitted as i32,
            prev_media_duration_and_runs: Some((self.prev_media_duration, self.prev_runs)),
            end_reason: self.end_reason.clone(),
        }
    }
}

/// A row used in `raw::list_oldest_recordings` and `db::delete_oldest_recordings`.
#[derive(Copy, Clone, Debug)]
pub(crate) struct ListOldestRecordingsRow {
    pub id: CompositeId,
    pub start: recording::Time,
    pub wall_duration_90k: i32,
    pub sample_file_bytes: i32,
}

#[derive(Debug)]
pub struct SampleFileDir {
    pub id: i32,
    pub path: PathBuf,
    pub uuid: Uuid,
    dir: Option<Arc<dir::SampleFileDir>>,
    last_complete_open: Option<Open>,

    /// ids which are in the `garbage` database table (rather than `recording`) as of last commit
    /// but may still exist on disk. These can't be safely removed from the database yet.
    pub(crate) garbage_needs_unlink: FastHashSet<CompositeId>,

    /// ids which are in the `garbage` database table and are guaranteed to no longer exist on
    /// disk (have been unlinked and the dir has been synced). These may be removed from the
    /// database on next flush. Mutually exclusive with `garbage_needs_unlink`.
    pub(crate) garbage_unlinked: Vec<CompositeId>,
}

impl SampleFileDir {
    /// Returns a cloned copy of the directory, or Err if closed.
    ///
    /// Use `LockedDatabase::open_sample_file_dirs` prior to calling this method.
    pub fn get(&self) -> Result<Arc<dir::SampleFileDir>, base::Error> {
        Ok(self
            .dir
            .as_ref()
            .ok_or_else(|| {
                err!(
                    FailedPrecondition,
                    msg("sample file dir {} is closed", self.id)
                )
            })?
            .clone())
    }

    /// Returns expected existing metadata when opening this directory.
    fn expected_meta(&self, db_uuid: &Uuid) -> schema::DirMeta {
        let mut meta = schema::DirMeta::default();
        meta.db_uuid.extend_from_slice(&db_uuid.as_bytes()[..]);
        meta.dir_uuid.extend_from_slice(&self.uuid.as_bytes()[..]);
        if let Some(o) = self.last_complete_open {
            let open = meta.last_complete_open.mut_or_insert_default();
            open.id = o.id;
            open.uuid.extend_from_slice(&o.uuid.as_bytes()[..]);
        }
        meta
    }
}

pub use crate::auth::RawSessionId;
pub use crate::auth::Request;
pub use crate::auth::Session;
pub use crate::auth::User;
pub use crate::auth::UserChange;

/// In-memory state about a camera.
#[derive(Debug)]
pub struct Camera {
    pub id: i32,
    pub uuid: Uuid,
    pub short_name: String,
    pub config: crate::json::CameraConfig,
    pub streams: [Option<i32>; NUM_STREAM_TYPES],
}

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
    pub id: i32,
    pub camera_id: i32,
    pub sample_file_dir_id: Option<i32>,
    pub type_: StreamType,
    pub config: crate::json::StreamConfig,

    /// The time range of recorded data associated with this stream (minimum start time and maximum
    /// end time). `None` iff there are no recordings for this camera.
    pub range: Option<Range<recording::Time>>,

    /// The total bytes of flushed sample files. This doesn't include disk space wasted in the
    /// last filesystem block allocated to each file ("internal fragmentation").
    pub sample_file_bytes: i64,

    /// The total bytes on the filesystem used by this stream. This slightly more than
    /// `sample_file_bytes` because it includes the wasted space in the last filesystem block.
    pub fs_bytes: i64,

    /// On flush, delete the following recordings (move them to the `garbage` table, to be
    /// collected later). Note they must be the oldest recordings. The later collection involves
    /// the syncer unlinking the files on disk and syncing the directory then enqueueing for
    /// another following flush removal from the `garbage` table.
    to_delete: Vec<ListOldestRecordingsRow>,

    /// The total bytes to delete with the next flush.
    pub bytes_to_delete: i64,
    pub fs_bytes_to_delete: i64,

    /// The total bytes to add with the next flush. (`mark_synced` has already been called on these
    /// recordings.)
    pub bytes_to_add: i64,
    pub fs_bytes_to_add: i64,

    /// The total duration of undeleted recorded data. This may not be `range.end - range.start`
    /// due to gaps and overlap.
    pub duration: recording::Duration,

    /// Mapping of calendar day (in the server's time zone) to a summary of committed recordings on
    /// that day.
    pub committed_days: days::Map<days::StreamValue>,

    /// The `cum_recordings` currently committed to the database.
    pub(crate) cum_recordings: i32,

    /// The `cum_media_duration_90k` currently committed to the database.
    cum_media_duration: recording::Duration,

    /// The `cum_runs` currently committed to the database.
    cum_runs: i32,

    /// The recordings which have been added via `LockedDatabase::add_recording` but have yet to
    /// committed to the database.
    ///
    /// `uncommitted[i]` uses sample filename `CompositeId::new(id, cum_recordings + i)`;
    /// `cum_recordings` should be advanced when one is committed to maintain this invariant.
    ///
    /// TODO: alter the serving path to show these just as if they were already committed.
    uncommitted: VecDeque<Arc<Mutex<RecordingToInsert>>>,

    /// The number of recordings in `uncommitted` which are synced and ready to commit.
    synced_recordings: usize,

    on_live_segment: Vec<Box<dyn FnMut(LiveSegment) -> bool + Send>>,
}

/// Bounds of a live view segment. Currently this is a single frame of video.
/// This is used for live stream recordings. The stream id should already be known to the
/// subscriber. Note this doesn't actually contain the video, just a reference that can be
/// looked up within the database.
#[derive(Clone, Debug)]
pub struct LiveSegment {
    pub recording: i32,

    /// If the segment's one frame is a key frame.
    pub is_key: bool,

    /// The pts, relative to the start of the recording, of the start and end of this live segment,
    /// in 90kHz units.
    pub media_off_90k: Range<i32>,
}

#[derive(Clone, Debug, Default)]
pub struct StreamChange {
    pub sample_file_dir_id: Option<i32>,
    pub config: crate::json::StreamConfig,
}

/// Information about a camera, used by `add_camera` and `update_camera`.
#[derive(Clone, Debug, Default)]
pub struct CameraChange {
    pub short_name: String,
    pub config: crate::json::CameraConfig,

    /// `StreamType t` is represented by `streams[t.index()]`. A default StreamChange will
    /// correspond to no stream in the database, provided there are no existing recordings for that
    /// stream.
    pub streams: [StreamChange; NUM_STREAM_TYPES],
}

impl Stream {
    /// Adds a single fully committed recording with the given properties to the in-memory state.
    fn add_recording(&mut self, r: Range<recording::Time>, sample_file_bytes: i32) {
        self.range = Some(match self.range {
            Some(ref e) => cmp::min(e.start, r.start)..cmp::max(e.end, r.end),
            None => r.start..r.end,
        });
        self.duration += r.end - r.start;
        self.sample_file_bytes += i64::from(sample_file_bytes);
        self.fs_bytes += round_up(i64::from(sample_file_bytes));
        self.committed_days.adjust(r, 1);
    }

    /// Returns a days map including unflushed recordings.
    pub fn days(&self) -> days::Map<days::StreamValue> {
        let mut days = self.committed_days.clone();
        for u in &self.uncommitted {
            let l = u.lock().unwrap();
            days.adjust(
                l.start..l.start + recording::Duration(i64::from(l.wall_duration_90k)),
                1,
            );
        }
        days
    }
}

/// Initializes the recordings associated with the given camera.
fn init_recordings(
    conn: &mut rusqlite::Connection,
    stream_id: i32,
    camera: &Camera,
    stream: &mut Stream,
) -> Result<(), Error> {
    info!(
        "Loading recordings for camera {} stream {:?}",
        camera.short_name, stream.type_
    );
    let mut stmt = conn.prepare(
        r#"
        select
          recording.start_time_90k,
          recording.wall_duration_90k,
          recording.sample_file_bytes
        from
          recording
        where
          stream_id = :stream_id
        "#,
    )?;
    let mut rows = stmt.query(named_params! {":stream_id": stream_id})?;
    let mut i = 0;
    while let Some(row) = rows.next()? {
        let start = recording::Time(row.get(0)?);
        let duration = recording::Duration(row.get(1)?);
        let bytes = row.get(2)?;
        stream.add_recording(start..start + duration, bytes);
        i += 1;
    }
    info!(
        "Loaded {} recordings for camera {} stream {:?}",
        i, camera.short_name, stream.type_
    );
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
    cameras_by_uuid: BTreeMap<Uuid, i32>, // values are ids.
    video_sample_entries_by_id: BTreeMap<i32, Arc<VideoSampleEntry>>,
    video_index_cache: RefCell<LinkedHashMap<i64, Box<[u8]>, base::RandomState>>,
    on_flush: Vec<Box<dyn Fn() + Send>>,
}

/// Represents a row of the `open` database table.
#[derive(Copy, Clone, Debug)]
pub struct Open {
    pub id: u32,
    pub(crate) uuid: Uuid,
}

/// A combination of a stream id and recording id into a single 64-bit int.
/// This is used as a primary key in the SQLite `recording` table (see `schema.sql`)
/// and the sample file's name on disk (see `dir.rs`).
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct CompositeId(pub i64);

impl CompositeId {
    pub fn new(stream_id: i32, recording_id: i32) -> Self {
        CompositeId((stream_id as i64) << 32 | recording_id as i64)
    }

    pub fn stream(self) -> i32 {
        (self.0 >> 32) as i32
    }
    pub fn recording(self) -> i32 {
        self.0 as i32
    }
}

impl ::std::fmt::Display for CompositeId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> Result<(), ::std::fmt::Error> {
        write!(f, "{}/{}", self.stream(), self.recording())
    }
}

/// Inserts, updates, or removes streams in the `State` object to match a set of `StreamChange`
/// structs.
struct StreamStateChanger {
    sids: [Option<i32>; NUM_STREAM_TYPES],

    /// For each stream to change, a (stream_id, upsert or `None` to delete) tuple.
    streams: Vec<(i32, Option<StreamStateChangerUpsert>)>,
}

/// Upsert state used internally within [`StreamStateChanger`].
struct StreamStateChangerUpsert {
    camera_id: i32,
    type_: StreamType,
    sc: StreamChange,
}

impl StreamStateChanger {
    /// Performs the database updates (guarded by the given transaction) and returns the state
    /// change to be applied on successful commit.
    fn new(
        tx: &rusqlite::Transaction,
        camera_id: i32,
        existing: Option<&Camera>,
        streams_by_id: &BTreeMap<i32, Stream>,
        change: &mut CameraChange,
    ) -> Result<Self, Error> {
        let mut sids = [None; NUM_STREAM_TYPES];
        let mut streams = Vec::with_capacity(NUM_STREAM_TYPES);
        let existing_streams = existing.map(|e| e.streams).unwrap_or_default();
        for (i, ref mut sc) in change.streams.iter_mut().enumerate() {
            let type_ = StreamType::from_index(i).unwrap();
            let mut have_data = false;
            if let Some(sid) = existing_streams[i] {
                let s = streams_by_id.get(&sid).unwrap();
                if s.range.is_some() {
                    have_data = true;
                    if let (Some(d), false) = (
                        s.sample_file_dir_id,
                        s.sample_file_dir_id == sc.sample_file_dir_id,
                    ) {
                        bail!(
                            FailedPrecondition,
                            msg(
                                "can't change sample_file_dir_id {:?}->{:?} for non-empty stream {}",
                                d,
                                sc.sample_file_dir_id,
                                sid,
                            ),
                        );
                    }
                }
                if !have_data && sc.config.is_empty() && sc.sample_file_dir_id.is_none() {
                    // Delete stream.
                    let mut stmt = tx.prepare_cached(
                        r#"
                        delete from stream where id = ?
                        "#,
                    )?;
                    if stmt.execute(params![sid])? != 1 {
                        bail!(Internal, msg("missing stream {sid}"));
                    }
                    streams.push((sid, None));
                } else {
                    // Update stream.
                    let mut stmt = tx.prepare_cached(
                        r#"
                        update stream set
                            config = :config,
                            sample_file_dir_id = :sample_file_dir_id
                        where
                            id = :id
                        "#,
                    )?;
                    let rows = stmt.execute(named_params! {
                        ":config": &sc.config,
                        ":sample_file_dir_id": sc.sample_file_dir_id,
                        ":id": sid,
                    })?;
                    if rows != 1 {
                        bail!(Internal, msg("missing stream {sid}"));
                    }
                    sids[i] = Some(sid);
                    streams.push((
                        sid,
                        Some(StreamStateChangerUpsert {
                            camera_id,
                            type_,
                            sc: mem::take(*sc),
                        }),
                    ));
                }
            } else {
                if sc.config.is_empty() && sc.sample_file_dir_id.is_none() {
                    // Do nothing; there is no record and we want to keep it that way.
                    continue;
                }
                // Insert stream.
                let mut stmt = tx.prepare_cached(
                    r#"
                    insert into stream (camera_id,  sample_file_dir_id,  type,  config,
                                        cum_recordings,  cum_media_duration_90k,  cum_runs)
                                values (:camera_id, :sample_file_dir_id, :type, :config,
                                        0,               0,                       0)
                    "#,
                )?;
                stmt.execute(named_params! {
                    ":camera_id": camera_id,
                    ":sample_file_dir_id": sc.sample_file_dir_id,
                    ":type": type_.as_str(),
                    ":config": &sc.config,
                })?;
                let id = tx.last_insert_rowid() as i32;
                sids[i] = Some(id);
                streams.push((
                    id,
                    Some(StreamStateChangerUpsert {
                        camera_id,
                        type_,
                        sc: mem::take(*sc),
                    }),
                ));
            }
        }
        Ok(StreamStateChanger { sids, streams })
    }

    /// Applies the change to the given `streams_by_id`. The caller is expected to set
    /// `Camera::streams` to the return value.
    fn apply(
        mut self,
        streams_by_id: &mut BTreeMap<i32, Stream>,
    ) -> [Option<i32>; NUM_STREAM_TYPES] {
        for (id, stream) in self.streams.drain(..) {
            use ::std::collections::btree_map::Entry;
            match (streams_by_id.entry(id), stream) {
                (
                    Entry::Vacant(e),
                    Some(StreamStateChangerUpsert {
                        camera_id,
                        type_,
                        sc,
                    }),
                ) => {
                    e.insert(Stream {
                        id,
                        type_,
                        camera_id,
                        sample_file_dir_id: sc.sample_file_dir_id,
                        config: sc.config,
                        range: None,
                        sample_file_bytes: 0,
                        fs_bytes: 0,
                        to_delete: Vec::new(),
                        bytes_to_delete: 0,
                        fs_bytes_to_delete: 0,
                        bytes_to_add: 0,
                        fs_bytes_to_add: 0,
                        duration: recording::Duration(0),
                        committed_days: days::Map::default(),
                        cum_recordings: 0,
                        cum_media_duration: recording::Duration(0),
                        cum_runs: 0,
                        uncommitted: VecDeque::new(),
                        synced_recordings: 0,
                        on_live_segment: Vec::new(),
                    });
                }
                (Entry::Vacant(_), None) => {}
                (Entry::Occupied(e), Some(StreamStateChangerUpsert { sc, .. })) => {
                    let e = e.into_mut();
                    e.sample_file_dir_id = sc.sample_file_dir_id;
                    e.config = sc.config;
                }
                (Entry::Occupied(e), None) => {
                    e.remove();
                }
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
    pub fn cameras_by_id(&self) -> &BTreeMap<i32, Camera> {
        &self.cameras_by_id
    }
    pub fn sample_file_dirs_by_id(&self) -> &BTreeMap<i32, SampleFileDir> {
        &self.sample_file_dirs_by_id
    }

    /// Returns the number of completed database flushes since startup.
    pub fn flushes(&self) -> usize {
        self.flush_count
    }

    /// Adds a placeholder for an uncommitted recording.
    ///
    /// The caller should write samples and fill the returned `RecordingToInsert` as it goes
    /// (noting that while holding the lock, it should not perform I/O or acquire the database
    /// lock). Then it should sync to permanent storage and call `mark_synced`. The data will
    /// be written to the database on the next `flush`.
    ///
    /// A call to `add_recording` is also a promise that previous recordings (even if not yet
    /// synced and committed) won't change.
    ///
    /// This fills the `prev_media_duration` and `prev_runs` fields.
    pub(crate) fn add_recording(
        &mut self,
        stream_id: i32,
        mut r: RecordingToInsert,
    ) -> Result<(CompositeId, Arc<Mutex<RecordingToInsert>>), Error> {
        let stream = match self.streams_by_id.get_mut(&stream_id) {
            None => bail!(FailedPrecondition, msg("no such stream {stream_id}")),
            Some(s) => s,
        };
        let id = CompositeId::new(
            stream_id,
            stream.cum_recordings + (stream.uncommitted.len() as i32),
        );
        match stream.uncommitted.back() {
            Some(s) => {
                let l = s.lock().unwrap();
                r.prev_media_duration =
                    l.prev_media_duration + recording::Duration(l.media_duration_90k.into());
                r.prev_runs = l.prev_runs + if l.run_offset == 0 { 1 } else { 0 };
            }
            None => {
                r.prev_media_duration = stream.cum_media_duration;
                r.prev_runs = stream.cum_runs;
            }
        };
        let recording = Arc::new(Mutex::new(r));
        stream.uncommitted.push_back(Arc::clone(&recording));
        Ok((id, recording))
    }

    /// Marks the given uncomitted recording as synced and ready to flush.
    /// This must be the next unsynced recording.
    pub(crate) fn mark_synced(&mut self, id: CompositeId) -> Result<(), Error> {
        let stream = match self.streams_by_id.get_mut(&id.stream()) {
            None => bail!(FailedPrecondition, msg("no stream for recording {id}")),
            Some(s) => s,
        };
        let next_unsynced = stream.cum_recordings + (stream.synced_recordings as i32);
        if id.recording() != next_unsynced {
            bail!(
                FailedPrecondition,
                msg(
                    "can't sync {} when next unsynced recording is {} (next unflushed is {})",
                    id,
                    next_unsynced,
                    stream.cum_recordings,
                ),
            );
        }
        if stream.synced_recordings == stream.uncommitted.len() {
            bail!(
                FailedPrecondition,
                msg("can't sync un-added recording {id}")
            );
        }
        let l = stream.uncommitted[stream.synced_recordings].lock().unwrap();
        let bytes = i64::from(l.sample_file_bytes);
        stream.bytes_to_add += bytes;
        stream.fs_bytes_to_add += round_up(bytes);
        stream.synced_recordings += 1;
        Ok(())
    }

    pub(crate) fn delete_garbage(
        &mut self,
        dir_id: i32,
        ids: &mut Vec<CompositeId>,
    ) -> Result<(), Error> {
        let dir = match self.sample_file_dirs_by_id.get_mut(&dir_id) {
            None => bail!(FailedPrecondition, msg("no such dir {dir_id}")),
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
            bail!(
                FailedPrecondition,
                msg("delete_garbage with non-garbage ids {:?}", &ids[..])
            );
        }
        Ok(())
    }

    /// Registers a callback to run on every live segment immediately after it's recorded.
    /// The callback is run with the database lock held, so it must not call back into the database
    /// or block. The callback should return false to unregister.
    pub fn watch_live(
        &mut self,
        stream_id: i32,
        cb: Box<dyn FnMut(LiveSegment) -> bool + Send>,
    ) -> Result<(), Error> {
        let s = match self.streams_by_id.get_mut(&stream_id) {
            None => bail!(NotFound, msg("no such stream {stream_id}")),
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
        for s in self.streams_by_id.values_mut() {
            s.on_live_segment.clear();
        }
    }

    pub(crate) fn send_live_segment(&mut self, stream: i32, l: LiveSegment) -> Result<(), Error> {
        let s = match self.streams_by_id.get_mut(&stream) {
            None => bail!(Internal, msg("no such stream {stream}")),
            Some(s) => s,
        };

        // TODO: use std's retain_mut after it's available in our minimum supported Rust version.
        // <https://github.com/rust-lang/rust/issues/48919>
        odds::vec::VecExt::retain_mut(&mut s.on_live_segment, |cb| cb(l.clone()));
        Ok(())
    }

    /// Helper for `DatabaseGuard::flush()` and `Database::drop()`.
    ///
    /// The public API is in `DatabaseGuard::flush()`; it supplies the `Clocks` to this function.
    fn flush<C: Clocks>(&mut self, clocks: &C, reason: &str) -> Result<(), Error> {
        let span = tracing::info_span!("flush", flush_count = self.flush_count, reason);
        let _enter = span.enter();
        let o = match self.open.as_ref() {
            None => bail!(Internal, msg("database is read-only")),
            Some(o) => o,
        };
        let tx = self.conn.transaction()?;
        let mut new_ranges =
            FastHashMap::with_capacity_and_hasher(self.streams_by_id.len(), Default::default());
        {
            let mut stmt = tx.prepare_cached(UPDATE_STREAM_COUNTERS_SQL)?;
            for (&stream_id, s) in &self.streams_by_id {
                // Process additions.
                let mut new_duration = 0;
                let mut new_runs = 0;
                for i in 0..s.synced_recordings {
                    let l = s.uncommitted[i].lock().unwrap();
                    raw::insert_recording(
                        &tx,
                        o,
                        CompositeId::new(stream_id, s.cum_recordings + i as i32),
                        &l,
                    )?;
                    new_duration += i64::from(l.wall_duration_90k);
                    new_runs += if l.run_offset == 0 { 1 } else { 0 };
                }
                if s.synced_recordings > 0 {
                    new_ranges.entry(stream_id).or_insert(None);
                    stmt.execute(named_params! {
                        ":stream_id": stream_id,
                        ":cum_recordings": s.cum_recordings + s.synced_recordings as i32,
                        ":cum_media_duration_90k": s.cum_media_duration.0 + new_duration,
                        ":cum_runs": s.cum_runs + new_runs,
                    })?;
                }

                // Process deletions.
                if let Some(l) = s.to_delete.last() {
                    new_ranges.entry(stream_id).or_insert(None);
                    let dir = match s.sample_file_dir_id {
                        None => bail!(Internal, msg("stream {stream_id} has no directory!")),
                        Some(d) => d,
                    };

                    // raw::delete_recordings does a bulk transfer of a range from recording to
                    // garbage, rather than operating on each element of to_delete. This is
                    // guaranteed to give the same result because to_delete is guaranteed to be the
                    // oldest recordings for the stream.
                    let start = CompositeId::new(stream_id, 0);
                    let end = CompositeId(l.id.0 + 1);
                    let n = raw::delete_recordings(&tx, dir, start..end)?;
                    if n != s.to_delete.len() {
                        bail!(
                            Internal,
                            msg(
                                "Found {} rows in {} .. {}, expected {}: {:?}",
                                n,
                                start,
                                end,
                                s.to_delete.len(),
                                &s.to_delete,
                            ),
                        );
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
                r"update open set duration_90k = ?, end_time_90k = ? where id = ?",
            )?;
            let rows = stmt.execute(params![
                (recording::Time::new(clocks.monotonic()) - self.open_monotonic).0,
                recording::Time::new(clocks.realtime()).0,
                o.id,
            ])?;
            if rows != 1 {
                bail!(Internal, msg("unable to find current open {}", o.id));
            }
        }
        self.auth.flush(&tx)?;
        self.signal.flush(&tx)?;
        tx.commit()?;

        #[derive(Default)]
        struct DirLog {
            added: SmallVec<[CompositeId; 32]>,
            deleted: SmallVec<[CompositeId; 32]>,
            gced: SmallVec<[CompositeId; 32]>,
            added_bytes: i64,
            deleted_bytes: i64,
        }
        let mut dir_logs: FastHashMap<i32, DirLog> = FastHashMap::default();

        // Process delete_garbage.
        for (&id, dir) in &mut self.sample_file_dirs_by_id {
            if !dir.garbage_unlinked.is_empty() {
                dir_logs
                    .entry(id)
                    .or_default()
                    .gced
                    .extend(dir.garbage_unlinked.drain(..));
            }
        }

        for (stream_id, new_range) in new_ranges.drain() {
            let s = self.streams_by_id.get_mut(&stream_id).unwrap();
            let dir_id = s.sample_file_dir_id.unwrap();
            let dir = self.sample_file_dirs_by_id.get_mut(&dir_id).unwrap();
            let log = dir_logs.entry(dir_id).or_default();

            // Process delete_oldest_recordings.
            s.sample_file_bytes -= s.bytes_to_delete;
            s.fs_bytes -= s.fs_bytes_to_delete;
            log.deleted_bytes += s.bytes_to_delete;
            s.bytes_to_delete = 0;
            s.fs_bytes_to_delete = 0;
            log.deleted.reserve(s.to_delete.len());
            for row in s.to_delete.drain(..) {
                log.deleted.push(row.id);
                dir.garbage_needs_unlink.insert(row.id);
                let d = recording::Duration(i64::from(row.wall_duration_90k));
                s.duration -= d;
                s.committed_days.adjust(row.start..row.start + d, -1);
            }

            // Process add_recordings.
            log.added_bytes += s.bytes_to_add;
            s.bytes_to_add = 0;
            s.fs_bytes_to_add = 0;
            log.added.reserve(s.synced_recordings);
            for _ in 0..s.synced_recordings {
                let u = s.uncommitted.pop_front().unwrap();
                log.added
                    .push(CompositeId::new(stream_id, s.cum_recordings));
                let l = u.lock().unwrap();
                s.cum_recordings += 1;
                let wall_dur = recording::Duration(l.wall_duration_90k.into());
                let media_dur = recording::Duration(l.media_duration_90k.into());
                s.cum_media_duration += media_dur;
                s.cum_runs += if l.run_offset == 0 { 1 } else { 0 };
                let end = l.start + wall_dur;
                s.add_recording(l.start..end, l.sample_file_bytes);
            }
            s.synced_recordings = 0;

            // Fix the range.
            s.range = new_range;
        }
        self.auth.post_flush();
        self.signal.post_flush();
        self.flush_count += 1;
        let mut log_msg = String::with_capacity(256);
        for (&dir_id, log) in &dir_logs {
            let dir = self.sample_file_dirs_by_id.get(&dir_id).unwrap();
            write!(
                &mut log_msg,
                "\n{}: added {}B in {} recordings ({}), deleted {}B in {} ({}), \
                   GCed {} recordings ({}).",
                dir.path.display(),
                &encode_size(log.added_bytes),
                log.added.len(),
                log.added.iter().join(", "),
                &encode_size(log.deleted_bytes),
                log.deleted.len(),
                log.deleted.iter().join(", "),
                log.gced.len(),
                log.gced.iter().join(", ")
            )
            .unwrap();
        }
        if log_msg.is_empty() {
            log_msg.push_str(" no recording changes");
        }
        info!("flush complete: {log_msg}");
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
        let mut in_progress = FastHashMap::with_capacity_and_hasher(ids.len(), Default::default());
        for &id in ids {
            let e = in_progress.entry(id);
            use ::std::collections::hash_map::Entry;
            let e = match e {
                Entry::Occupied(_) => continue, // suppress duplicate.
                Entry::Vacant(e) => e,
            };
            let dir = self
                .sample_file_dirs_by_id
                .get_mut(&id)
                .ok_or_else(|| err!(NotFound, msg("no such dir {id}")))?;
            if dir.dir.is_some() {
                continue;
            }
            let mut expected_meta = dir.expected_meta(&self.uuid);
            if let Some(o) = self.open.as_ref() {
                let open = expected_meta.in_progress_open.mut_or_insert_default();
                open.id = o.id;
                open.uuid.extend_from_slice(&o.uuid.as_bytes()[..]);
            }
            let d = dir::SampleFileDir::open(&dir.path, &expected_meta)
                .map_err(|e| err!(e, msg("Failed to open dir {}", dir.path.display())))?;
            if self.open.is_none() {
                // read-only mode; it's already fully opened.
                dir.dir = Some(d);
            } else {
                // read-write mode; there are more steps to do.
                e.insert((expected_meta, d));
            }
        }

        let o = match self.open.as_ref() {
            None => return Ok(()), // read-only mode; all done.
            Some(o) => o,
        };

        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                r#"
                update sample_file_dir set last_complete_open_id = ? where id = ?
                "#,
            )?;
            for &id in in_progress.keys() {
                if stmt.execute(params![o.id, id])? != 1 {
                    bail!(Internal, msg("unable to update dir {id}"));
                }
            }
        }
        tx.commit()?;

        for (id, (mut meta, d)) in in_progress.drain() {
            let dir = self.sample_file_dirs_by_id.get_mut(&id).unwrap();
            meta.last_complete_open = meta.in_progress_open.take().into();
            d.write_meta(&meta)?;
            dir.dir = Some(d);
        }

        Ok(())
    }

    pub fn streams_by_id(&self) -> &BTreeMap<i32, Stream> {
        &self.streams_by_id
    }

    /// Returns an immutable view of the video sample entries.
    pub fn video_sample_entries_by_id(&self) -> &BTreeMap<i32, Arc<VideoSampleEntry>> {
        &self.video_sample_entries_by_id
    }

    /// Gets a given camera by uuid.
    pub fn get_camera(&self, uuid: Uuid) -> Option<&Camera> {
        self.cameras_by_uuid.get(&uuid).map(|id| {
            self.cameras_by_id
                .get(id)
                .expect("uuid->id requires id->cam")
        })
    }

    /// Lists the specified recordings, passing them to a supplied function. Given that the
    /// function is called with the database lock held, it should be quick.
    ///
    /// Note that at present, the returned recordings are _not_ completely ordered by start time.
    /// Uncommitted recordings are returned id order after the others.
    pub fn list_recordings_by_time(
        &self,
        stream_id: i32,
        desired_time: Range<recording::Time>,
        f: &mut dyn FnMut(ListRecordingsRow) -> Result<(), base::Error>,
    ) -> Result<(), base::Error> {
        let s = match self.streams_by_id.get(&stream_id) {
            None => bail!(NotFound, msg("no such stream {stream_id}")),
            Some(s) => s,
        };
        raw::list_recordings_by_time(&self.conn, stream_id, desired_time.clone(), f)?;
        for (i, u) in s.uncommitted.iter().enumerate() {
            let row = {
                let l = u.lock().unwrap();
                if l.video_samples > 0 {
                    let end = l.start + recording::Duration(l.wall_duration_90k as i64);
                    if l.start > desired_time.end || end < desired_time.start {
                        continue; // there's no overlap with the requested range.
                    }
                    l.to_list_row(
                        CompositeId::new(stream_id, s.cum_recordings + i as i32),
                        self.open.unwrap().id,
                    )
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
        &self,
        stream_id: i32,
        desired_ids: Range<i32>,
        f: &mut dyn FnMut(ListRecordingsRow) -> Result<(), base::Error>,
    ) -> Result<(), base::Error> {
        let s = match self.streams_by_id.get(&stream_id) {
            None => bail!(NotFound, msg("no such stream {stream_id}")),
            Some(s) => s,
        };
        if desired_ids.start < s.cum_recordings {
            raw::list_recordings_by_id(&self.conn, stream_id, desired_ids.clone(), f)?;
        }
        if desired_ids.end > s.cum_recordings {
            let start = cmp::max(0, desired_ids.start - s.cum_recordings) as usize;
            let end = cmp::min(
                (desired_ids.end - s.cum_recordings) as usize,
                s.uncommitted.len(),
            );
            for i in start..end {
                let row = {
                    let l = s.uncommitted[i].lock().unwrap();
                    if l.video_samples > 0 {
                        l.to_list_row(
                            CompositeId::new(stream_id, s.cum_recordings + i as i32),
                            self.open.unwrap().id,
                        )
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
        &self,
        stream_id: i32,
        desired_time: Range<recording::Time>,
        forced_split: recording::Duration,
        f: &mut dyn FnMut(ListAggregatedRecordingsRow) -> Result<(), base::Error>,
    ) -> Result<(), base::Error> {
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
            let has_trailing_zero = (row.flags & RecordingFlags::TrailingZero as i32) != 0;
            use std::collections::btree_map::Entry;
            match aggs.entry(run_start_id) {
                Entry::Occupied(mut e) => {
                    let a = e.get_mut();
                    let new_dur = a.time.end - a.time.start
                        + recording::Duration(row.wall_duration_90k as i64);
                    let needs_flush = a.ids.end != recording_id
                        || row.video_sample_entry_id != a.video_sample_entry_id
                        || new_dur >= forced_split;
                    if needs_flush {
                        // flush then start a new entry.
                        f(std::mem::replace(a, ListAggregatedRecordingsRow::from(row)))?;
                    } else {
                        // append.
                        if a.time.end != row.start {
                            bail!(
                                Internal,
                                msg(
                                    "stream {} recording {} ends at {} but {} starts at {}",
                                    stream_id,
                                    a.ids.end - 1,
                                    a.time.end,
                                    row.id,
                                    row.start,
                                ),
                            );
                        }
                        if a.open_id != row.open_id {
                            bail!(
                                Internal,
                                msg(
                                    "stream {} recording {} has open id {} but {} has {}",
                                    stream_id,
                                    a.ids.end - 1,
                                    a.open_id,
                                    row.id,
                                    row.open_id,
                                ),
                            );
                        }
                        a.time.end.0 += row.wall_duration_90k as i64;
                        a.ids.end = recording_id + 1;
                        a.video_samples += row.video_samples as i64;
                        a.video_sync_samples += row.video_sync_samples as i64;
                        a.sample_file_bytes += row.sample_file_bytes as i64;
                        if uncommitted {
                            a.first_uncommitted = a.first_uncommitted.or(Some(recording_id));
                        }
                        a.growing = growing;
                        a.has_trailing_zero = has_trailing_zero;
                        a.end_reason = row.end_reason;
                    }
                }
                Entry::Vacant(e) => {
                    e.insert(ListAggregatedRecordingsRow::from(row));
                }
            }
            Ok(())
        })?;
        for a in aggs.into_values() {
            f(a)?;
        }
        Ok(())
    }

    /// Calls `f` with a single `recording_playback` row.
    /// Note the lock is held for the duration of `f`.
    /// This uses a LRU cache to reduce the number of retrievals from the database.
    pub fn with_recording_playback<R>(
        &self,
        id: CompositeId,
        f: &mut dyn FnMut(&RecordingPlayback) -> Result<R, Error>,
    ) -> Result<R, Error> {
        // Check for uncommitted path.
        let s = self
            .streams_by_id
            .get(&id.stream())
            .ok_or_else(|| err!(Internal, msg("no stream for {}", id)))?;
        if s.cum_recordings <= id.recording() {
            let i = id.recording() - s.cum_recordings;
            if i as usize >= s.uncommitted.len() {
                bail!(
                    Internal,
                    msg(
                        "no such recording {}; latest committed is {}, latest is {}",
                        id,
                        s.cum_recordings,
                        s.cum_recordings + s.uncommitted.len() as i32,
                    ),
                );
            }
            let l = s.uncommitted[i as usize].lock().unwrap();
            return f(&RecordingPlayback {
                video_index: &l.video_index,
            });
        }

        // Committed path.
        let mut cache = self.video_index_cache.borrow_mut();
        use hashlink::linked_hash_map::RawEntryMut;
        match cache.raw_entry_mut().from_key(&id.0) {
            RawEntryMut::Occupied(mut occupied) => {
                trace!("cache hit for recording {}", id);
                occupied.to_back();
                let video_index = occupied.get();
                f(&RecordingPlayback { video_index })
            }
            RawEntryMut::Vacant(vacant) => {
                trace!("cache miss for recording {}", id);
                let mut stmt = self.conn.prepare_cached(GET_RECORDING_PLAYBACK_SQL)?;
                let mut rows = stmt.query(named_params! {":composite_id": id.0})?;
                if let Some(row) = rows.next()? {
                    let video_index: VideoIndex = row.get(0)?;
                    let result = f(&RecordingPlayback {
                        video_index: &video_index.0[..],
                    });
                    vacant.insert(id.0, video_index.0);
                    if cache.len() > VIDEO_INDEX_CACHE_LEN {
                        cache.pop_front();
                    }
                    return result;
                }
                Err(err!(Internal, msg("no such recording {id}")))
            }
        }
    }

    /// Queues for deletion the oldest recordings that aren't already queued.
    /// `f` should return true for each row that should be deleted.
    pub(crate) fn delete_oldest_recordings(
        &mut self,
        stream_id: i32,
        f: &mut dyn FnMut(&ListOldestRecordingsRow) -> bool,
    ) -> Result<(), Error> {
        let s = match self.streams_by_id.get_mut(&stream_id) {
            None => bail!(Internal, msg("no stream {stream_id}")),
            Some(s) => s,
        };
        let end = match s.to_delete.last() {
            None => 0,
            Some(row) => row.id.recording() + 1,
        };
        raw::list_oldest_recordings(&self.conn, CompositeId::new(stream_id, end), &mut |r| {
            if f(&r) {
                s.to_delete.push(r);
                let bytes = i64::from(r.sample_file_bytes);
                s.bytes_to_delete += bytes;
                s.fs_bytes_to_delete += round_up(bytes);
                return true;
            }
            false
        })
    }

    /// Initializes the video_sample_entries. To be called during construction.
    fn init_video_sample_entries(&mut self) -> Result<(), Error> {
        info!("Loading video sample entries");
        let mut stmt = self.conn.prepare(
            r#"
            select
                id,
                width,
                height,
                pasp_h_spacing,
                pasp_v_spacing,
                rfc6381_codec,
                data
            from
                video_sample_entry
            "#,
        )?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            let data: Vec<u8> = row.get(6)?;
            let get_and_cvt = |i: usize| {
                let raw = row.get::<_, i32>(i)?;
                u16::try_from(raw).map_err(|e| err!(OutOfRange, source(e)))
            };
            self.video_sample_entries_by_id.insert(
                id,
                Arc::new(VideoSampleEntry {
                    id,
                    width: get_and_cvt(1)?,
                    height: get_and_cvt(2)?,
                    pasp_h_spacing: get_and_cvt(3)?,
                    pasp_v_spacing: get_and_cvt(4)?,
                    data,
                    rfc6381_codec: row.get(5)?,
                }),
            );
        }
        info!(
            "Loaded {} video sample entries",
            self.video_sample_entries_by_id.len()
        );
        Ok(())
    }

    /// Initializes the sample file dirs.
    /// To be called during construction.
    fn init_sample_file_dirs(&mut self) -> Result<(), Error> {
        info!("Loading sample file dirs");
        let mut stmt = self.conn.prepare(
            r#"
            select
              d.id,
              d.config,
              d.uuid,
              d.last_complete_open_id,
              o.uuid
            from
              sample_file_dir d left join open o on (d.last_complete_open_id = o.id);
            "#,
        )?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            let config: SampleFileDirConfig = row.get(1)?;
            let dir_uuid: SqlUuid = row.get(2)?;
            let open_id: Option<u32> = row.get(3)?;
            let open_uuid: Option<SqlUuid> = row.get(4)?;
            let last_complete_open = match (open_id, open_uuid) {
                (Some(id), Some(uuid)) => Some(Open { id, uuid: uuid.0 }),
                (None, None) => None,
                _ => bail!(Internal, msg("open table missing id {id}")),
            };
            self.sample_file_dirs_by_id.insert(
                id,
                SampleFileDir {
                    id,
                    uuid: dir_uuid.0,
                    path: config.path,
                    dir: None,
                    last_complete_open,
                    garbage_needs_unlink: raw::list_garbage(&self.conn, id)?,
                    garbage_unlinked: Vec::new(),
                },
            );
        }
        info!(
            "Loaded {} sample file dirs",
            self.sample_file_dirs_by_id.len()
        );
        Ok(())
    }

    /// Initializes the cameras, but not their matching recordings.
    /// To be called during construction.
    fn init_cameras(&mut self) -> Result<(), Error> {
        info!("Loading cameras");
        let mut stmt = self.conn.prepare(
            r#"
            select
              id,
              uuid,
              short_name,
              config
            from
              camera;
            "#,
        )?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            let uuid: SqlUuid = row.get(1)?;
            self.cameras_by_id.insert(
                id,
                Camera {
                    id,
                    uuid: uuid.0,
                    short_name: row.get(2)?,
                    config: row.get(3)?,
                    streams: Default::default(),
                },
            );
            self.cameras_by_uuid.insert(uuid.0, id);
        }
        info!("Loaded {} cameras", self.cameras_by_id.len());
        Ok(())
    }

    /// Initializes the streams, but not their matching recordings.
    /// To be called during construction.
    fn init_streams(&mut self) -> Result<(), Error> {
        info!("Loading streams");
        let mut stmt = self.conn.prepare(
            r#"
            select
              id,
              type,
              camera_id,
              sample_file_dir_id,
              config,
              cum_recordings,
              cum_media_duration_90k,
              cum_runs
            from
              stream;
            "#,
        )?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            let type_: String = row.get(1)?;
            let type_ = StreamType::parse(&type_)
                .ok_or_else(|| err!(DataLoss, msg("no such stream type {type_}")))?;
            let camera_id = row.get(2)?;
            let c = self
                .cameras_by_id
                .get_mut(&camera_id)
                .ok_or_else(|| err!(DataLoss, msg("missing camera {camera_id} for stream {id}")))?;
            self.streams_by_id.insert(
                id,
                Stream {
                    id,
                    type_,
                    camera_id,
                    sample_file_dir_id: row.get(3)?,
                    config: row.get(4)?,
                    range: None,
                    sample_file_bytes: 0,
                    fs_bytes: 0,
                    to_delete: Vec::new(),
                    bytes_to_delete: 0,
                    fs_bytes_to_delete: 0,
                    bytes_to_add: 0,
                    fs_bytes_to_add: 0,
                    duration: recording::Duration(0),
                    committed_days: days::Map::default(),
                    cum_recordings: row.get(5)?,
                    cum_media_duration: recording::Duration(row.get(6)?),
                    cum_runs: row.get(7)?,
                    uncommitted: VecDeque::new(),
                    synced_recordings: 0,
                    on_live_segment: Vec::new(),
                },
            );
            c.streams[type_.index()] = Some(id);
        }
        info!("Loaded {} streams", self.streams_by_id.len());
        Ok(())
    }

    /// Inserts the specified video sample entry if absent.
    /// On success, returns the id of a new or existing row.
    pub fn insert_video_sample_entry(
        &mut self,
        entry: VideoSampleEntryToInsert,
    ) -> Result<i32, Error> {
        // Check if it already exists.
        // There shouldn't be too many entries, so it's fine to enumerate everything.
        for (&id, v) in &self.video_sample_entries_by_id {
            if v.data == entry.data {
                // The other fields are derived from data, so differences indicate a bug.
                if v.width != entry.width
                    || v.height != entry.height
                    || v.pasp_h_spacing != entry.pasp_h_spacing
                    || v.pasp_v_spacing != entry.pasp_v_spacing
                {
                    bail!(
                        Internal,
                        msg("video_sample_entry id {id}: existing entry {v:?}, new {entry:?}"),
                    );
                }
                return Ok(id);
            }
        }

        let mut stmt = self.conn.prepare_cached(INSERT_VIDEO_SAMPLE_ENTRY_SQL)?;
        stmt.execute(named_params! {
            ":width": i32::from(entry.width),
            ":height": i32::from(entry.height),
            ":pasp_h_spacing": i32::from(entry.pasp_h_spacing),
            ":pasp_v_spacing": i32::from(entry.pasp_v_spacing),
            ":rfc6381_codec": &entry.rfc6381_codec,
            ":data": &entry.data,
        })
        .map_err(|e| err!(e, msg("Unable to insert {entry:#?}")))?;

        let id = self.conn.last_insert_rowid() as i32;
        self.video_sample_entries_by_id.insert(
            id,
            Arc::new(VideoSampleEntry {
                id,
                width: entry.width,
                height: entry.height,
                pasp_h_spacing: entry.pasp_h_spacing,
                pasp_v_spacing: entry.pasp_v_spacing,
                data: entry.data,
                rfc6381_codec: entry.rfc6381_codec,
            }),
        );

        Ok(id)
    }

    pub fn add_sample_file_dir(&mut self, path: PathBuf) -> Result<i32, Error> {
        let mut meta = schema::DirMeta::default();
        let uuid = Uuid::new_v4();
        let uuid_bytes = &uuid.as_bytes()[..];
        let o = self
            .open
            .as_ref()
            .ok_or_else(|| err!(FailedPrecondition, msg("database is read-only")))?;

        // Populate meta.
        {
            meta.db_uuid.extend_from_slice(&self.uuid.as_bytes()[..]);
            meta.dir_uuid.extend_from_slice(uuid_bytes);
            let open = meta.in_progress_open.mut_or_insert_default();
            open.id = o.id;
            open.uuid.extend_from_slice(&o.uuid.as_bytes()[..]);
        }

        let dir = dir::SampleFileDir::create(&path, &meta)?;
        let config = SampleFileDirConfig {
            path: path.clone(),
            ..Default::default()
        };
        self.conn.execute(
            r#"
            insert into sample_file_dir (config, uuid, last_complete_open_id)
                                 values (?,      ?,    ?)
            "#,
            params![&config, uuid_bytes, o.id],
        )?;
        let id = self.conn.last_insert_rowid() as i32;
        use ::std::collections::btree_map::Entry;
        let e = self.sample_file_dirs_by_id.entry(id);
        let d = match e {
            Entry::Vacant(e) => e.insert(SampleFileDir {
                id,
                path,
                uuid,
                dir: Some(dir),
                last_complete_open: Some(*o),
                garbage_needs_unlink: FastHashSet::default(),
                garbage_unlinked: Vec::new(),
            }),
            Entry::Occupied(_) => bail!(Internal, msg("duplicate sample file dir id {id}")),
        };
        meta.last_complete_open = meta.in_progress_open.take().into();
        d.dir.as_ref().unwrap().write_meta(&meta)?;
        Ok(id)
    }

    pub fn delete_sample_file_dir(&mut self, dir_id: i32) -> Result<(), Error> {
        for (&id, s) in self.streams_by_id.iter() {
            if s.sample_file_dir_id == Some(dir_id) {
                bail!(
                    FailedPrecondition,
                    msg("can't delete dir referenced by stream {id}")
                );
            }
        }
        let mut d = match self.sample_file_dirs_by_id.entry(dir_id) {
            ::std::collections::btree_map::Entry::Occupied(e) => e,
            _ => bail!(NotFound, msg("no such dir {dir_id} to remove")),
        };
        if !d.get().garbage_needs_unlink.is_empty() || !d.get().garbage_unlinked.is_empty() {
            bail!(
                FailedPrecondition,
                msg(
                    "must collect garbage before deleting directory {}",
                    d.get().path.display(),
                ),
            );
        }
        let dir = match d.get_mut().dir.take() {
            None => dir::SampleFileDir::open(&d.get().path, &d.get().expected_meta(&self.uuid))?,
            Some(arc) => match Arc::strong_count(&arc) {
                1 => arc, // LockedDatabase is only reference
                c => {
                    // a writer::Syncer also has a reference.
                    d.get_mut().dir = Some(arc); // put it back.
                    bail!(
                        FailedPrecondition,
                        msg("can't delete directory {dir_id} with active syncer (refcnt {c})"),
                    );
                }
            },
        };
        if !dir.is_empty()? {
            bail!(
                FailedPrecondition,
                msg(
                    "can't delete sample file directory {} which still has files",
                    &d.get().path.display(),
                ),
            );
        }
        let mut meta = d.get().expected_meta(&self.uuid);
        meta.in_progress_open = meta.last_complete_open.take().into();
        dir.write_meta(&meta)?;
        if self
            .conn
            .execute("delete from sample_file_dir where id = ?", params![dir_id])?
            != 1
        {
            bail!(Internal, msg("missing database row for dir {dir_id}"));
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
            let mut stmt = tx.prepare_cached(
                r#"
                insert into camera (uuid,  short_name,  config)
                            values (:uuid, :short_name, :config)
                "#,
            )?;
            stmt.execute(named_params! {
                ":uuid": uuid_bytes,
                ":short_name": &camera.short_name,
                ":config": &camera.config,
            })?;
            camera_id = tx.last_insert_rowid() as i32;
            streams =
                StreamStateChanger::new(&tx, camera_id, None, &self.streams_by_id, &mut camera)?;
        }
        tx.commit()?;
        let streams = streams.apply(&mut self.streams_by_id);
        self.cameras_by_id.insert(
            camera_id,
            Camera {
                id: camera_id,
                uuid,
                short_name: camera.short_name,
                config: camera.config,
                streams,
            },
        );
        self.cameras_by_uuid.insert(uuid, camera_id);
        Ok(camera_id)
    }

    /// Returns a `CameraChange` for the given camera which does nothing.
    ///
    /// The caller can modify it to taste then pass it to `update_camera`.
    /// TODO: consider renaming this to `update_camera` and creating a bulk
    /// `apply_camera_changes`.
    pub fn null_camera_change(&mut self, camera_id: i32) -> Result<CameraChange, Error> {
        let Some(camera) = self.cameras_by_id.get(&camera_id) else {
            bail!(Internal, msg("no such camera {camera_id}"));
        };
        let mut change = CameraChange {
            short_name: camera.short_name.clone(),
            config: camera.config.clone(),
            streams: Default::default(),
        };
        for i in 0..NUM_STREAM_TYPES {
            if let Some(stream_id) = camera.streams[i] {
                let s = self
                    .streams_by_id
                    .get(&stream_id)
                    .expect("cameras reference valid streams");
                change.streams[i] = StreamChange {
                    sample_file_dir_id: s.sample_file_dir_id,
                    config: s.config.clone(),
                };
            }
        }
        Ok(change)
    }

    /// Updates a camera.
    pub fn update_camera(&mut self, camera_id: i32, mut camera: CameraChange) -> Result<(), Error> {
        let tx = self.conn.transaction()?;
        let streams;
        let Some(c) = self.cameras_by_id.get_mut(&camera_id) else {
            bail!(Internal, msg("no such camera {camera_id}"));
        };
        {
            streams =
                StreamStateChanger::new(&tx, camera_id, Some(c), &self.streams_by_id, &mut camera)?;
            let mut stmt = tx.prepare_cached(
                r#"
                update camera set
                    short_name = :short_name,
                    config = :config
                where
                    id = :id
                "#,
            )?;
            let rows = stmt.execute(named_params! {
                ":id": camera_id,
                ":short_name": &camera.short_name,
                ":config": &camera.config,
            })?;
            if rows != 1 {
                bail!(Internal, msg("camera {camera_id} missing from database"));
            }
        }
        tx.commit()?;
        c.short_name = camera.short_name;
        c.config = camera.config;
        c.streams = streams.apply(&mut self.streams_by_id);
        Ok(())
    }

    /// Deletes a camera and its streams. The camera must have no recordings.
    pub fn delete_camera(&mut self, id: i32) -> Result<(), Error> {
        // TODO: also verify there are no uncommitted recordings.
        let Some(uuid) = self.cameras_by_id.get(&id).map(|c| c.uuid) else {
            bail!(NotFound, msg("no such camera {id}"));
        };
        let mut streams_to_delete = Vec::new();
        let tx = self.conn.transaction()?;
        {
            let mut stream_stmt = tx.prepare_cached(r"delete from stream where id = :id")?;
            for (stream_id, stream) in &self.streams_by_id {
                if stream.camera_id != id {
                    continue;
                };
                if stream.range.is_some() {
                    bail!(
                        FailedPrecondition,
                        msg("can't remove camera {id}; has recordings")
                    );
                }
                let rows = stream_stmt.execute(named_params! {":id": stream_id})?;
                if rows != 1 {
                    bail!(Internal, msg("stream {id} missing from database"));
                }
                streams_to_delete.push(*stream_id);
            }
            let mut cam_stmt = tx.prepare_cached(r"delete from camera where id = :id")?;
            let rows = cam_stmt.execute(named_params! {":id": id})?;
            if rows != 1 {
                bail!(Internal, msg("camera {id} missing from database"));
            }
        }
        tx.commit()?;
        for id in streams_to_delete {
            self.streams_by_id.remove(&id);
        }
        self.cameras_by_id.remove(&id);
        self.cameras_by_uuid.remove(&uuid);
        Ok(())
    }

    // TODO: it'd make more sense to have a bulk camera/stream edit API than
    // this specific one.
    pub fn update_retention(&mut self, changes: &[RetentionChange]) -> Result<(), Error> {
        // TODO: should validate there's only one change per id.
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                r#"
                update stream
                set
                  config = :config
                where
                  id = :id
                "#,
            )?;
            for c in changes {
                let Some(stream) = self.streams_by_id.get(&c.stream_id) else {
                    bail!(Internal, msg("no such stream {}", c.stream_id));
                };
                let mut new_config = stream.config.clone();
                new_config.mode = (if c.new_record { "record" } else { "" }).into();
                new_config.retain_bytes = c.new_limit;
                let rows = stmt.execute(named_params! {
                    ":config": &new_config,
                    ":id": c.stream_id,
                })?;
                assert_eq!(rows, 1, "missing stream {}", c.stream_id);
            }
        }
        tx.commit()?;
        for c in changes {
            let s = self
                .streams_by_id
                .get_mut(&c.stream_id)
                .expect("stream in db but not state");
            s.config.mode = (if c.new_record { "record" } else { "" }).into();
            s.config.retain_bytes = c.new_limit;
        }
        Ok(())
    }

    // ---- auth ----

    pub fn users_by_id(&self) -> &BTreeMap<i32, User> {
        self.auth.users_by_id()
    }

    pub fn get_user_by_id_mut(&mut self, id: i32) -> Option<&mut User> {
        self.auth.get_user_by_id_mut(id)
    }

    pub fn apply_user_change(&mut self, change: UserChange) -> Result<&User, base::Error> {
        self.auth.apply(&self.conn, change)
    }

    pub fn delete_user(&mut self, id: i32) -> Result<(), base::Error> {
        self.auth.delete_user(&mut self.conn, id)
    }

    pub fn get_user(&self, username: &str) -> Option<&User> {
        self.auth.get_user(username)
    }

    pub fn login_by_password(
        &mut self,
        req: auth::Request,
        username: &str,
        password: String,
        domain: Option<Vec<u8>>,
        session_flags: i32,
    ) -> Result<(RawSessionId, &Session), base::Error> {
        self.auth
            .login_by_password(&self.conn, req, username, password, domain, session_flags)
    }

    pub fn make_session(
        &mut self,
        creation: Request,
        uid: i32,
        domain: Option<Vec<u8>>,
        flags: i32,
        permissions: schema::Permissions,
    ) -> Result<(RawSessionId, &Session), base::Error> {
        self.auth
            .make_session(&self.conn, creation, uid, domain, flags, permissions)
    }

    pub fn authenticate_session(
        &mut self,
        req: auth::Request,
        sid: &auth::SessionHash,
    ) -> Result<(&auth::Session, &User), base::Error> {
        self.auth.authenticate_session(&self.conn, req, sid)
    }

    pub fn revoke_session(
        &mut self,
        reason: auth::RevocationReason,
        detail: Option<String>,
        req: auth::Request,
        hash: &auth::SessionHash,
    ) -> Result<(), base::Error> {
        self.auth
            .revoke_session(&self.conn, reason, detail, req, hash)
    }

    // ---- signal ----

    pub fn signals_by_id(&self) -> &BTreeMap<u32, signal::Signal> {
        self.signal.signals_by_id()
    }
    pub fn signal_types_by_uuid(&self) -> &FastHashMap<Uuid, signal::Type> {
        self.signal.types_by_uuid()
    }
    pub fn list_changes_by_time(
        &self,
        desired_time: Range<recording::Time>,
        f: &mut dyn FnMut(&signal::ListStateChangesRow),
    ) {
        self.signal.list_changes_by_time(desired_time, f)
    }
    pub fn update_signals(
        &mut self,
        when: Range<recording::Time>,
        signals: &[u32],
        states: &[u16],
    ) -> Result<(), base::Error> {
        self.signal.update_signals(when, signals, states)
    }
}

/// Pragmas for full database integrity.
///
/// These are `pub` so that the `moonfire-nvr sql` command can pass to the SQLite3 binary with
/// `-cmd`.
pub static INTEGRITY_PRAGMAS: [&str; 3] = [
    // Enforce foreign keys. This is on by default with --features=bundled (as rusqlite
    // compiles the SQLite3 amalgamation with -DSQLITE_DEFAULT_FOREIGN_KEYS=1). Ensure it's
    // always on. Note that our foreign keys are immediate rather than deferred, so we have to
    // be careful about the order of operations during the upgrade.
    "pragma foreign_keys = on",
    // Make the database actually durable.
    "pragma fullfsync = on",
    "pragma synchronous = 3",
];

/// Sets pragmas for full database integrity.
pub(crate) fn set_integrity_pragmas(conn: &mut rusqlite::Connection) -> Result<(), Error> {
    for pragma in INTEGRITY_PRAGMAS {
        conn.execute(pragma, params![])?;
    }
    Ok(())
}

pub(crate) fn check_sqlite_version() -> Result<(), Error> {
    // SQLite version 3.8.2 introduced the "without rowid" syntax used in the schema.
    // https://www.sqlite.org/withoutrowid.html
    if rusqlite::version_number() < 3008002 {
        bail!(
            FailedPrecondition,
            msg(
                "SQLite version {} is too old; need at least 3.8.2",
                rusqlite::version()
            ),
        );
    }
    Ok(())
}

/// Initializes a database.
/// Note this doesn't set journal options, so that it can be used on in-memory databases for
/// test code.
pub fn init(conn: &mut rusqlite::Connection) -> Result<(), Error> {
    check_sqlite_version()?;
    set_integrity_pragmas(conn)?;
    let tx = conn.transaction()?;
    tx.execute_batch(include_str!("schema.sql"))
        .map_err(|e| err!(e, msg("unable to create database schema")))?;
    {
        let uuid = ::uuid::Uuid::new_v4();
        let uuid_bytes = &uuid.as_bytes()[..];
        tx.execute("insert into meta (uuid) values (?)", params![uuid_bytes])?;
    }
    tx.commit()?;
    Ok(())
}

/// Gets the schema version from the given database connection.
/// A fully initialized database will return `Ok(Some(schema_version))` where `schema_version` is
/// an integer that can be compared to `EXPECTED_SCHEMA_VERSION`. An empty database will return
/// `Ok(None)`. A partially initialized database (in particular, one without a version row) will
/// return some error.
pub fn get_schema_version(conn: &rusqlite::Connection) -> Result<Option<i32>, Error> {
    let ver_tables: i32 = conn.query_row_and_then(
        "select count(*) from sqlite_master where name = 'version'",
        params![],
        |row| row.get(0),
    )?;
    if ver_tables == 0 {
        return Ok(None);
    }
    Ok(Some(conn.query_row_and_then(
        "select max(id) from version",
        params![],
        |row| row.get(0),
    )?))
}

/// Returns the UUID associated with the current system boot, if available.
fn get_boot_uuid() -> Result<Option<Uuid>, Error> {
    if cfg!(target_os = "linux") {
        let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        Ok(Some(Uuid::parse_str(boot_id.trim_end()).map_err(|e| {
            err!(Internal, msg("boot_id is not a valid uuid"), source(e))
        })?))
    } else {
        Ok(None) // don't complain about lack of platform support; just return None.
    }
}

/// Checks that the schema version in the given database is as expected.
pub(crate) fn check_schema_version(conn: &rusqlite::Connection) -> Result<(), Error> {
    let Some(ver) = get_schema_version(conn)? else {
        bail!(
            FailedPrecondition,
            msg("no such table: version.\n\n\
                If you have created an empty database by hand, delete it and use `nvr init` \
                instead, as noted in the installation instructions: \
                <https://github.com/scottlamb/moonfire-nvr/blob/master/guide/install.md>\n\n\
                If you are starting from a database that predates schema versioning, see \
                <https://github.com/scottlamb/moonfire-nvr/blob/master/guide/schema.md>."),
        )
    };
    match ver.cmp(&EXPECTED_SCHEMA_VERSION) {
        std::cmp::Ordering::Less => bail!(
            FailedPrecondition,
            msg(
                "database schema version {ver} is too old (expected {EXPECTED_SCHEMA_VERSION}); \
                see upgrade instructions in guide/upgrade.md"
            ),
        ),
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Greater => bail!(
            FailedPrecondition,
            msg(
                "database schema version {ver} is too new (expected {EXPECTED_SCHEMA_VERSION}); \
                must use a newer binary to match"
            ),
        ),
    }
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
            return; // don't flush while panicking.
        }
        if let Some(m) = self.db.take() {
            if let Err(e) = m.into_inner().unwrap().flush(&self.clocks, "drop") {
                error!(err = %e.chain(), "final database flush failed");
            }
        }
    }
}

// Helpers for Database::lock(). Closures don't implement Fn.
fn acquisition() -> &'static str {
    "database lock acquisition"
}
fn operation() -> &'static str {
    "database operation"
}

impl<C: Clocks + Clone> Database<C> {
    /// Creates the database from a caller-supplied SQLite connection.
    pub fn new(
        clocks: C,
        mut conn: rusqlite::Connection,
        read_write: bool,
    ) -> Result<Database<C>, Error> {
        check_sqlite_version()?;
        set_integrity_pragmas(&mut conn)?;
        check_schema_version(&conn)?;

        // Note: the meta check comes after the version check to improve the error message when
        // trying to open a version 0 or version 1 database (which lacked the meta table).
        let (db_uuid, config) = raw::read_meta(&conn)?;
        let open_monotonic = recording::Time::new(clocks.monotonic());
        let open = if read_write {
            let real = recording::Time::new(clocks.realtime());
            let mut stmt = conn
                .prepare(" insert into open (uuid, start_time_90k, boot_uuid) values (?, ?, ?)")?;
            let open_uuid = SqlUuid(Uuid::new_v4());
            let boot_uuid = match get_boot_uuid() {
                Err(e) => {
                    warn!(err = %e.chain(), "unable to get boot uuid");
                    None
                }
                Ok(id) => id.map(SqlUuid),
            };
            stmt.execute(params![open_uuid, real.0, boot_uuid])?;
            let id = conn.last_insert_rowid() as u32;
            Some(Open {
                id,
                uuid: open_uuid.0,
            })
        } else {
            None
        };
        let auth = auth::State::init(&conn)?;
        let signal = signal::State::init(&conn, &config)?;
        let db = Database {
            db: Some(Mutex::new(LockedDatabase {
                conn,
                uuid: db_uuid,
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
                video_index_cache: RefCell::new(LinkedHashMap::with_capacity_and_hasher(
                    VIDEO_INDEX_CACHE_LEN + 1,
                    Default::default(),
                )),
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
    pub fn clocks(&self) -> C {
        self.clocks.clone()
    }

    /// Locks the database; the returned reference is the only way to perform (read or write)
    /// operations.
    pub fn lock(&self) -> DatabaseGuard<C> {
        let timer = clock::TimerGuard::new(&self.clocks, acquisition);
        let db = self.db.as_ref().unwrap().lock().unwrap();
        drop(timer);
        let _timer = clock::TimerGuard::<C, &'static str, fn() -> &'static str>::new(
            &self.clocks,
            operation,
        );
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
        self.db.take().unwrap().into_inner().unwrap().conn
    }
}

/// Reference to a locked database returned by [Database::lock].
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
    fn deref(&self) -> &LockedDatabase {
        &self.db
    }
}

impl<'db, C: Clocks + Clone> ::std::ops::DerefMut for DatabaseGuard<'db, C> {
    fn deref_mut(&mut self) -> &mut LockedDatabase {
        &mut self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::{self, TIME_UNITS_PER_SEC};
    use crate::testutil;
    use base::clock;
    use rusqlite::Connection;
    use url::Url;
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
                assert_eq!(
                    "http://test-camera/",
                    row.config.onvif_base_url.as_ref().unwrap().as_str()
                );
                assert_eq!("foo", &row.config.username);
                assert_eq!("bar", &row.config.password);
                //assert_eq!("/main", row.main_rtsp_url);
                //assert_eq!("/sub", row.sub_rtsp_url);
                //assert_eq!(42, row.retain_bytes);
                //assert_eq!(None, row.range);
                //assert_eq!(recording::Duration(0), row.duration);
                //assert_eq!(0, row.sample_file_bytes);
            }
        }
        assert_eq!(1, rows);

        let stream_id = camera_id; // TODO
        rows = 0;
        {
            let db = db.lock();
            let all_time = recording::Time(i64::min_value())..recording::Time(i64::max_value());
            db.list_recordings_by_time(stream_id, all_time, &mut |_row| {
                rows += 1;
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(0, rows);
    }

    fn assert_single_recording(db: &Database, stream_id: i32, r: &RecordingToInsert) {
        {
            let db = db.lock();
            let stream = db.streams_by_id().get(&stream_id).unwrap();
            let dur = recording::Duration(r.wall_duration_90k as i64);
            assert_eq!(Some(r.start..r.start + dur), stream.range);
            assert_eq!(r.sample_file_bytes as i64, stream.sample_file_bytes);
            assert_eq!(dur, stream.duration);
            db.cameras_by_id().get(&stream.camera_id).unwrap();
        }

        // TODO(slamb): test that the days logic works correctly.

        let mut rows = 0;
        let mut recording_id = None;
        {
            let db = db.lock();
            let all_time = recording::Time(i64::min_value())..recording::Time(i64::max_value());
            db.list_recordings_by_time(stream_id, all_time, &mut |row| {
                rows += 1;
                recording_id = Some(row.id);
                assert_eq!(r.start, row.start);
                assert_eq!(r.wall_duration_90k, row.wall_duration_90k);
                assert_eq!(r.video_samples, row.video_samples);
                assert_eq!(r.video_sync_samples, row.video_sync_samples);
                assert_eq!(r.sample_file_bytes, row.sample_file_bytes);
                let vse = db
                    .video_sample_entries_by_id()
                    .get(&row.video_sample_entry_id)
                    .unwrap();
                assert_eq!(vse.rfc6381_codec, "avc1.4d0029");
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(1, rows);

        rows = 0;
        raw::list_oldest_recordings(
            &db.lock().conn,
            CompositeId::new(stream_id, 0),
            &mut |row| {
                rows += 1;
                assert_eq!(recording_id, Some(row.id));
                assert_eq!(r.start, row.start);
                assert_eq!(r.wall_duration_90k, row.wall_duration_90k);
                assert_eq!(r.sample_file_bytes, row.sample_file_bytes);
                true
            },
        )
        .unwrap();
        assert_eq!(1, rows);

        // TODO: list_aggregated_recordings.
        // TODO: with_recording_playback.
    }

    #[test]
    fn test_no_meta_or_version() {
        testutil::init();
        let e = Database::new(
            clock::RealClocks {},
            Connection::open_in_memory().unwrap(),
            false,
        )
        .err()
        .unwrap();
        assert!(e.msg().unwrap().starts_with("no such table"), "{}", e);
    }

    #[test]
    fn test_version_too_old() {
        testutil::init();
        let c = setup_conn();
        c.execute_batch("delete from version; insert into version values (6, 0, '');")
            .unwrap();
        let e = Database::new(clock::RealClocks {}, c, false).err().unwrap();
        assert!(
            e.msg()
                .unwrap()
                .starts_with("database schema version 6 is too old (expected 7)"),
            "got: {e:?}"
        );
    }

    #[test]
    fn test_version_too_new() {
        testutil::init();
        let c = setup_conn();
        c.execute_batch("delete from version; insert into version values (8, 0, '');")
            .unwrap();
        let e = Database::new(clock::RealClocks {}, c, false).err().unwrap();
        assert!(
            e.msg()
                .unwrap()
                .starts_with("database schema version 8 is too new (expected 7)"),
            "got: {e:?}"
        );
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
        let tmpdir = tempfile::Builder::new()
            .prefix("moonfire-nvr-test")
            .tempdir()
            .unwrap();
        let path = tmpdir.path().to_owned();
        let sample_file_dir_id = { db.lock() }.add_sample_file_dir(path).unwrap();
        let mut c = CameraChange {
            short_name: "testcam".to_owned(),
            config: crate::json::CameraConfig {
                description: "".to_owned(),
                onvif_base_url: Some(Url::parse("http://test-camera/").unwrap()),
                username: "foo".to_owned(),
                password: "bar".to_owned(),
                ..Default::default()
            },
            streams: [
                StreamChange {
                    sample_file_dir_id: Some(sample_file_dir_id),
                    config: crate::json::StreamConfig {
                        url: Some(Url::parse("rtsp://test-camera/main").unwrap()),
                        mode: crate::json::STREAM_MODE_RECORD.to_owned(),
                        flush_if_sec: 1,
                        ..Default::default()
                    },
                },
                StreamChange {
                    sample_file_dir_id: Some(sample_file_dir_id),
                    config: crate::json::StreamConfig {
                        url: Some(Url::parse("rtsp://test-camera/sub").unwrap()),
                        mode: crate::json::STREAM_MODE_RECORD.to_owned(),
                        flush_if_sec: 1,
                        ..Default::default()
                    },
                },
                StreamChange::default(),
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
            }])
            .unwrap();
            {
                let main = l.streams_by_id().get(&main_stream_id).unwrap();
                assert_eq!(main.config.mode, crate::json::STREAM_MODE_RECORD);
                assert_eq!(main.config.retain_bytes, 42);
                assert_eq!(main.config.flush_if_sec, 1);
            }

            assert_eq!(
                l.streams_by_id()
                    .get(&sub_stream_id)
                    .unwrap()
                    .config
                    .flush_if_sec,
                1
            );
            c.streams[1].config.flush_if_sec = 2;
            l.update_camera(camera_id, c).unwrap();
            assert_eq!(
                l.streams_by_id()
                    .get(&sub_stream_id)
                    .unwrap()
                    .config
                    .flush_if_sec,
                2
            );
        }
        let camera_uuid = { db.lock().cameras_by_id().get(&camera_id).unwrap().uuid };
        assert_no_recordings(&db, camera_uuid);
        assert_eq!(
            db.lock()
                .streams_by_id()
                .get(&main_stream_id)
                .unwrap()
                .cum_recordings,
            0
        );

        // Closing and reopening the database should present the same contents.
        let conn = db.close();
        let db = Database::new(clock::RealClocks {}, conn, true).unwrap();
        assert_eq!(
            db.lock()
                .streams_by_id()
                .get(&sub_stream_id)
                .unwrap()
                .config
                .flush_if_sec,
            2
        );
        assert_no_recordings(&db, camera_uuid);
        assert_eq!(
            db.lock()
                .streams_by_id()
                .get(&main_stream_id)
                .unwrap()
                .cum_recordings,
            0
        );

        // TODO: assert_eq!(db.lock().list_garbage(sample_file_dir_id).unwrap(), &[]);

        let vse_id = db
            .lock()
            .insert_video_sample_entry(VideoSampleEntryToInsert {
                width: 1920,
                height: 1080,
                pasp_h_spacing: 1,
                pasp_v_spacing: 1,
                data: include_bytes!("testdata/avc1").to_vec(),
                rfc6381_codec: "avc1.4d0029".to_owned(),
            })
            .unwrap();
        assert!(vse_id > 0, "vse_id = {vse_id}");

        // Inserting a recording should succeed and advance the next recording id.
        let start = recording::Time(1430006400 * TIME_UNITS_PER_SEC);
        let recording = RecordingToInsert {
            sample_file_bytes: 42,
            run_offset: 0,
            flags: 0,
            start,
            prev_media_duration: recording::Duration(0),
            prev_runs: 0,
            wall_duration_90k: TIME_UNITS_PER_SEC.try_into().unwrap(),
            media_duration_90k: TIME_UNITS_PER_SEC.try_into().unwrap(),
            local_time_delta: recording::Duration(0),
            video_samples: 1,
            video_sync_samples: 1,
            video_sample_entry_id: vse_id,
            video_index: [0u8; 100].to_vec(),
            sample_file_blake3: None,
            end_reason: None,
        };
        let id = {
            let mut db = db.lock();
            let (id, _) = db.add_recording(main_stream_id, recording.clone()).unwrap();
            db.mark_synced(id).unwrap();
            db.flush("add test").unwrap();
            id
        };
        assert_eq!(
            db.lock()
                .streams_by_id()
                .get(&main_stream_id)
                .unwrap()
                .cum_recordings,
            1
        );

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
            db.delete_oldest_recordings(main_stream_id, &mut |_| {
                n += 1;
                true
            })
            .unwrap();
            assert_eq!(n, 1);
            {
                let s = db.streams_by_id().get(&main_stream_id).unwrap();
                assert_eq!(s.sample_file_bytes, 42);
                assert_eq!(s.bytes_to_delete, 42);
            }
            n = 0;

            // A second run
            db.delete_oldest_recordings(main_stream_id, &mut |_| {
                n += 1;
                true
            })
            .unwrap();
            assert_eq!(n, 0);
            assert_eq!(
                db.streams_by_id()
                    .get(&main_stream_id)
                    .unwrap()
                    .bytes_to_delete,
                42
            );
            db.flush("delete test").unwrap();
            let s = db.streams_by_id().get(&main_stream_id).unwrap();
            assert_eq!(s.sample_file_bytes, 0);
            assert_eq!(s.bytes_to_delete, 0);
        }
        assert_no_recordings(&db, camera_uuid);
        let g: Vec<_> = db
            .lock()
            .sample_file_dirs_by_id()
            .get(&sample_file_dir_id)
            .unwrap()
            .garbage_needs_unlink
            .iter()
            .copied()
            .collect();
        assert_eq!(&g, &[id]);
        let g: Vec<_> = db
            .lock()
            .sample_file_dirs_by_id()
            .get(&sample_file_dir_id)
            .unwrap()
            .garbage_unlinked
            .iter()
            .copied()
            .collect();
        assert_eq!(&g, &[]);
    }

    #[test]
    fn round_up() {
        assert_eq!(super::round_up(0), 0);
        assert_eq!(super::round_up(8_191), 8_192);
        assert_eq!(super::round_up(8_192), 8_192);
        assert_eq!(super::round_up(8_193), 12_288);
    }
}
