// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Raw database access: SQLite statements which do not touch any cached state.

use crate::db::{self, CompositeId, SqlUuid};
use crate::json::GlobalConfig;
use crate::recording;
use base::{ErrorKind, ResultExt as _};
use failure::{bail, Error, ResultExt as _};
use fnv::FnvHashSet;
use rusqlite::{named_params, params};
use std::ops::Range;
use uuid::Uuid;

// Note: the magic number "27000000" below is recording::MAX_RECORDING_DURATION.
const LIST_RECORDINGS_BY_TIME_SQL: &str = r#"
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

const LIST_RECORDINGS_BY_ID_SQL: &str = r#"
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

const STREAM_MIN_START_SQL: &str = r#"
    select
      start_time_90k
    from
      recording
    where
      stream_id = :stream_id
    order by start_time_90k limit 1
"#;

const STREAM_MAX_START_SQL: &str = r#"
    select
      start_time_90k,
      wall_duration_90k
    from
      recording
    where
      stream_id = :stream_id
    order by start_time_90k desc;
"#;

const LIST_OLDEST_RECORDINGS_SQL: &str = r#"
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
    conn: &rusqlite::Connection,
    stream_id: i32,
    desired_time: Range<recording::Time>,
    f: &mut dyn FnMut(db::ListRecordingsRow) -> Result<(), base::Error>,
) -> Result<(), base::Error> {
    let mut stmt = conn
        .prepare_cached(LIST_RECORDINGS_BY_TIME_SQL)
        .err_kind(ErrorKind::Internal)?;
    let rows = stmt
        .query(named_params! {
            ":stream_id": stream_id,
            ":start_time_90k": desired_time.start.0,
            ":end_time_90k": desired_time.end.0,
        })
        .err_kind(ErrorKind::Internal)?;
    list_recordings_inner(rows, false, f)
}

/// Lists the specified recordings in ascending order by id.
pub(crate) fn list_recordings_by_id(
    conn: &rusqlite::Connection,
    stream_id: i32,
    desired_ids: Range<i32>,
    f: &mut dyn FnMut(db::ListRecordingsRow) -> Result<(), base::Error>,
) -> Result<(), base::Error> {
    let mut stmt = conn
        .prepare_cached(LIST_RECORDINGS_BY_ID_SQL)
        .err_kind(ErrorKind::Internal)?;
    let rows = stmt
        .query(named_params! {
            ":start": CompositeId::new(stream_id, desired_ids.start).0,
            ":end": CompositeId::new(stream_id, desired_ids.end).0,
        })
        .err_kind(ErrorKind::Internal)?;
    list_recordings_inner(rows, true, f)
}

fn list_recordings_inner(
    mut rows: rusqlite::Rows,
    include_prev: bool,
    f: &mut dyn FnMut(db::ListRecordingsRow) -> Result<(), base::Error>,
) -> Result<(), base::Error> {
    while let Some(row) = rows.next().err_kind(ErrorKind::Internal)? {
        let wall_duration_90k = row.get(4).err_kind(ErrorKind::Internal)?;
        let media_duration_delta_90k: i32 = row.get(5).err_kind(ErrorKind::Internal)?;
        f(db::ListRecordingsRow {
            id: CompositeId(row.get(0).err_kind(ErrorKind::Internal)?),
            run_offset: row.get(1).err_kind(ErrorKind::Internal)?,
            flags: row.get(2).err_kind(ErrorKind::Internal)?,
            start: recording::Time(row.get(3).err_kind(ErrorKind::Internal)?),
            wall_duration_90k,
            media_duration_90k: wall_duration_90k + media_duration_delta_90k,
            sample_file_bytes: row.get(6).err_kind(ErrorKind::Internal)?,
            video_samples: row.get(7).err_kind(ErrorKind::Internal)?,
            video_sync_samples: row.get(8).err_kind(ErrorKind::Internal)?,
            video_sample_entry_id: row.get(9).err_kind(ErrorKind::Internal)?,
            open_id: row.get(10).err_kind(ErrorKind::Internal)?,
            prev_media_duration_and_runs: match include_prev {
                false => None,
                true => Some((
                    recording::Duration(row.get(11).err_kind(ErrorKind::Internal)?),
                    row.get(12).err_kind(ErrorKind::Internal)?,
                )),
            },
        })?;
    }
    Ok(())
}

pub(crate) fn read_meta(conn: &rusqlite::Connection) -> Result<(Uuid, GlobalConfig), Error> {
    Ok(conn.query_row(
        "select uuid, config from meta",
        params![],
        |row| -> rusqlite::Result<(Uuid, GlobalConfig)> {
            let uuid: SqlUuid = row.get(0)?;
            let config: GlobalConfig = row.get(1)?;
            Ok((uuid.0, config))
        },
    )?)
}

/// Inserts the specified recording (for from `try_flush` only).
pub(crate) fn insert_recording(
    tx: &rusqlite::Transaction,
    o: &db::Open,
    id: CompositeId,
    r: &db::RecordingToInsert,
) -> Result<(), Error> {
    let mut stmt = tx
        .prepare_cached(
            r#"
            insert into recording (composite_id, stream_id, open_id, run_offset, flags,
                               sample_file_bytes, start_time_90k, prev_media_duration_90k,
                               prev_runs, wall_duration_90k, media_duration_delta_90k,
                               video_samples, video_sync_samples, video_sample_entry_id,
                               end_reason)
                       values (:composite_id, :stream_id, :open_id, :run_offset, :flags,
                               :sample_file_bytes, :start_time_90k, :prev_media_duration_90k,
                               :prev_runs, :wall_duration_90k, :media_duration_delta_90k,
                               :video_samples, :video_sync_samples, :video_sample_entry_id,
                               :end_reason)
            "#,
        )
        .with_context(|e| format!("can't prepare recording insert: {}", e))?;
    stmt.execute(named_params! {
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
        ":end_reason": r.end_reason.as_deref(),
    })
    .with_context(|e| {
        format!(
            "unable to insert recording for recording {} {:#?}: {}",
            id, r, e
        )
    })?;

    let mut stmt = tx
        .prepare_cached(
            r#"
            insert into recording_integrity (composite_id,  local_time_delta_90k,
                                             sample_file_blake3)
                                     values (:composite_id, :local_time_delta_90k,
                                             :sample_file_blake3)
            "#,
        )
        .with_context(|e| format!("can't prepare recording_integrity insert: {}", e))?;
    let blake3 = r.sample_file_blake3.as_ref().map(|b| &b[..]);
    let delta = match r.run_offset {
        0 => None,
        _ => Some(r.local_time_delta.0),
    };
    stmt.execute(named_params! {
        ":composite_id": id.0,
        ":local_time_delta_90k": delta,
        ":sample_file_blake3": blake3,
    })
    .with_context(|e| format!("unable to insert recording_integrity for {:#?}: {}", r, e))?;

    let mut stmt = tx
        .prepare_cached(
            r#"
            insert into recording_playback (composite_id,  video_index)
                                    values (:composite_id, :video_index)
            "#,
        )
        .with_context(|e| format!("can't prepare recording_playback insert: {}", e))?;
    stmt.execute(named_params! {
        ":composite_id": id.0,
        ":video_index": &r.video_index,
    })
    .with_context(|e| format!("unable to insert recording_playback for {:#?}: {}", r, e))?;

    Ok(())
}

/// Transfers the given recording range from the `recording` and associated tables to the `garbage`
/// table. `sample_file_dir_id` is assumed to be correct.
///
/// Returns the number of recordings which were deleted.
pub(crate) fn delete_recordings(
    tx: &rusqlite::Transaction,
    sample_file_dir_id: i32,
    ids: Range<CompositeId>,
) -> Result<usize, Error> {
    let mut insert = tx.prepare_cached(
        r#"
        insert into garbage (sample_file_dir_id, composite_id)
        select
          :sample_file_dir_id,
          composite_id
        from
          recording
        where
          :start <= composite_id and
          composite_id < :end
        "#,
    )?;
    let mut del_playback = tx.prepare_cached(
        r#"
        delete from recording_playback
        where
          :start <= composite_id and
          composite_id < :end
        "#,
    )?;
    let mut del_integrity = tx.prepare_cached(
        r#"
        delete from recording_integrity
        where
          :start <= composite_id and
          composite_id < :end
        "#,
    )?;
    let mut del_main = tx.prepare_cached(
        r#"
        delete from recording
        where
          :start <= composite_id and
          composite_id < :end
        "#,
    )?;
    let n = insert.execute(named_params! {
        ":sample_file_dir_id": sample_file_dir_id,
        ":start": ids.start.0,
        ":end": ids.end.0,
    })?;
    let p = named_params! {
        ":start": ids.start.0,
        ":end": ids.end.0,
    };
    let n_playback = del_playback.execute(p)?;
    if n_playback != n {
        bail!(
            "inserted {} garbage rows but deleted {} recording_playback rows!",
            n,
            n_playback
        );
    }
    let n_integrity = del_integrity.execute(p)?;
    if n_integrity > n {
        // fewer is okay; recording_integrity is optional.
        bail!(
            "inserted {} garbage rows but deleted {} recording_integrity rows!",
            n,
            n_integrity
        );
    }
    let n_main = del_main.execute(p)?;
    if n_main != n {
        bail!(
            "inserted {} garbage rows but deleted {} recording rows!",
            n,
            n_main
        );
    }
    Ok(n)
}

/// Marks the given sample files as deleted. This shouldn't be called until the files have
/// been `unlink()`ed and the parent directory `fsync()`ed.
pub(crate) fn mark_sample_files_deleted(
    tx: &rusqlite::Transaction,
    ids: &[CompositeId],
) -> Result<(), Error> {
    if ids.is_empty() {
        return Ok(());
    }
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
pub(crate) fn get_range(
    conn: &rusqlite::Connection,
    stream_id: i32,
) -> Result<Option<Range<recording::Time>>, Error> {
    // The minimum is straightforward, taking advantage of the start_time_90k index.
    let mut stmt = conn.prepare_cached(STREAM_MIN_START_SQL)?;
    let mut rows = stmt.query(named_params! {":stream_id": stream_id})?;
    let min_start = match rows.next()? {
        Some(row) => recording::Time(row.get(0)?),
        None => return Ok(None),
    };

    // There was a minimum, so there should be a maximum too. Calculating it is less
    // straightforward because recordings could overlap. All recordings starting in the
    // last MAX_RECORDING_DURATION must be examined in order to take advantage of the
    // start_time_90k index.
    let mut stmt = conn.prepare_cached(STREAM_MAX_START_SQL)?;
    let mut rows = stmt.query(named_params! {":stream_id": stream_id})?;
    let mut maxes_opt = None;
    while let Some(row) = rows.next()? {
        let row_start = recording::Time(row.get(0)?);
        let row_duration: i64 = row.get(1)?;
        let row_end = recording::Time(row_start.0 + row_duration);
        let maxes = match maxes_opt {
            None => row_start..row_end,
            Some(Range { start: s, end: e }) => s..::std::cmp::max(e, row_end),
        };
        if row_start.0 <= maxes.start.0 - recording::MAX_RECORDING_WALL_DURATION {
            break;
        }
        maxes_opt = Some(maxes);
    }
    let max_end = match maxes_opt {
        Some(Range { start: _, end: e }) => e,
        None => bail!(
            "missing max for stream {} which had min {}",
            stream_id,
            min_start
        ),
    };
    Ok(Some(min_start..max_end))
}

/// Lists all garbage ids for the given sample file directory.
pub(crate) fn list_garbage(
    conn: &rusqlite::Connection,
    dir_id: i32,
) -> Result<FnvHashSet<CompositeId>, Error> {
    let mut garbage = FnvHashSet::default();
    let mut stmt =
        conn.prepare_cached("select composite_id from garbage where sample_file_dir_id = ?")?;
    let mut rows = stmt.query([&dir_id])?;
    while let Some(row) = rows.next()? {
        garbage.insert(CompositeId(row.get(0)?));
    }
    Ok(garbage)
}

/// Lists the oldest recordings for a stream, starting with the given id.
/// `f` should return true as long as further rows are desired.
pub(crate) fn list_oldest_recordings(
    conn: &rusqlite::Connection,
    start: CompositeId,
    f: &mut dyn FnMut(db::ListOldestRecordingsRow) -> bool,
) -> Result<(), Error> {
    let mut stmt = conn.prepare_cached(LIST_OLDEST_RECORDINGS_SQL)?;
    let mut rows = stmt.query(named_params! {
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
