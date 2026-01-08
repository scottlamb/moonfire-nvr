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
use crate::dir;
use crate::json::SampleFileDirConfig;
use crate::raw;
use crate::recording;
use crate::sample_entries;
use crate::schema;
use crate::signal;
use crate::stream;
use crate::stream::recent_frames::RecentFrames;
use crate::stream::LockedStream;
use crate::stream::Stream;
use crate::stream::StreamCommitted;
use crate::stream::StreamComplete;
use crate::stream::StreamType;
use crate::stream::NUM_STREAM_TYPES;
use base::clock::{self, Clocks};
use base::strutil::encode_size;
use base::FastHashSet;
use base::{bail, err, Error, FastHashMap, Mutex, MutexGuard};
use bitflags::bitflags;
use hashlink::LinkedHashMap;
use itertools::Itertools;
use rusqlite::{named_params, params};
use smallvec::SmallVec;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::mem;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::panic::Location;
use std::path::PathBuf;
use std::str;
use std::string::String;
use std::sync::Arc;
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

const UPDATE_STREAM_COUNTERS_SQL: &str = r#"
    update stream
    set cum_recordings = :cum_recordings,
        cum_media_duration_90k = :cum_media_duration_90k,
        cum_runs = :cum_runs
    where id = :stream_id
"#;

const DIR_POOL_WORKERS: NonZeroUsize = const { NonZeroUsize::new(2).unwrap() };

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

/// A row used in `list_recordings_by_time` and `list_recordings_by_id`.
#[derive(Clone, Debug)]
pub struct ListRecordingsRow<'a> {
    pub start: recording::Time,
    pub video_sample_entry_id: i32,

    pub id: CompositeId,

    /// This is a recording::Duration, but a single recording's duration fits into an i32.
    pub wall_duration_90k: i32,
    pub media_duration_90k: i32,
    pub video_samples: i32,
    pub video_sync_samples: i32,
    pub sample_file_bytes: u32,
    pub run_offset: i32,
    pub open_id: u32,
    pub flags: RecordingFlags,

    /// This is populated by `list_recordings_by_id` but not `list_recordings_by_time`.
    /// (It's not included in the `recording_cover` index, so adding it to
    /// `list_recordings_by_time` would be inefficient.)
    pub prev_media_duration_and_runs: Option<(recording::Duration, i32)>,
    pub end_reason: Option<String>,

    /// If this row was constructed from a recent recording, the `RecentRecording`
    /// within the locked stream.
    pub(crate) recent_recording: Option<&'a RecentRecording>,
}

impl ListRecordingsRow<'_> {
    /// Calls `f` with playback data for this recording.
    ///
    /// Unlike `LockedDatabase::with_playback`, this may be called (potentially
    /// indirectly) from a `list_recordings_by_id` callback on the row it was
    /// passed:
    ///
    /// * if `self.recent_recordings.is_some()`, the stream lock is held by
    ///   `list_recordings_by_id`, and no database I/O will be performed.
    /// * otherwise, the stream lock must not be held, and database I/O may be performed.
    ///
    /// `f` must not block.
    pub fn with_playback<T>(
        &self,
        db: &LockedDatabase,
        f: &mut dyn FnMut(&RecordingPlayback) -> Result<T, Error>,
    ) -> Result<T, Error> {
        if let Some(r) = self.recent_recording.as_ref() {
            return f(&RecordingPlayback {
                video_index: &r.video_index[..],
            });
        }
        db.with_recording_playback_db_path(self.id, f)
    }
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
            first_uncommitted: if row.flags.contains(RecordingFlags::UNCOMMITTED) {
                Some(recording_id)
            } else {
                None
            },
            growing: row.flags.contains(RecordingFlags::GROWING),
            has_trailing_zero: row.flags.contains(RecordingFlags::TRAILING_ZERO),
            end_reason: row.end_reason,
        }
    }
}

/// Select fields from the `recordings_playback` table. Retrieve with `with_recording_playback`.
#[derive(Debug)]
pub struct RecordingPlayback<'a> {
    pub video_index: &'a [u8],
}

bitflags! {
    /// Bitmask in the `flags` field in the `recordings` table; see `schema.sql`.
    #[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
    pub struct RecordingFlags: u32 {
        const TRAILING_ZERO = 1;

        // The flags below must never be persisted to the database.

        /// The recording is still growing: frames are being appended. Only
        /// the most recent recording can be growing.
        const GROWING = 1<<29;

        /// The recording is considered deleted.
        /// * If `UNCOMMITTED`, it has been aborted by the writer.
        /// * If committed, it has since been deleted and flushed.
        /// The sample file may still exist on disk as garbage.
        const DELETED = 1<<30;

        /// The recording has not been committed to the database.
        /// This must be set iff this recording's id is >= `stream.committed.cum_recordings`.
        const UNCOMMITTED = 1<<31;

        // Preserve any bits set in the database.
        const _ = !0;
    }
}

/// A recording which is being maintained in-RAM within [`LockedStream::recent_recordings`].
///
/// Must be non-empty and with valid time.
#[derive(Clone, derive_more::Debug, Default)]
pub struct RecentRecording {
    pub id: i32,
    pub run_offset: i32,

    pub(crate) flags: RecordingFlags,
    pub sample_file_bytes: u32,

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
    #[debug(skip)]
    pub video_index: Vec<u8>,
    #[debug(skip)]
    pub sample_file_blake3: Option<[u8; 32]>,
    pub end_reason: Option<String>,
}

impl RecentRecording {
    fn to_list_row(&self, id: CompositeId, open_id: u32) -> ListRecordingsRow<'_> {
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
            flags: self.flags,
            prev_media_duration_and_runs: Some((self.prev_media_duration, self.prev_runs)),
            end_reason: self.end_reason.clone(),
            recent_recording: Some(self),
        }
    }
}

/// A row used in `raw::list_oldest_recordings` and `db::delete_oldest_recordings`.
#[derive(Copy, Clone, Debug)]
pub(crate) struct ListOldestRecordingsRow {
    pub id: CompositeId,
    pub start: recording::Time,
    pub wall_duration_90k: i32,
    pub sample_file_bytes: u32,
}

#[derive(Clone)]
pub struct SampleFileDir {
    pub id: i32,
    pub(crate) pool: dir::Pool,
}

impl SampleFileDir {
    /// Returns a worker pool handle; the pool is not guaranteed to be open.
    pub fn pool(&self) -> &dir::Pool {
        &self.pool
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
    pub streams: [Option<i32>; stream::NUM_STREAM_TYPES],
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

/// Initializes the recordings associated with the given camera.
fn init_recordings(
    conn: &rusqlite::Connection,
    stream_id: i32,
    camera: &Camera,
    stream: &mut stream::LockedStream,
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
        stream
            .committed
            .add_recording(start..start + duration, bytes);
        i += 1;
    }
    info!(
        "Loaded {} recordings for camera {} stream {:?}",
        i, camera.short_name, stream.type_
    );
    Ok(())
}

pub struct LockedDatabase {
    /// The connection, which should never be used while holding a [`Stream`] or
    /// [`dir::Pool`] lock to avoid stalling other threads on potentially long
    /// database operations. `Antilock` enforces this in debug mode.
    conn: base::Antilock<1, rusqlite::Connection>,
    uuid: Uuid,
    flush_count: u64,
    pub(crate) flusher_notify: Arc<tokio::sync::Notify>,

    /// If the database is open in read-write mode, the information about the current Open row.
    pub open: Option<Open>,

    /// The monotonic time when the database was opened (whether in read-write mode or read-only
    /// mode).
    open_monotonic: base::clock::Instant,

    auth: auth::State,
    signal: signal::State,

    sample_file_dirs_by_id: BTreeMap<i32, SampleFileDir>,
    cameras_by_id: BTreeMap<i32, Camera>,
    streams_by_id: BTreeMap<i32, Arc<Stream>>,
    cameras_by_uuid: BTreeMap<Uuid, i32>, // values are ids.
    sample_entries: sample_entries::Handle,
    video_index_cache: RefCell<LinkedHashMap<i64, Box<[u8]>, base::RandomState>>,
    on_flush: tokio::sync::watch::Sender<u64>,
}

/// Represents a row of the `open` database table, representing a time the
/// database has been opened in read/write mode.
#[derive(Copy, Clone, Debug)]
pub struct Open {
    pub id: u32,
    pub(crate) uuid: Uuid,
}

impl Open {
    pub(crate) fn matches(&self, o: &schema::dir_meta::Open) -> bool {
        o.uuid == self.uuid.as_bytes() && o.id == self.id
    }
}

impl From<Open> for schema::dir_meta::Open {
    fn from(o: Open) -> Self {
        schema::dir_meta::Open {
            id: o.id,
            uuid: o.uuid.as_bytes().to_vec(),
            ..Default::default()
        }
    }
}

/// A combination of a stream id and recording id into a single 64-bit int.
/// This is used as a primary key in the SQLite `recording` table (see `schema.sql`)
/// and the sample file's name on disk (see `dir.rs`).
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct CompositeId(pub i64);

impl CompositeId {
    pub fn new(stream_id: i32, recording_id: i32) -> Self {
        CompositeId(((stream_id as i64) << 32) | recording_id as i64)
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
    sids: [Option<i32>; stream::NUM_STREAM_TYPES],

    /// For each stream to change, a (stream_id, upsert or `None` to delete) tuple.
    streams: Vec<(i32, Option<StreamStateChangerUpsert>)>,
}

/// Upsert state used internally within [`StreamStateChanger`].
struct StreamStateChangerUpsert {
    camera_id: i32,
    type_: stream::StreamType,
    sc: StreamChange,
}

impl StreamStateChanger {
    /// Performs the database updates (guarded by the given transaction) and returns the state
    /// change to be applied on successful commit.
    fn new(
        tx: &rusqlite::Transaction,
        camera_id: i32,
        existing: Option<&Camera>,
        streams_by_id: &BTreeMap<i32, Arc<Stream>>,
        change: &mut CameraChange,
    ) -> Result<Self, Error> {
        let mut sids = [None; stream::NUM_STREAM_TYPES];
        let mut streams = Vec::with_capacity(stream::NUM_STREAM_TYPES);
        let existing_streams = existing.map(|e| e.streams).unwrap_or_default();
        for (i, ref mut sc) in change.streams.iter_mut().enumerate() {
            let type_ = StreamType::from_index(i).unwrap();
            let mut have_data = false;
            if let Some(sid) = existing_streams[i] {
                let s = streams_by_id.get(&sid).unwrap();
                let l = s.inner.lock();
                if l.committed.range.is_some() {
                    have_data = true;
                    if let Some(d) = l.sample_file_dir.as_ref() {
                        if Some(d.id) != sc.sample_file_dir_id {
                            bail!(
                                FailedPrecondition,
                                msg(
                                    "can't change sample_file_dir_id {:?}->{:?} for non-empty stream {}",
                                    d.id,
                                    sc.sample_file_dir_id,
                                    sid,
                                ),
                            );
                        }
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
        sample_file_dirs_by_id: &BTreeMap<i32, SampleFileDir>,
        streams_by_id: &mut BTreeMap<i32, Arc<Stream>>,
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
                    e.insert(Stream::new(LockedStream {
                        open_writer: false,
                        id,
                        type_,
                        camera_id,
                        sample_file_dir: sc.sample_file_dir_id.map(|id| {
                            sample_file_dirs_by_id
                                .get(&id)
                                .expect("sample_file_dir_id should exist")
                                .clone()
                        }),
                        config: sc.config,
                        committed: StreamCommitted::default(),
                        to_delete: Vec::new(),
                        bytes_to_delete: 0,
                        fs_bytes_to_delete: 0,
                        complete: StreamComplete::default(),
                        flush_ready: 0,
                        recent_recordings: VecDeque::new(),
                        recent_recordings_pinned: false,
                        recent_frames: RecentFrames::default(),
                        writer_state: crate::db::dir::writer::State::default(),
                    }));
                }
                (Entry::Vacant(_), None) => {}
                (Entry::Occupied(e), Some(StreamStateChangerUpsert { sc, .. })) => {
                    let e = e.into_mut();
                    let mut l = e.inner.lock();
                    l.sample_file_dir = sc.sample_file_dir_id.map(|id| {
                        sample_file_dirs_by_id
                            .get(&id)
                            .expect("sample_file_dir_id should exist")
                            .clone()
                    });
                    l.config = sc.config;
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
    pub fn flushes(&self) -> u64 {
        self.flush_count
    }

    pub fn sample_entries(&self) -> &sample_entries::Handle {
        &self.sample_entries
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

        #[derive(Copy, Clone)]
        struct NewTotals {
            cum_recordings: i32,
            cum_runs: i32,
            cum_media_duration: recording::Duration,
        }

        struct Deletion {
            n: usize,
            dir_id: i32,
            end_id: i32,
        }

        #[derive(Default)]
        struct ChangingStream {
            /// The new time range for this stream, to be filled later in the transaction.
            new_range: Option<Range<recording::Time>>,
            deletion: Option<Deletion>,
            new_counters: Option<NewTotals>,
        }
        let mut changing_streams =
            FastHashMap::with_capacity_and_hasher(self.streams_by_id.len(), Default::default());
        let mut new_recordings = Vec::new();
        {
            for (&stream_id, s) in &self.streams_by_id {
                let s = s.inner.lock();

                // Process additions.
                let mut new_totals: Option<NewTotals> = None;
                let i = s
                    .recent_recordings
                    .partition_point(|r| r.id < s.committed.cum_recordings);
                for r in s
                    .recent_recordings
                    .iter()
                    .skip(i)
                    .take_while(|r| r.id < s.flush_ready)
                {
                    debug_assert!(r.flags.contains(RecordingFlags::UNCOMMITTED));
                    if r.flags.contains(RecordingFlags::DELETED) {
                        continue;
                    }
                    new_recordings.push((stream_id, r.clone()));
                    #[cfg(debug_assertions)]
                    if let Some(last) = new_totals {
                        assert!(last.cum_recordings <= r.id);
                        assert!(last.cum_runs <= r.prev_runs);
                        assert!(last.cum_media_duration <= r.prev_media_duration);
                    }
                    new_totals = Some(NewTotals {
                        cum_recordings: r.id + 1,
                        cum_runs: r.prev_runs + i32::from(r.run_offset == 0),
                        cum_media_duration: r.prev_media_duration
                            + recording::Duration(i64::from(r.wall_duration_90k)),
                    });
                }
                if let Some(new_totals) = new_totals {
                    changing_streams.insert(
                        stream_id,
                        ChangingStream {
                            new_range: None,
                            deletion: None,
                            new_counters: Some(new_totals),
                        },
                    );
                }

                // Process deletions.
                if let Some(l) = s.to_delete.last() {
                    let ent = changing_streams
                        .entry(stream_id)
                        .or_insert_with(Default::default);
                    let dir_id = match &s.sample_file_dir {
                        None => bail!(Internal, msg("stream {stream_id} has no directory!")),
                        Some(d) => d.id,
                    };
                    ent.deletion = Some(Deletion {
                        n: s.to_delete.len(),
                        dir_id,
                        end_id: l.id.recording() + 1,
                    });
                }
            }
        }

        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;

        let sample_entries_to_flush = self.sample_entries.lock().get_entries_to_flush();
        sample_entries_to_flush.perform_inserts(&tx)?;
        raw::insert_recordings(&tx, o, &new_recordings)?;
        let mut update_stream_counters_stmt = tx.prepare_cached(UPDATE_STREAM_COUNTERS_SQL)?;
        for (&stream_id, ent) in &changing_streams {
            if let Some(ref c) = ent.new_counters {
                update_stream_counters_stmt.execute(named_params! {
                    ":stream_id": stream_id,
                    ":cum_recordings": c.cum_recordings,
                    ":cum_media_duration_90k": c.cum_media_duration.0,
                    ":cum_runs": c.cum_runs,
                })?;
            }
            if let Some(ref deletion) = ent.deletion {
                // raw::delete_recordings does a bulk transfer of a range from recording to
                // garbage, rather than operating on each element of to_delete. This is
                // guaranteed to give the same result because to_delete is guaranteed to be the
                // oldest recordings for the stream.
                let start = CompositeId::new(stream_id, 0);
                let end = CompositeId::new(stream_id, deletion.end_id);
                let n = raw::delete_recordings(&tx, deletion.dir_id, start..end)?;
                if n != deletion.n {
                    bail!(
                        Internal,
                        msg(
                            "Found {n} rows in {start} .. {end}, expected {expected_n}",
                            expected_n = deletion.n,
                        ),
                    );
                }
            }
        }

        /// Tracks changes to a sample file directory. Used for logging and for garbage collection tracking.
        #[derive(Default)]
        struct DirChange {
            added: SmallVec<[CompositeId; 32]>,
            deleted: SmallVec<[CompositeId; 32]>,
            gced: SmallVec<[CompositeId; 32]>,
            added_bytes: i64,
            deleted_bytes: i64,
        }
        let mut dir_changes: FastHashMap<i32, DirChange> = FastHashMap::default();

        // Process delete_garbage.
        for (&id, dir) in &mut self.sample_file_dirs_by_id {
            let l = dir.pool.lock();
            let garbage_unlinked = l.garbage_unlinked();
            if !garbage_unlinked.is_empty() {
                dir_changes.insert(
                    id,
                    DirChange {
                        gced: garbage_unlinked.iter().copied().collect(),
                        ..Default::default()
                    },
                );
            }
        }
        for (&stream_id, changing) in &mut changing_streams {
            changing.new_range = raw::get_range(&tx, stream_id)?;
        }
        {
            let mut stmt = tx.prepare_cached(
                r"update open set duration_90k = ?, end_time_90k = ? where id = ?",
            )?;
            let rows = stmt.execute(params![
                recording::Duration::try_from(clocks.monotonic() - self.open_monotonic)
                    .expect("valid duration")
                    .0,
                recording::Time::from(clocks.realtime()).0,
                o.id,
            ])?;
            if rows != 1 {
                bail!(Internal, msg("unable to find current open {}", o.id));
            }
        }
        for dir in dir_changes.values() {
            raw::mark_sample_files_deleted(&tx, &dir.gced)?;
        }

        self.auth.flush(&tx)?;
        self.signal.flush(&tx)?;
        drop(update_stream_counters_stmt);
        tx.commit()?;
        drop(conn);

        for (stream_id, changing) in changing_streams.drain() {
            let s = self.streams_by_id.get_mut(&stream_id).unwrap();
            let mut l = s.inner.lock();
            let s = &mut *l;
            let dir = s.sample_file_dir.as_ref().unwrap().clone();
            let log = dir_changes.entry(dir.id).or_default();

            // Process delete_oldest_recordings.
            s.committed.sample_file_bytes -= s.bytes_to_delete;
            s.committed.fs_bytes -= s.fs_bytes_to_delete;
            log.deleted_bytes += s.bytes_to_delete;
            s.bytes_to_delete = 0;
            s.fs_bytes_to_delete = 0;
            if let Some(last) = s.to_delete.last() {
                s.delete_until(last.id.recording() + 1);
            }
            log.deleted.reserve(s.to_delete.len());
            {
                let mut pool = dir.pool.lock();
                for row in s.to_delete.drain(..) {
                    log.deleted.push(row.id);
                    pool.insert_garbage_needs_unlink(row.id);
                    let d = recording::Duration(i64::from(row.wall_duration_90k));
                    s.committed.duration -= d;
                    s.committed.days.adjust(row.start..row.start + d, -1);
                }
            }

            // Process add_recordings.
            if let Some(new_totals) = changing.new_counters {
                let i = s
                    .recent_recordings
                    .partition_point(|r| r.id < s.committed.cum_recordings);
                for r in s.recent_recordings.iter_mut().skip(i) {
                    if r.id >= new_totals.cum_recordings {
                        break;
                    }
                    if r.flags.contains(RecordingFlags::DELETED) {
                        continue;
                    }
                    assert!(r.flags.contains(RecordingFlags::UNCOMMITTED));
                    r.flags.remove(RecordingFlags::UNCOMMITTED);
                    log.added.push(CompositeId::new(stream_id, r.id));
                    let wall_dur = recording::Duration(r.wall_duration_90k.into());
                    let end = r.start + wall_dur;
                    s.committed.add_recording(r.start..end, r.sample_file_bytes);
                    log.added_bytes += i64::from(r.sample_file_bytes);
                }
                s.committed.cum_recordings = new_totals.cum_recordings;
            }

            s.maybe_prune_recent_recordings();

            // Fix the range.
            s.committed.range = changing.new_range;
        }
        self.auth.post_flush();
        self.signal.post_flush();
        sample_entries_to_flush.post_flush(&mut self.sample_entries.lock());
        self.flush_count += 1;
        let mut log_msg = String::with_capacity(256);
        for (&dir_id, log) in &dir_changes {
            let dir = self.sample_file_dirs_by_id.get(&dir_id).unwrap();
            if !log.gced.is_empty() {
                dir.pool
                    .lock()
                    .remove_garbage_unlinked_prefix(&log.gced[..]);
            }
            write!(
                &mut log_msg,
                "\n{}: added {}B in {} recordings ({}), deleted {}B in {} ({}), \
                   GCed {} recordings ({}).",
                dir.pool.path().display(),
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
        let _ = self.on_flush.send(self.flush_count);
        Ok(())
    }

    /// Sets a watcher which will receive the current flush count whenever a flush completes.
    pub(crate) fn on_flush(&self) -> tokio::sync::watch::Receiver<u64> {
        self.on_flush.subscribe()
    }

    pub fn streams_by_id(&self) -> &BTreeMap<i32, Arc<Stream>> {
        &self.streams_by_id
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
    /// This guarantees that within a run, recordings are returned in ascending order by id.
    /// It does *not* guarantee that recordings are returned in ascending order by start time,
    /// or that a run is completed before the next one begins. That is, given the following
    /// recordings:
    ///
    /// | run start | id | time |
    /// | 1         | 1  | 1000 |
    /// | 1         | 2  | 2000 |
    /// | 1         | 3  | 3000 |
    /// | 4         | 4  | 1500 |
    ///
    /// It guarantees 1 is before 2 which is before 3, but 4 may be interleaved
    /// between any others.
    ///
    /// Empty recordings are omitted.
    ///
    /// # Caveats
    ///
    /// * `f` must not block because it is called while holding the database lock.
    /// * `f` may also be called with the stream lock. Do not call `with_recording_playback` within
    ///   `f`; use `ListRecordingsRow::with_playback` instead.
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

        // When a recording exists both in the database and in recent_recordings, prefer
        // the recent_recordings version for efficiency.
        let db_end_recording_id = {
            let mut s = s.inner.lock();
            assert!(!s.recent_recordings_pinned);
            s.recent_recordings_pinned = true;
            s.recent_recordings
                .front()
                .map(|r| r.id)
                .unwrap_or(s.committed.cum_recordings)
        };
        let db_res = raw::list_recordings_by_time(
            &self.conn.borrow(),
            stream_id,
            desired_time.clone(),
            db_end_recording_id,
            f,
        );
        let mut s = s.inner.lock();
        assert!(s.recent_recordings_pinned);
        s.recent_recordings_pinned = false;
        db_res?;
        for r in s.recent_recordings.iter() {
            if r.flags.contains(RecordingFlags::DELETED) {
                continue;
            }
            let end = r.start + recording::Duration(r.wall_duration_90k as i64);
            if r.start > desired_time.end || end < desired_time.start {
                continue; // there's no overlap with the requested range.
            }
            let row = r.to_list_row(CompositeId::new(stream_id, r.id), self.open.unwrap().id);
            f(row)?;
        }
        Ok(())
    }

    /// Lists the specified recordings in ascending order by id.
    ///
    /// This considers both on-disk and recent recordings, preferring recent
    /// recordings.
    ///
    /// # Caveats
    ///
    /// * `f` must not block because it is called while holding the database lock.
    /// * `f` may also be called with the stream lock. Do not call `with_recording_playback` within
    ///   `f`; use `ListRecordingsRow::with_playback` instead.
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
        let mut l = s.inner.lock();
        let min_recent = l
            .recent_recordings
            .front()
            .map(|r| r.id)
            .unwrap_or(l.committed.cum_recordings);
        let l = if desired_ids.start < min_recent {
            assert!(!l.recent_recordings_pinned);
            l.recent_recordings_pinned = true;
            drop(l);
            let db_end = min_recent.min(desired_ids.end);
            let db_res = raw::list_recordings_by_id(
                &self.conn.borrow(),
                stream_id,
                desired_ids.start..db_end,
                f,
            );
            let mut l = s.inner.lock();
            assert!(l.recent_recordings_pinned);
            l.recent_recordings_pinned = false;
            db_res?;
            l
        } else {
            l
        };
        let start_i = l
            .recent_recordings
            .partition_point(|r| r.id < desired_ids.start);
        for r in l
            .recent_recordings
            .iter()
            .skip(start_i)
            .take_while(|r| r.id < desired_ids.end)
            .filter(|r| !r.flags.contains(RecordingFlags::DELETED))
        {
            let row = r.to_list_row(CompositeId::new(stream_id, r.id), self.open.unwrap().id);
            f(row)?
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
            let has_trailing_zero = row.flags.contains(RecordingFlags::TRAILING_ZERO);
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
                        if a.first_uncommitted.is_none()
                            && row.flags.contains(RecordingFlags::UNCOMMITTED)
                        {
                            a.first_uncommitted = Some(recording_id);
                        }
                        a.growing = row.flags.contains(RecordingFlags::GROWING);
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
    ///
    /// This uses a LRU cache to reduce the number of retrievals from the database.
    ///
    /// # Caveats
    ///
    /// * `f` must not block because it is called while holding the database lock.
    /// * This function must not be called within `list_recordings_by_id` or `list_recordings_by_time`
    ///   because it attempts to acquire the stream lock, which may be held by those functions.
    ///   Use `ListRecordingsRow::with_playback` instead.
    pub fn with_recording_playback<R>(
        &self,
        id: CompositeId,
        f: &mut dyn FnMut(&RecordingPlayback) -> Result<R, Error>,
    ) -> Result<R, Error> {
        // Recent path.
        let s = self
            .streams_by_id
            .get(&id.stream())
            .ok_or_else(|| err!(Internal, msg("no stream for {}", id)))?;
        {
            let l = s.inner.lock();
            if let Ok(i) = l
                .recent_recordings
                .binary_search_by_key(&id.recording(), |r| r.id)
            {
                let r = &l.recent_recordings[i];
                return f(&RecordingPlayback {
                    video_index: &r.video_index,
                });
            } else if id.recording() >= l.committed.cum_recordings
                || l.recent_recordings
                    .front()
                    .is_some_and(|r| r.id <= id.recording())
            {
                // Avoid querying the database for a recording that would be in `recent_recordings` if it existed.
                bail!(Internal, msg("no such recording {}", id));
            }
        }
        self.with_recording_playback_db_path(id, f)
    }

    /// Helper for `LockedDatabase::with_recording_playback` and `ListRecordingsRow::with_playback`.
    /// This only checks the cache and database, not `recent_recordings`.
    fn with_recording_playback_db_path<R>(
        &self,
        id: CompositeId,
        f: &mut dyn FnMut(&RecordingPlayback) -> Result<R, Error>,
    ) -> Result<R, Error> {
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
                let conn = self.conn.borrow();
                let mut stmt = conn.prepare_cached(GET_RECORDING_PLAYBACK_SQL)?;
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
                bail!(Internal, msg("no such recording {id}"))
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
        let start = match s.inner.lock().to_delete.last() {
            None => 0,
            Some(row) => row.id.recording() + 1,
        };
        let mut to_delete = Vec::new();
        let mut bytes_to_delete = 0;
        let mut fs_bytes_to_delete = 0;
        raw::list_oldest_recordings(
            &self.conn.borrow(),
            CompositeId::new(stream_id, start),
            &mut |r| {
                if f(&r) {
                    to_delete.push(r);
                    let bytes = i64::from(r.sample_file_bytes);
                    bytes_to_delete += bytes;
                    fs_bytes_to_delete += round_up(bytes);
                    return true;
                }
                false
            },
        )?;
        let mut l = s.inner.lock();
        l.to_delete.extend(to_delete);
        l.bytes_to_delete += bytes_to_delete;
        l.fs_bytes_to_delete += fs_bytes_to_delete;
        Ok(())
    }

    /// Initializes the sample file dirs.
    /// To be called during construction.
    fn init_sample_file_dirs(&mut self) -> Result<(), Error> {
        info!("Loading sample file dirs");
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
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
            let SqlUuid(dir_uuid) = row.get(2)?;
            let open_id = row.get(3)?;
            let open_uuid = row.get(4)?;
            let last_complete_open = match (open_id, open_uuid) {
                (Some(id), Some(SqlUuid(uuid))) => Some(Open { id, uuid }),
                (None, None) => None,
                _ => bail!(Internal, msg("open table missing id {id}")),
            };
            let config = dir::Config {
                path: config.path,
                db_uuid: self.uuid,
                dir_uuid,
                last_complete_open,
                current_open: self.open,
                flusher_notify: self.flusher_notify.clone(),
            };
            self.sample_file_dirs_by_id.insert(
                id,
                SampleFileDir {
                    id,
                    pool: dir::Pool::new(config, raw::list_garbage(&self.conn.borrow(), id)?),
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
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
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
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
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
            let cum_recordings = row.get(5)?;
            let sample_file_dir_id: Option<i32> = row.get(3)?;
            let sample_file_dir = if let Some(id) = sample_file_dir_id {
                Some(SampleFileDir {
                    id,
                    pool: self
                        .sample_file_dirs_by_id
                        .get(&id)
                        .ok_or_else(|| {
                            err!(
                                DataLoss,
                                msg("no such sample file dir {id} for stream {id}")
                            )
                        })?
                        .pool
                        .clone(),
                })
            } else {
                None
            };
            self.streams_by_id.insert(
                id,
                Stream::new(LockedStream {
                    open_writer: false,
                    id,
                    type_,
                    camera_id,
                    sample_file_dir,
                    config: row.get(4)?,
                    to_delete: Vec::new(),
                    bytes_to_delete: 0,
                    fs_bytes_to_delete: 0,
                    committed: StreamCommitted {
                        cum_recordings,
                        ..Default::default()
                    },
                    complete: StreamComplete {
                        cum_recordings,
                        cum_media_duration: recording::Duration(row.get(6)?),
                        cum_runs: row.get(7)?,
                    },
                    flush_ready: cum_recordings,
                    recent_recordings: VecDeque::new(),
                    recent_recordings_pinned: false,
                    recent_frames: RecentFrames::default(),
                    writer_state: crate::dir::writer::State::default(),
                }),
            );
            c.streams[type_.index()] = Some(id);
        }
        info!("Loaded {} streams", self.streams_by_id.len());
        Ok(())
    }

    /// Adds a camera.
    pub fn add_camera(&mut self, mut camera: CameraChange) -> Result<i32, Error> {
        let uuid = Uuid::now_v7();
        let uuid_bytes = &uuid.as_bytes()[..];
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
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
        let streams = streams.apply(&self.sample_file_dirs_by_id, &mut self.streams_by_id);
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
                let l = s.inner.lock();
                change.streams[i] = StreamChange {
                    sample_file_dir_id: l.sample_file_dir.as_ref().map(|dir| dir.id),
                    config: l.config.clone(),
                };
            }
        }
        Ok(change)
    }

    /// Updates a camera.
    pub fn update_camera(&mut self, camera_id: i32, mut camera: CameraChange) -> Result<(), Error> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
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
        c.streams = streams.apply(&self.sample_file_dirs_by_id, &mut self.streams_by_id);
        Ok(())
    }

    /// Deletes a camera and its streams. The camera must have no recordings.
    pub fn delete_camera(&mut self, id: i32) -> Result<(), Error> {
        // TODO: also verify there are no uncommitted recordings.
        let Some(uuid) = self.cameras_by_id.get(&id).map(|c| c.uuid) else {
            bail!(NotFound, msg("no such camera {id}"));
        };
        let mut streams_to_delete = Vec::new();
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        {
            let mut stream_stmt = tx.prepare_cached(r"delete from stream where id = :id")?;
            for (stream_id, stream) in &self.streams_by_id {
                let stream = stream.inner.lock();
                if stream.camera_id != id {
                    continue;
                };
                if stream.committed.range.is_some() {
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
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
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
                let stream = stream.inner.lock();
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
            let mut s = s.inner.lock();
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
        self.auth.apply(&self.conn.borrow_mut(), change)
    }

    pub fn delete_user(&mut self, id: i32) -> Result<(), base::Error> {
        self.auth.delete_user(&mut self.conn.borrow_mut(), id)
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
        self.auth.login_by_password(
            &self.conn.borrow(),
            req,
            username,
            password,
            domain,
            session_flags,
        )
    }

    pub fn make_session(
        &mut self,
        creation: Request,
        uid: i32,
        domain: Option<Vec<u8>>,
        flags: i32,
        permissions: schema::Permissions,
    ) -> Result<(RawSessionId, &Session), base::Error> {
        self.auth.make_session(
            &self.conn.borrow(),
            creation,
            uid,
            domain,
            flags,
            permissions,
        )
    }

    pub fn authenticate_session(
        &mut self,
        req: auth::Request,
        sid: &auth::SessionHash,
    ) -> Result<(&auth::Session, &User), base::Error> {
        self.auth
            .authenticate_session(&self.conn.borrow(), req, sid)
    }

    pub fn revoke_session(
        &mut self,
        reason: auth::RevocationReason,
        detail: Option<String>,
        req: auth::Request,
        hash: &auth::SessionHash,
    ) -> Result<(), base::Error> {
        self.auth
            .revoke_session(&self.conn.borrow(), reason, detail, req, hash)
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
        let uuid = ::uuid::Uuid::now_v7();
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
pub struct Database<C: Clocks = clock::RealClocks> {
    /// This is wrapped in an `Option` to allow the `Drop` implementation and `close` to coexist.
    db: Option<Mutex<LockedDatabase, 1>>,

    /// This is kept separately from the `LockedDatabase` to allow the `lock()` operation itself to
    /// access it. It doesn't need a `Mutex` anyway; it's `Sync`, and all operations work on
    /// `&self`.
    clocks: C,
}

impl<C: Clocks> Drop for Database<C> {
    fn drop(&mut self) {
        if ::std::thread::panicking() {
            return; // don't flush while panicking.
        }
        if let Some(m) = self.db.take() {
            if let Err(e) = m.into_inner().flush(&self.clocks, "drop") {
                error!(err = %e.chain(), "final database flush failed");
            }
        }
    }
}

// Helpers for Database::lock().
#[track_caller]
fn acquisition(location: &Location) -> String {
    format!("database lock acquisition at {location}")
}
#[track_caller]
fn operation(location: &'static Location<'static>) -> String {
    format!("database operation at {location}")
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
        let open_monotonic = clocks.monotonic();
        let open = if read_write {
            let real = recording::Time::from(clocks.realtime());
            let mut stmt = conn
                .prepare(" insert into open (uuid, start_time_90k, boot_uuid) values (?, ?, ?)")?;
            let open_uuid = SqlUuid(Uuid::now_v7());
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
        let sample_entries = Arc::new(Mutex::new(sample_entries::State::load(&conn)?));
        let db = Database {
            db: Some(Mutex::new(LockedDatabase {
                conn: base::Antilock::new(conn),
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
                sample_entries,
                video_index_cache: RefCell::new(LinkedHashMap::with_capacity_and_hasher(
                    VIDEO_INDEX_CACHE_LEN + 1,
                    Default::default(),
                )),
                on_flush: tokio::sync::watch::channel(0).0,
                flusher_notify: Arc::new(tokio::sync::Notify::new()),
            })),
            clocks,
        };
        {
            let l = &mut *db.lock();
            l.init_sample_file_dirs()?;
            l.init_cameras()?;
            l.init_streams()?;
            for (&stream_id, ref mut stream) in &mut l.streams_by_id {
                // Avoid taking a stream lock, not just for efficiency but to avoid tripping the
                // debug assertion that the connection isn't used while a stream lock is held.
                let stream = Arc::get_mut(stream)
                    .expect("no other references yet")
                    .inner
                    .get_mut();
                let camera = l.cameras_by_id.get(&stream.camera_id).unwrap();
                init_recordings(&l.conn.borrow(), stream_id, camera, &mut *stream)?;
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
    #[track_caller]
    pub fn lock(&self) -> DatabaseGuard<'_, C> {
        let timer = clock::TimerGuard::new(&self.clocks, acquisition);
        let db = self.db.as_ref().unwrap().lock();
        drop(timer);
        let _timer = clock::TimerGuard::<_, _, _>::new(
            &self.clocks,
            operation as fn(&'static Location<'static>) -> String,
        );
        DatabaseGuard {
            clocks: &self.clocks,
            db,
            _timer,
        }
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
    pub async fn open_sample_file_dirs(&self, ids: &[i32]) -> Result<(), Error> {
        let mut in_progress = FastHashMap::with_capacity_and_hasher(ids.len(), Default::default());
        let open = {
            let mut l = self.lock();
            for &id in ids {
                let e = in_progress.entry(id);
                use ::std::collections::hash_map::Entry;
                let e = match e {
                    Entry::Occupied(_) => continue, // suppress duplicate.
                    Entry::Vacant(e) => e,
                };
                let dir = l
                    .sample_file_dirs_by_id
                    .get_mut(&id)
                    .ok_or_else(|| err!(NotFound, msg("no such dir {id}")))?;

                if dir.pool.is_open() {
                    continue;
                }
                e.insert((dir.pool.clone(), dir.pool.open(DIR_POOL_WORKERS)));
            }
            l.open
        };

        // Now, with lock released, wait for the open futures.
        for (pool, f) in &mut in_progress.values_mut() {
            f.await
                .map_err(|e| err!(e, msg("Failed to open dir {}", pool.path().display())))?;
        }

        let Some(o) = open.as_ref() else {
            return Ok(()); // read-only mode; all done.
        };

        {
            let mut l = self.lock();
            let mut conn = l.conn.borrow_mut();
            let tx = conn.transaction()?;
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
            drop(stmt);
            tx.commit()?;
        }

        for (pool, f) in in_progress.values_mut() {
            *f = pool.complete_open_for_write();
        }

        for (pool, f) in in_progress.values_mut() {
            f.await.map_err(|e| {
                err!(
                    e,
                    msg(
                        "Failed to complete open for write on dir {}",
                        pool.path().display()
                    )
                )
            })?;
        }

        Ok(())
    }

    pub async fn close_sample_file_dirs(&self, ids: &[i32]) -> Result<(), Error> {
        let mut to_close = Vec::new();
        {
            let l = self.lock();
            for &id in ids {
                let dir = l
                    .sample_file_dirs_by_id
                    .get(&id)
                    .ok_or_else(|| err!(NotFound, msg("no such dir {id}")))?;
                if dir.pool.is_open() {
                    to_close.push(dir.pool.clone());
                }
            }
        }
        for pool in to_close {
            pool.close().await?;
        }
        Ok(())
    }

    pub async fn add_sample_file_dir(&self, path: PathBuf) -> Result<i32, Error> {
        let open;
        let cfg = {
            let l = self.lock();
            let Some(o) = l.open else {
                bail!(FailedPrecondition, msg("database is read-only"));
            };
            open = o;
            dir::Config {
                path: path.clone(),
                db_uuid: l.uuid,
                dir_uuid: Uuid::now_v7(),
                last_complete_open: None,
                current_open: Some(o),
                flusher_notify: l.flusher_notify.clone(),
            }
        };

        let pool = dir::Pool::new(cfg, FastHashSet::default());
        pool.open(DIR_POOL_WORKERS).await?;

        let id;
        {
            let mut l = self.lock();
            let conn = l.conn.borrow();
            let config = SampleFileDirConfig {
                path,
                ..Default::default()
            };
            conn.execute(
                r#"
                insert into sample_file_dir (config, uuid, last_complete_open_id)
                                     values (?,      ?,    ?)
                "#,
                params![&config, SqlUuid(pool.config().dir_uuid), open.id],
            )?;
            id = conn.last_insert_rowid() as i32;
            use ::std::collections::btree_map::Entry;
            let e = l.sample_file_dirs_by_id.entry(id);
            match e {
                Entry::Vacant(e) => e.insert(SampleFileDir {
                    id,
                    pool: pool.clone(),
                }),
                Entry::Occupied(_) => bail!(Internal, msg("duplicate sample file dir id {id}")),
            };
        }
        pool.complete_open_for_write().await?;
        Ok(id)
    }

    /// Deletes a sample file directory—marks the directory's metadata as deleted and removes the entry from the database.
    ///
    /// XXX: This may not be not robust against concurrent access or failure. It's good enough for the current use from
    /// the separate `moonfire-nvr config` command but needs some additional thought to handle online
    /// reconfiguration.
    pub async fn delete_sample_file_dir(&self, dir_id: i32) -> Result<(), Error> {
        let path;
        let f = {
            let l = self.lock();
            for (&id, s) in l.streams_by_id.iter() {
                let s = s.inner.lock();
                if s.sample_file_dir
                    .as_ref()
                    .is_some_and(|dir| dir.id == dir_id)
                {
                    bail!(
                        FailedPrecondition,
                        msg("can't delete dir referenced by stream {id}")
                    );
                }
            }
            let Some(d) = l.sample_file_dirs_by_id.get(&dir_id) else {
                bail!(NotFound, msg("no such dir {dir_id} to remove"));
            };
            path = d.pool.path().to_owned();
            d.pool.mark_deleted()?
        };
        f.await?;
        let mut l = self.lock();
        if l.streams_by_id.values().any(|s| {
            let s = s.inner.lock();
            s.sample_file_dir
                .as_ref()
                .is_some_and(|dir| dir.id == dir_id)
        }) {
            bail!(
                FailedPrecondition,
                msg(
                    "stream reference was added while deleting directory {}; keeping in database",
                    path.display(),
                ),
            );
        }
        if l.conn
            .borrow()
            .execute("delete from sample_file_dir where id = ?", [dir_id])?
            != 1
        {
            bail!(FailedPrecondition, msg("failed to delete sample file dir"));
        }
        l.sample_file_dirs_by_id.remove(&dir_id);
        Ok(())
    }

    /// For testing: closes the database (without flushing) and returns the connection.
    /// This allows verification that a newly opened database is in an acceptable state.
    #[cfg(test)]
    fn close(mut self) -> rusqlite::Connection {
        self.db.take().unwrap().into_inner().conn.into_inner()
    }
}

/// Reference to a locked database returned by [Database::lock].
pub struct DatabaseGuard<'db, C: Clocks + Clone> {
    clocks: &'db C,
    db: MutexGuard<'db, LockedDatabase>,
    _timer: clock::TimerGuard<'db, C, String, fn(&'static Location<'static>) -> String>,
}

impl<C: Clocks + Clone> DatabaseGuard<'_, C> {
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

impl<C: Clocks + Clone> ::std::ops::Deref for DatabaseGuard<'_, C> {
    type Target = LockedDatabase;
    fn deref(&self) -> &LockedDatabase {
        &self.db
    }
}

impl<C: Clocks + Clone> ::std::ops::DerefMut for DatabaseGuard<'_, C> {
    fn deref_mut(&mut self) -> &mut LockedDatabase {
        &mut self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::{self, TIME_UNITS_PER_SEC};
    use crate::sample_entries::Video;
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
            let all_time = recording::Time(i64::MIN)..recording::Time(i64::MAX);
            db.list_recordings_by_time(stream_id, all_time, &mut |_row| {
                rows += 1;
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(0, rows);
    }

    #[track_caller]
    fn assert_single_recording(db: &Database, stream_id: i32, r: &RecentRecording) {
        {
            let db = db.lock();
            let stream = db.streams_by_id().get(&stream_id).unwrap();
            let stream = stream.inner.lock();
            let dur = recording::Duration(r.wall_duration_90k as i64);
            assert_eq!(Some(r.start..r.start + dur), stream.committed.range);
            assert_eq!(
                r.sample_file_bytes as i64,
                stream.committed.sample_file_bytes
            );
            assert_eq!(dur, stream.committed.duration);
            db.cameras_by_id().get(&stream.camera_id).unwrap();
        }

        // TODO(slamb): test that the days logic works correctly.

        let mut rows = 0;
        let mut recording_id = None;
        {
            let db = db.lock();
            let all_time = recording::Time(i64::MIN)..recording::Time(i64::MAX);
            db.list_recordings_by_time(stream_id, all_time, &mut |row| {
                rows += 1;
                recording_id = Some(row.id);
                assert_eq!(r.start, row.start);
                assert_eq!(r.wall_duration_90k, row.wall_duration_90k);
                assert_eq!(r.video_samples, row.video_samples);
                assert_eq!(r.video_sync_samples, row.video_sync_samples);
                assert_eq!(r.sample_file_bytes, row.sample_file_bytes);
                let vse = db
                    .sample_entries()
                    .lock()
                    .get_video(row.video_sample_entry_id)
                    .unwrap();
                assert_eq!(vse.1.rfc6381_codec, "avc1.4d0029");
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(1, rows);

        rows = 0;
        raw::list_oldest_recordings(
            &db.lock().conn.borrow(),
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
    #[tokio::test]
    async fn test_full_lifecycle() {
        testutil::init();
        let conn = setup_conn();
        let db = Arc::new(Database::new(clock::RealClocks {}, conn, true).unwrap());
        let tmpdir = tempfile::Builder::new()
            .prefix("moonfire-nvr-test")
            .tempdir()
            .unwrap();
        let path = tmpdir.path().to_owned();
        let (flusher_channel, flusher_join) = crate::lifecycle::start_flusher(db.clone());
        let sample_file_dir_id = db.add_sample_file_dir(path).await.unwrap();
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
                let main = main.inner.lock();
                assert_eq!(main.config.mode, crate::json::STREAM_MODE_RECORD);
                assert_eq!(main.config.retain_bytes, 42);
                assert_eq!(main.config.flush_if_sec, 1);
            }

            assert_eq!(
                l.streams_by_id()
                    .get(&sub_stream_id)
                    .unwrap()
                    .inner
                    .lock()
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
                    .inner
                    .lock()
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
                .inner
                .lock()
                .committed
                .cum_recordings,
            0
        );

        // Closing and reopening the database should present the same contents.
        db.lock()
            .sample_file_dirs_by_id()
            .get(&sample_file_dir_id)
            .unwrap()
            .pool
            .close()
            .await
            .unwrap();
        drop(flusher_channel);
        flusher_join.await.unwrap();
        let db = Arc::into_inner(db).expect("no other references to db exist");
        let conn = db.close();
        let db = Database::new(clock::RealClocks {}, conn, true).unwrap();
        assert_eq!(
            db.lock()
                .streams_by_id()
                .get(&sub_stream_id)
                .unwrap()
                .inner
                .lock()
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
                .inner
                .lock()
                .committed
                .cum_recordings,
            0
        );

        // TODO: assert_eq!(db.lock().list_garbage(sample_file_dir_id).unwrap(), &[]);

        let vse_id = db
            .lock()
            .sample_entries()
            .lock()
            .insert_video(Video {
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
        let recording = RecentRecording {
            id: 0, // placeholder.
            sample_file_bytes: 42,
            run_offset: 0,
            flags: RecordingFlags::UNCOMMITTED,
            start,
            prev_media_duration: recording::Duration(0),
            prev_runs: 0,
            wall_duration_90k: TIME_UNITS_PER_SEC.try_into().unwrap(),
            media_duration_90k: TIME_UNITS_PER_SEC.try_into().unwrap(),
            local_time_delta: recording::Duration(0),
            video_samples: 1,
            video_sync_samples: 1,
            video_sample_entry_id: vse_id,
            video_index: vec![0u8; 100],
            sample_file_blake3: None,
            end_reason: None,
        };
        let id = {
            let mut db = db.lock();
            let s = db.streams_by_id().get(&main_stream_id).unwrap();
            let mut s = s.inner.lock();
            let id = s.add_recording(recording.clone());
            s.writer_state.recording_id = id + 1;
            s.flush_ready = id + 1;
            drop(s);
            db.flush("add test").unwrap();
            CompositeId::new(main_stream_id, id)
        };
        assert_eq!(
            db.lock()
                .streams_by_id()
                .get(&main_stream_id)
                .unwrap()
                .inner
                .lock()
                .committed
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
                let s = s.inner.lock();
                assert_eq!(s.committed.sample_file_bytes, 42);
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
                    .inner
                    .lock()
                    .bytes_to_delete,
                42
            );
            db.flush("delete test").unwrap();
            let s = db.streams_by_id().get(&main_stream_id).unwrap();
            let s = s.inner.lock();
            assert_eq!(s.committed.sample_file_bytes, 0);
            assert_eq!(s.bytes_to_delete, 0);
        }
        assert_no_recordings(&db, camera_uuid);
        let db_l = db.lock();
        let p_l = db_l
            .sample_file_dirs_by_id()
            .get(&sample_file_dir_id)
            .unwrap()
            .pool
            .lock();
        assert_eq!(
            p_l.garbage_needs_unlink()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            &[id]
        );
        assert_eq!(p_l.garbage_unlinked(), &[]);
    }

    #[test]
    fn round_up() {
        assert_eq!(super::round_up(0), 0);
        assert_eq!(super::round_up(8_191), 8_192);
        assert_eq!(super::round_up(8_192), 8_192);
        assert_eq!(super::round_up(8_193), 12_288);
    }
}
