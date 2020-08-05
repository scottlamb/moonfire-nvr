// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018-2020 The Moonfire NVR Authors
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

//! Raw database access: SQLite statements which do not touch any cached state.

use crate::db::{self, CompositeId, FromSqlUuid};
use failure::{Error, ResultExt, bail};
use fnv::FnvHashSet;
use crate::recording;
use rusqlite::{named_params, params};
use std::ops::Range;
use uuid::Uuid;

// Note: the magic number "27000000" below is recording::MAX_RECORDING_DURATION.
const LIST_RECORDINGS_BY_TIME_SQL: &'static str = r#"
    select
        recording.composite_id,
        recording.run_offset,
        recording.flags,
        recording.start_time_90k,
        recording.wall_duration_90k,
        recording.media_duration_delta_90k,
        recording.sample_file_bytes,
        recording.video_samples,
        recording.video_sync_samples,
        recording.video_sample_entry_id,
        recording.open_id
    from
        recording
    where
        stream_id = :stream_id and
        recording.start_time_90k > :start_time_90k - 27000000 and
        recording.start_time_90k < :end_time_90k and
        recording.start_time_90k + recording.wall_duration_90k > :start_time_90k
    order by
        recording.start_time_90k
"#;

const LIST_RECORDINGS_BY_ID_SQL: &'static str = r#"
    select
        recording.composite_id,
        recording.run_offset,
        recording.flags,
        recording.start_time_90k,
        recording.wall_duration_90k,
        recording.media_duration_delta_90k,
        recording.sample_file_bytes,
        recording.video_samples,
        recording.video_sync_samples,
        recording.video_sample_entry_id,
        recording.open_id,
        recording.prev_media_duration_90k,
        recording.prev_runs
    from
        recording
    where
        :start <= composite_id and
        composite_id < :end
    order by
        recording.composite_id
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
      wall_duration_90k
    from
      recording
    where
      stream_id = :stream_id
    order by start_time_90k desc;
"#;

const LIST_OLDEST_RECORDINGS_SQL: &'static str = r#"
    select
      composite_id,
      start_time_90k,
      wall_duration_90k,
      sample_file_bytes
    from
      recording
    where
      :start <= composite_id and
      composite_id < :end
    order by
      composite_id
"#;

/// Lists the specified recordings in ascending order by start time, passing them to a supplied
/// function. Given that the function is called with the database lock held, it should be quick.
pub(crate) fn list_recordings_by_time(
    conn: &rusqlite::Connection, stream_id: i32, desired_time: Range<recording::Time>,
    f: &mut dyn FnMut(db::ListRecordingsRow) -> Result<(), Error>) -> Result<(), Error> {
    let mut stmt = conn.prepare_cached(LIST_RECORDINGS_BY_TIME_SQL)?;
    let rows = stmt.query_named(named_params!{
        ":stream_id": stream_id,
        ":start_time_90k": desired_time.start.0,
        ":end_time_90k": desired_time.end.0,
    })?;
    list_recordings_inner(rows, false, f)
}

/// Lists the specified recordings in ascending order by id.
pub(crate) fn list_recordings_by_id(
    conn: &rusqlite::Connection, stream_id: i32, desired_ids: Range<i32>,
    f: &mut dyn FnMut(db::ListRecordingsRow) -> Result<(), Error>) -> Result<(), Error> {
    let mut stmt = conn.prepare_cached(LIST_RECORDINGS_BY_ID_SQL)?;
    let rows = stmt.query_named(named_params!{
        ":start": CompositeId::new(stream_id, desired_ids.start).0,
        ":end": CompositeId::new(stream_id, desired_ids.end).0,
    })?;
    list_recordings_inner(rows, true, f)
}

fn list_recordings_inner(mut rows: rusqlite::Rows, include_prev: bool,
                         f: &mut dyn FnMut(db::ListRecordingsRow) -> Result<(), Error>)
                         -> Result<(), Error> {
    while let Some(row) = rows.next()? {
        let wall_duration_90k = row.get(4)?;
        let media_duration_delta_90k: i32 = row.get(5)?;
        f(db::ListRecordingsRow {
            id: CompositeId(row.get(0)?),
            run_offset: row.get(1)?,
            flags: row.get(2)?,
            start: recording::Time(row.get(3)?),
            wall_duration_90k,
            media_duration_90k: wall_duration_90k + media_duration_delta_90k,
            sample_file_bytes: row.get(6)?,
            video_samples: row.get(7)?,
            video_sync_samples: row.get(8)?,
            video_sample_entry_id: row.get(9)?,
            open_id: row.get(10)?,
            prev_media_duration_and_runs: match include_prev {
                false => None,
                true => Some((recording::Duration(row.get(11)?), row.get(12)?)),
            },
        })?;
    }
    Ok(())
}

pub(crate) fn get_db_uuid(conn: &rusqlite::Connection) -> Result<Uuid, Error> {
    Ok(conn.query_row("select uuid from meta", params![], |row| -> rusqlite::Result<Uuid> {
        let uuid: FromSqlUuid = row.get(0)?;
        Ok(uuid.0)
    })?)
}

/// Inserts the specified recording (for from `try_flush` only).
pub(crate) fn insert_recording(tx: &rusqlite::Transaction, o: &db::Open, id: CompositeId,
                               r: &db::RecordingToInsert) -> Result<(), Error> {
    let mut stmt = tx.prepare_cached(r#"
        insert into recording (composite_id, stream_id, open_id, run_offset, flags,
                               sample_file_bytes, start_time_90k, prev_media_duration_90k,
                               prev_runs, wall_duration_90k, media_duration_delta_90k,
                               video_samples, video_sync_samples, video_sample_entry_id)
                       values (:composite_id, :stream_id, :open_id, :run_offset, :flags,
                               :sample_file_bytes, :start_time_90k, :prev_media_duration_90k,
                               :prev_runs, :wall_duration_90k, :media_duration_delta_90k,
                               :video_samples, :video_sync_samples, :video_sample_entry_id)
    "#).with_context(|e| format!("can't prepare recording insert: {}", e))?;
    stmt.execute_named(named_params!{
        ":composite_id": id.0,
        ":stream_id": i64::from(id.stream()),
        ":open_id": o.id,
        ":run_offset": r.run_offset,
        ":flags": r.flags,
        ":sample_file_bytes": r.sample_file_bytes,
        ":start_time_90k": r.start.0,
        ":wall_duration_90k": r.wall_duration_90k,
        ":media_duration_delta_90k": r.media_duration_90k - r.wall_duration_90k,
        ":prev_media_duration_90k": r.prev_media_duration.0,
        ":prev_runs": r.prev_runs,
        ":video_samples": r.video_samples,
        ":video_sync_samples": r.video_sync_samples,
        ":video_sample_entry_id": r.video_sample_entry_id,
    }).with_context(|e| format!("unable to insert recording for recording {} {:#?}: {}",
                                id, r, e))?;

    let mut stmt = tx.prepare_cached(r#"
        insert into recording_integrity (composite_id,  local_time_delta_90k,  sample_file_blake3)
                                 values (:composite_id, :local_time_delta_90k, :sample_file_blake3)
    "#).with_context(|e| format!("can't prepare recording_integrity insert: {}", e))?;
    let blake3 = r.sample_file_blake3.as_ref().map(|b| &b[..]);
    let delta = match r.run_offset {
        0 => None,
        _ => Some(r.local_time_delta.0),
    };
    stmt.execute_named(named_params!{
        ":composite_id": id.0,
        ":local_time_delta_90k": delta,
        ":sample_file_blake3": blake3,
    }).with_context(|e| format!("unable to insert recording_integrity for {:#?}: {}", r, e))?;

    let mut stmt = tx.prepare_cached(r#"
        insert into recording_playback (composite_id,  video_index)
                                values (:composite_id, :video_index)
    "#).with_context(|e| format!("can't prepare recording_playback insert: {}", e))?;
    stmt.execute_named(named_params!{
        ":composite_id": id.0,
        ":video_index": &r.video_index,
    }).with_context(|e| format!("unable to insert recording_playback for {:#?}: {}", r, e))?;

    Ok(())
}

/// Transfers the given recording range from the `recording` and associated tables to the `garbage`
/// table. `sample_file_dir_id` is assumed to be correct.
///
/// Returns the number of recordings which were deleted.
pub(crate) fn delete_recordings(tx: &rusqlite::Transaction, sample_file_dir_id: i32,
                                ids: Range<CompositeId>)
                                -> Result<usize, Error> {
    let mut insert = tx.prepare_cached(r#"
        insert into garbage (sample_file_dir_id, composite_id)
        select
          :sample_file_dir_id,
          composite_id
        from
          recording
        where
          :start <= composite_id and
          composite_id < :end
    "#)?;
    let mut del_playback = tx.prepare_cached(r#"
        delete from recording_playback
        where
          :start <= composite_id and
          composite_id < :end
    "#)?;
    let mut del_integrity = tx.prepare_cached(r#"
        delete from recording_integrity
        where
          :start <= composite_id and
          composite_id < :end
    "#)?;
    let mut del_detection = tx.prepare_cached(r#"
        delete from recording_object_detection
        where
          :start <= composite_id and
          composite_id < :end
    "#)?;
    let mut del_main = tx.prepare_cached(r#"
        delete from recording
        where
          :start <= composite_id and
          composite_id < :end
    "#)?;
    let n = insert.execute_named(named_params!{
        ":sample_file_dir_id": sample_file_dir_id,
        ":start": ids.start.0,
        ":end": ids.end.0,
    })?;
    let p = named_params!{
        ":start": ids.start.0,
        ":end": ids.end.0,
    };
    let n_playback = del_playback.execute_named(p)?;
    if n_playback != n {
        bail!("inserted {} garbage rows but deleted {} recording_playback rows!", n, n_playback);
    }
    let n_integrity = del_integrity.execute_named(p)?;
    if n_integrity > n {  // fewer is okay; recording_integrity is optional.
        bail!("inserted {} garbage rows but deleted {} recording_integrity rows!", n, n_integrity);
    }
    // Any number of object detection rows is okay, as there can be zero or more models per
    // recording.
    del_detection.execute_named(p)?;
    let n_main = del_main.execute_named(p)?;
    if n_main != n {
        bail!("inserted {} garbage rows but deleted {} recording rows!", n, n_main);
    }
    Ok(n)
}

/// Marks the given sample files as deleted. This shouldn't be called until the files have
/// been `unlink()`ed and the parent directory `fsync()`ed.
pub(crate) fn mark_sample_files_deleted(tx: &rusqlite::Transaction, ids: &[CompositeId])
                                        -> Result<(), Error> {
    if ids.is_empty() { return Ok(()); }
    let mut stmt = tx.prepare_cached("delete from garbage where composite_id = ?")?;
    for &id in ids {
        let changes = stmt.execute(params![id.0])?;
        if changes != 1 {
            // panic rather than return error. Errors get retried indefinitely, but there's no
            // recovery from this condition.
            //
            // Tempting to just consider logging error and moving on, but this represents a logic
            // flaw, so complain loudly. The freshly deleted file might still be referenced in the
            // recording table.
            panic!("no garbage row for {}", id);
        }
    }
    Ok(())
}

/// Gets the time range of recordings for the given stream.
pub(crate) fn get_range(conn: &rusqlite::Connection, stream_id: i32)
                        -> Result<Option<Range<recording::Time>>, Error> {
    // The minimum is straightforward, taking advantage of the start_time_90k index.
    let mut stmt = conn.prepare_cached(STREAM_MIN_START_SQL)?;
    let mut rows = stmt.query_named(named_params!{":stream_id": stream_id})?;
    let min_start = match rows.next()? {
        Some(row) => recording::Time(row.get(0)?),
        None => return Ok(None),
    };

    // There was a minimum, so there should be a maximum too. Calculating it is less
    // straightforward because recordings could overlap. All recordings starting in the
    // last MAX_RECORDING_DURATION must be examined in order to take advantage of the
    // start_time_90k index.
    let mut stmt = conn.prepare_cached(STREAM_MAX_START_SQL)?;
    let mut rows = stmt.query_named(named_params!{":stream_id": stream_id})?;
    let mut maxes_opt = None;
    while let Some(row) = rows.next()? {
        let row_start = recording::Time(row.get(0)?);
        let row_duration: i64 = row.get(1)?;
        let row_end = recording::Time(row_start.0 + row_duration);
        let maxes = match maxes_opt {
            None => row_start .. row_end,
            Some(Range{start: s, end: e}) => s .. ::std::cmp::max(e, row_end),
        };
        if row_start.0 <= maxes.start.0 - recording::MAX_RECORDING_WALL_DURATION {
            break;
        }
        maxes_opt = Some(maxes);
    }
    let max_end = match maxes_opt {
        Some(Range{start: _, end: e}) => e,
        None => bail!("missing max for stream {} which had min {}", stream_id, min_start),
    };
    Ok(Some(min_start .. max_end))
}

/// Lists all garbage ids for the given sample file directory.
pub(crate) fn list_garbage(conn: &rusqlite::Connection, dir_id: i32)
                           -> Result<FnvHashSet<CompositeId>, Error> {
    let mut garbage = FnvHashSet::default();
    let mut stmt = conn.prepare_cached(
        "select composite_id from garbage where sample_file_dir_id = ?")?;
    let mut rows = stmt.query(&[&dir_id])?;
    while let Some(row) = rows.next()? {
        garbage.insert(CompositeId(row.get(0)?));
    }
    Ok(garbage)
}

/// Lists the oldest recordings for a stream, starting with the given id.
/// `f` should return true as long as further rows are desired.
pub(crate) fn list_oldest_recordings(conn: &rusqlite::Connection, start: CompositeId,
                                     f: &mut dyn FnMut(db::ListOldestRecordingsRow) -> bool)
    -> Result<(), Error> {
    let mut stmt = conn.prepare_cached(LIST_OLDEST_RECORDINGS_SQL)?;
    let mut rows = stmt.query_named(named_params!{
        ":start": start.0,
        ":end": CompositeId::new(start.stream() + 1, 0).0,
    })?;
    while let Some(row) = rows.next()? {
        let should_continue = f(db::ListOldestRecordingsRow {
            id: CompositeId(row.get(0)?),
            start: recording::Time(row.get(1)?),
            wall_duration_90k: row.get(2)?,
            sample_file_bytes: row.get(3)?,
        });
        if !should_continue {
            break;
        }
    }
    Ok(())
}
