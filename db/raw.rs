// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2018 Scott Lamb <slamb@slamb.org>
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

use db::{self, CompositeId};
use failure::Error;
use fnv::FnvHashSet;
use recording;
use rusqlite;
use std::ops::Range;

const INSERT_RECORDING_SQL: &'static str = r#"
    insert into recording (composite_id, stream_id, run_offset, flags, sample_file_bytes,
                           start_time_90k, duration_90k, local_time_delta_90k, video_samples,
                           video_sync_samples, video_sample_entry_id)
                   values (:composite_id, :stream_id, :run_offset, :flags, :sample_file_bytes,
                           :start_time_90k, :duration_90k, :local_time_delta_90k,
                           :video_samples, :video_sync_samples, :video_sample_entry_id)
"#;

const INSERT_RECORDING_PLAYBACK_SQL: &'static str = r#"
    insert into recording_playback (composite_id,  open_id,  sample_file_sha1,  video_index)
                            values (:composite_id, :open_id, :sample_file_sha1, :video_index)
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

/// Inserts the specified recording (for from `try_flush` only).
pub(crate) fn insert_recording(tx: &rusqlite::Transaction, o: &db::Open, id: CompositeId,
                    r: &db::RecordingToInsert) -> Result<(), Error> {
    if r.time.end < r.time.start {
        bail!("end time {} must be >= start time {}", r.time.end, r.time.start);
    }

    let mut stmt = tx.prepare_cached(INSERT_RECORDING_SQL)?;
    stmt.execute_named(&[
        (":composite_id", &id.0),
        (":stream_id", &(id.stream() as i64)),
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

    let mut stmt = tx.prepare_cached(INSERT_RECORDING_PLAYBACK_SQL)?;
    let sha1 = &r.sample_file_sha1[..];
    stmt.execute_named(&[
        (":composite_id", &id.0),
        (":open_id", &o.id),
        (":sample_file_sha1", &sha1),
        (":video_index", &r.video_index),
    ])?;
    Ok(())
}

/// Deletes the given recordings from the `recording` and `recording_playback` tables.
/// Note they are not fully removed from the database; the ids are transferred to the
/// `garbage` table.
pub(crate) fn delete_recordings(tx: &rusqlite::Transaction, rows: &[db::ListOldestSampleFilesRow])
                                -> Result<(), Error> {
    let mut del1 = tx.prepare_cached(
        "delete from recording_playback where composite_id = :composite_id")?;
    let mut del2 = tx.prepare_cached(
        "delete from recording where composite_id = :composite_id")?;
    let mut insert = tx.prepare_cached(r#"
        insert into garbage (sample_file_dir_id,  composite_id)
                     values (:sample_file_dir_id, :composite_id)
    "#)?;
    for row in rows {
        let changes = del1.execute_named(&[(":composite_id", &row.id.0)])?;
        if changes != 1 {
            bail!("no such recording_playback {}", row.id);
        }
        let changes = del2.execute_named(&[(":composite_id", &row.id.0)])?;
        if changes != 1 {
            bail!("no such recording {}", row.id);
        }
        insert.execute_named(&[
            (":sample_file_dir_id", &row.sample_file_dir_id),
            (":composite_id", &row.id.0)],
        )?;
    }
    Ok(())
}

/// Marks the given sample files as deleted. This shouldn't be called until the files have
/// been `unlink()`ed and the parent directory `fsync()`ed.
pub(crate) fn mark_sample_files_deleted(tx: &rusqlite::Transaction, ids: &[CompositeId])
                                        -> Result<(), Error> {
    if ids.is_empty() { return Ok(()); }
    let mut stmt = tx.prepare_cached("delete from garbage where composite_id = ?")?;
    for &id in ids {
        let changes = stmt.execute(&[&id.0])?;
        if changes != 1 {
            bail!("no garbage row for {}", id);
        }
    }
    Ok(())
}

/// Gets the time range of recordings for the given stream.
pub(crate) fn get_range(conn: &rusqlite::Connection, stream_id: i32)
                        -> Result<Option<Range<recording::Time>>, Error> {
    // The minimum is straightforward, taking advantage of the start_time_90k index.
    let mut stmt = conn.prepare_cached(STREAM_MIN_START_SQL)?;
    let mut rows = stmt.query_named(&[(":stream_id", &stream_id)])?;
    let min_start = match rows.next() {
        Some(row) => recording::Time(row?.get_checked(0)?),
        None => return Ok(None),
    };

    // There was a minimum, so there should be a maximum too. Calculating it is less
    // straightforward because recordings could overlap. All recordings starting in the
    // last MAX_RECORDING_DURATION must be examined in order to take advantage of the
    // start_time_90k index.
    let mut stmt = conn.prepare_cached(STREAM_MAX_START_SQL)?;
    let mut rows = stmt.query_named(&[(":stream_id", &stream_id)])?;
    let mut maxes_opt = None;
    while let Some(row) = rows.next() {
        let row = row?;
        let row_start = recording::Time(row.get_checked(0)?);
        let row_duration: i64 = row.get_checked(1)?;
        let row_end = recording::Time(row_start.0 + row_duration);
        let maxes = match maxes_opt {
            None => row_start .. row_end,
            Some(Range{start: s, end: e}) => s .. ::std::cmp::max(e, row_end),
        };
        if row_start.0 <= maxes.start.0 - recording::MAX_RECORDING_DURATION {
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
    while let Some(row) = rows.next() {
        let row = row?;
        garbage.insert(CompositeId(row.get_checked(0)?));
    }
    Ok(garbage)
}
