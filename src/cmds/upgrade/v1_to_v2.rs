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

/// Upgrades a version 1 schema to a version 2 schema.

use error::Error;
use rusqlite;

pub fn run(tx: &rusqlite::Transaction) -> Result<(), Error> {
    // These create statements match the schema.sql when version 2 was the latest.
    tx.execute_batch(r#"
        alter table camera rename to old_camera;
        alter table recording rename to old_recording;
        drop index recording_cover;

        create table camera (
          id integer primary key,
          uuid blob unique not null check (length(uuid) = 16),
          short_name text not null,
          description text,
          host text,
          username text,
          password text
        );

        create table stream (
          id integer primary key,
          camera_id integer not null references camera (id),
          type text not null check (type in ('main', 'sub')),
          record integer not null check (record in (1, 0)),
          rtsp_path text not null,
          retain_bytes integer not null check (retain_bytes >= 0),
          next_recording_id integer not null check (next_recording_id >= 0),
          unique (camera_id, type)
        );

        create table recording (
          composite_id integer primary key,
          stream_id integer not null references stream (id),
          run_offset integer not null,
          flags integer not null,
          sample_file_bytes integer not null check (sample_file_bytes > 0),
          start_time_90k integer not null check (start_time_90k > 0),
          duration_90k integer not null
              check (duration_90k >= 0 and duration_90k < 5*60*90000),
          local_time_delta_90k integer not null,
          video_samples integer not null check (video_samples > 0),
          video_sync_samples integer not null check (video_sync_samples > 0),
          video_sample_entry_id integer references video_sample_entry (id),
          check (composite_id >> 32 = stream_id)
        );

        create index recording_cover on recording (
          stream_id,
          start_time_90k,
          duration_90k,
          video_samples,
          video_sync_samples,
          video_sample_entry_id,
          sample_file_bytes,
          run_offset,
          flags
        );

        insert into camera
        select
          id,
          uuid,
          short_name,
          description,
          host,
          username,
          password
        from old_camera;

        -- Insert main streams using the same id as the camera, to ease changing recordings.
        insert into stream
        select
          id,
          id,
          'main',
          1,
          main_rtsp_path,
          retain_bytes,
          next_recording_id
        from
          old_camera;

        -- Insert sub stream (if path is non-empty) using any id.
        insert into stream (camera_id, type, record, rtsp_path, retain_bytes, next_recording_id)
        select
          id,
          'sub',
          0,
          sub_rtsp_path,
          0,
          0
        from
          old_camera
        where
          sub_rtsp_path != '';

        insert into recording
        select
          composite_id,
          camera_id,
          run_offset,
          flags,
          sample_file_bytes,
          start_time_90k,
          duration_90k,
          local_time_delta_90k,
          video_samples,
          video_sync_samples,
          video_sample_entry_id
        from
          old_recording;

        drop table old_camera;
        drop table old_recording;
    "#)?;
    Ok(())
}
