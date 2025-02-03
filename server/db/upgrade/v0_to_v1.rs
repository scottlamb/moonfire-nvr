// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2016 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

/// Upgrades a version 0 schema to a version 1 schema.
use crate::db;
use crate::recording;
use base::Error;
use base::FastHashMap;
use rusqlite::{named_params, params};
use tracing::warn;

pub fn run(_args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    // These create statements match the schema.sql when version 1 was the latest.
    tx.execute_batch(
        r#"
        alter table camera rename to old_camera;
        create table camera (
          id integer primary key,
          uuid blob unique not null check (length(uuid) = 16),
          short_name text not null,
          description text,
          host text,
          username text,
          password text,
          main_rtsp_path text,
          sub_rtsp_path text,
          retain_bytes integer not null check (retain_bytes >= 0),
          next_recording_id integer not null check (next_recording_id >= 0)
        );
        alter table recording rename to old_recording;
        drop index recording_cover;
        create table recording (
          composite_id integer primary key,
          camera_id integer not null references camera (id),
          run_offset integer not null,
          flags integer not null,
          sample_file_bytes integer not null check (sample_file_bytes > 0),
          start_time_90k integer not null check (start_time_90k > 0),
          duration_90k integer not null
              check (duration_90k >= 0 and duration_90k < 5*60*90000),
          local_time_delta_90k integer not null,
          video_samples integer not null check (video_samples > 0),
          video_sync_samples integer not null check (video_samples > 0),
          video_sample_entry_id integer references video_sample_entry (id),
          check (composite_id >> 32 = camera_id)
        );
        create index recording_cover on recording (
          camera_id,
          start_time_90k,
          duration_90k,
          video_samples,
          video_sync_samples,
          video_sample_entry_id,
          sample_file_bytes,
          run_offset,
          flags
        );
        create table recording_playback (
          composite_id integer primary key references recording (composite_id),
          sample_file_uuid blob not null check (length(sample_file_uuid) = 16),
          sample_file_sha1 blob not null check (length(sample_file_sha1) = 20),
          video_index blob not null check (length(video_index) > 0)
        );
        insert into camera
        select
          id,
          uuid,
          short_name,
          description,
          host,
          username,
          password,
          main_rtsp_path,
          sub_rtsp_path,
          retain_bytes,
          1 as next_recording_id
        from
          old_camera;
    "#,
    )?;
    let camera_state = fill_recording(tx)?;
    update_camera(tx, camera_state)?;
    tx.execute_batch(
        r#"
      drop table old_recording;
      drop table old_camera;
    "#,
    )?;
    Ok(())
}

struct CameraState {
    /// tuple of (run_start_id, next_start_90k).
    current_run: Option<(i64, i64)>,

    /// As in the `next_recording_id` field of the `camera` table.
    next_recording_id: i32,
}

fn has_trailing_zero(video_index: &[u8]) -> Result<bool, Error> {
    let mut it = recording::SampleIndexIterator::default();
    while it.next(video_index)? {}
    Ok(it.duration_90k == 0)
}

/// Fills the `recording` and `recording_playback` tables from `old_recording`, returning
/// the `camera_state` map for use by a following call to `fill_cameras`.
fn fill_recording(tx: &rusqlite::Transaction) -> Result<FastHashMap<i32, CameraState>, Error> {
    let mut select = tx.prepare(
        r#"
      select
        camera_id,
        sample_file_bytes,
        start_time_90k,
        duration_90k,
        local_time_delta_90k,
        video_samples,
        video_sync_samples,
        video_sample_entry_id,
        sample_file_uuid,
        sample_file_sha1,
        video_index,
        id
      from
        old_recording
    "#,
    )?;
    let mut insert1 = tx.prepare(
        r#"
      insert into recording values (:composite_id, :camera_id, :run_offset, :flags,
                                    :sample_file_bytes, :start_time_90k, :duration_90k,
                                    :local_time_delta_90k, :video_samples, :video_sync_samples,
                                    :video_sample_entry_id)
    "#,
    )?;
    let mut insert2 = tx.prepare(
        r#"
      insert into recording_playback values (:composite_id, :sample_file_uuid, :sample_file_sha1,
                                             :video_index)
    "#,
    )?;
    let mut rows = select.query(params![])?;
    let mut camera_state: FastHashMap<i32, CameraState> = FastHashMap::default();
    while let Some(row) = rows.next()? {
        let camera_id: i32 = row.get(0)?;
        let camera_state = camera_state
            .entry(camera_id)
            .or_insert_with(|| CameraState {
                current_run: None,
                next_recording_id: 1,
            });
        let composite_id = ((camera_id as i64) << 32) | (camera_state.next_recording_id as i64);
        camera_state.next_recording_id += 1;
        let sample_file_bytes: i32 = row.get(1)?;
        let start_time_90k: i64 = row.get(2)?;
        let duration_90k: i32 = row.get(3)?;
        let local_time_delta_90k: i64 = row.get(4)?;
        let video_samples: i32 = row.get(5)?;
        let video_sync_samples: i32 = row.get(6)?;
        let video_sample_entry_id: i32 = row.get(7)?;
        let sample_file_uuid: db::SqlUuid = row.get(8)?;
        let sample_file_sha1: Vec<u8> = row.get(9)?;
        let video_index: Vec<u8> = row.get(10)?;
        let old_id: i32 = row.get(11)?;
        let trailing_zero = has_trailing_zero(&video_index).unwrap_or_else(|e| {
            warn!(
                "recording {}/{} (sample file {}, formerly recording {}) has corrupt \
                video_index: {}",
                camera_id,
                composite_id & 0xFFFF,
                sample_file_uuid.0,
                old_id,
                e
            );
            false
        });
        let run_id = match camera_state.current_run {
            Some((run_id, expected_start)) if expected_start == start_time_90k => run_id,
            _ => composite_id,
        };
        insert1.execute(named_params! {
            ":composite_id": &composite_id,
            ":camera_id": &camera_id,
            ":run_offset": &(composite_id - run_id),
            ":flags": &(
                if trailing_zero {
                    db::RecordingFlags::TrailingZero as i32
                } else {
                    0
                }),
            ":sample_file_bytes": &sample_file_bytes,
            ":start_time_90k": &start_time_90k,
            ":duration_90k": &duration_90k,
            ":local_time_delta_90k": &local_time_delta_90k,
            ":video_samples": &video_samples,
            ":video_sync_samples": &video_sync_samples,
            ":video_sample_entry_id": &video_sample_entry_id,
        })?;
        insert2.execute(named_params! {
            ":composite_id": &composite_id,
            ":sample_file_uuid": &sample_file_uuid.0.as_bytes()[..],
            ":sample_file_sha1": &sample_file_sha1,
            ":video_index": &video_index,
        })?;
        camera_state.current_run = if trailing_zero {
            None
        } else {
            Some((run_id, start_time_90k + duration_90k as i64))
        };
    }
    Ok(camera_state)
}

fn update_camera(
    tx: &rusqlite::Transaction,
    camera_state: FastHashMap<i32, CameraState>,
) -> Result<(), Error> {
    let mut stmt = tx.prepare(
        r#"
      update camera set next_recording_id = :next_recording_id where id = :id
    "#,
    )?;
    for (ref id, state) in &camera_state {
        stmt.execute(named_params! {
            ":id": &id,
            ":next_recording_id": &state.next_recording_id,
        })?;
    }
    Ok(())
}
