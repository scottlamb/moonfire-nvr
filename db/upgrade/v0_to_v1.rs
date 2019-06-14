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

/// Upgrades a version 0 schema to a version 1 schema.

use crate::db;
use crate::recording;
use failure::Error;
use log::warn;
use rusqlite::types::ToSql;
use std::collections::HashMap;

pub fn run(_args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    // These create statements match the schema.sql when version 1 was the latest.
    tx.execute_batch(r#"
        alter table camera rename to old_camera;
        create table camera (
          id integer primary key,
          uuid blob unique,
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
    "#)?;
    let camera_state = fill_recording(tx).unwrap();
    fill_camera(tx, camera_state).unwrap();
    tx.execute_batch(r#"
      drop table old_camera;
      drop table old_recording;
    "#)?;
    Ok(())
}

struct CameraState {
    /// tuple of (run_start_id, next_start_90k).
    current_run: Option<(i64, i64)>,

    /// As in the `next_recording_id` field of the `camera` table.
    next_recording_id: i32,
}

fn has_trailing_zero(video_index: &[u8]) -> Result<bool, Error> {
    let mut it = recording::SampleIndexIterator::new();
    while it.next(video_index)? {}
    Ok(it.duration_90k == 0)
}

/// Fills the `recording` and `recording_playback` tables from `old_recording`, returning
/// the `camera_state` map for use by a following call to `fill_cameras`.
fn fill_recording(tx: &rusqlite::Transaction) -> Result<HashMap<i32, CameraState>, Error> {
    let mut select = tx.prepare(r#"
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
    "#)?;
    let mut insert1 = tx.prepare(r#"
      insert into recording values (:composite_id, :camera_id, :run_offset, :flags,
                                    :sample_file_bytes, :start_time_90k, :duration_90k,
                                    :local_time_delta_90k, :video_samples, :video_sync_samples,
                                    :video_sample_entry_id)
    "#)?;
    let mut insert2 = tx.prepare(r#"
      insert into recording_playback values (:composite_id, :sample_file_uuid, :sample_file_sha1,
                                             :video_index)
    "#)?;
    let mut rows = select.query(&[] as &[&dyn ToSql])?;
    let mut camera_state: HashMap<i32, CameraState> = HashMap::new();
    while let Some(row) = rows.next()? {
        let camera_id: i32 = row.get(0)?;
        let camera_state = camera_state.entry(camera_id).or_insert_with(|| {
            CameraState{
                current_run: None,
                next_recording_id: 1,
            }
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
        let sample_file_uuid: db::FromSqlUuid = row.get(8)?;
        let sample_file_sha1: Vec<u8> = row.get(9)?;
        let video_index: Vec<u8> = row.get(10)?;
        let old_id: i32 = row.get(11)?;
        let trailing_zero = has_trailing_zero(&video_index).unwrap_or_else(|e| {
            warn!("recording {}/{} (sample file {}, formerly recording {}) has corrupt \
                  video_index: {}",
                  camera_id, composite_id & 0xFFFF, sample_file_uuid.0, old_id, e);
            false
        });
        let run_id = match camera_state.current_run {
            Some((run_id, expected_start)) if expected_start == start_time_90k => run_id,
            _ => composite_id,
        };
        insert1.execute_named(&[
            (":composite_id", &composite_id),
            (":camera_id", &camera_id),
            (":run_offset", &(composite_id - run_id)),
            (":flags", &(if trailing_zero { db::RecordingFlags::TrailingZero as i32 } else { 0 })),
            (":sample_file_bytes", &sample_file_bytes),
            (":start_time_90k", &start_time_90k),
            (":duration_90k", &duration_90k),
            (":local_time_delta_90k", &local_time_delta_90k),
            (":video_samples", &video_samples),
            (":video_sync_samples", &video_sync_samples),
            (":video_sample_entry_id", &video_sample_entry_id),
        ])?;
        insert2.execute_named(&[
            (":composite_id", &composite_id),
            (":sample_file_uuid", &&sample_file_uuid.0.as_bytes()[..]),
            (":sample_file_sha1", &sample_file_sha1),
            (":video_index", &video_index),
        ])?;
        camera_state.current_run = if trailing_zero {
            None
        } else {
            Some((run_id, start_time_90k + duration_90k as i64))
        };
    }
    Ok(camera_state)
}

fn fill_camera(tx: &rusqlite::Transaction, camera_state: HashMap<i32, CameraState>)
                -> Result<(), Error> {
    let mut select = tx.prepare(r#"
      select
        id, uuid, short_name, description, host, username, password, main_rtsp_path,
        sub_rtsp_path, retain_bytes
      from
        old_camera
    "#)?;
    let mut insert = tx.prepare(r#"
      insert into camera values (:id, :uuid, :short_name, :description, :host, :username, :password,
      :main_rtsp_path, :sub_rtsp_path, :retain_bytes, :next_recording_id)
    "#)?;
    let mut rows = select.query(&[] as &[&dyn ToSql])?;
    while let Some(row) = rows.next()? {
        let id: i32 = row.get(0)?;
        let uuid: Vec<u8> = row.get(1)?;
        let short_name: String = row.get(2)?;
        let description: String = row.get(3)?;
        let host: String = row.get(4)?;
        let username: String = row.get(5)?;
        let password: String = row.get(6)?;
        let main_rtsp_path: String = row.get(7)?;
        let sub_rtsp_path: String = row.get(8)?;
        let retain_bytes: i64 = row.get(9)?;
        insert.execute_named(&[
            (":id", &id),
            (":uuid", &uuid),
            (":short_name", &short_name),
            (":description", &description),
            (":host", &host),
            (":username", &username),
            (":password", &password),
            (":main_rtsp_path", &main_rtsp_path),
            (":sub_rtsp_path", &sub_rtsp_path),
            (":retain_bytes", &retain_bytes),
            (":next_recording_id",
             &camera_state.get(&id).map(|s| s.next_recording_id).unwrap_or(1)),
        ])?;
    }
    Ok(())
}
