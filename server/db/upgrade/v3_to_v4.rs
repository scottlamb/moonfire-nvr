// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2019 The Moonfire NVR Authors
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

/// Upgrades a version 3 schema to a version 4 schema.

use failure::Error;

pub fn run(_args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    // These create statements match the schema.sql when version 4 was the latest.
    tx.execute_batch(r#"
        alter table meta add column max_signal_changes integer check (max_signal_changes >= 0);

        create table signal (
          id integer primary key,
          source_uuid blob not null check (length(source_uuid) = 16),
          type_uuid blob not null check (length(type_uuid) = 16),
          short_name not null,
          unique (source_uuid, type_uuid)
        );

        create table signal_type_enum (
          type_uuid blob not null check (length(type_uuid) = 16),
          value integer not null check (value > 0 and value < 16),
          name text not null,
          motion int not null check (motion in (0, 1)) default 0,
          color text
        );

        create table signal_camera (
          signal_id integer references signal (id),
          camera_id integer references camera (id),
          type integer not null,
          primary key (signal_id, camera_id)
        ) without rowid;

        create table signal_change (
          time_90k integer primary key,
          changes blob not null
        );

        alter table user add column permissions blob not null default X'';
        alter table user_session add column permissions blob not null default X'';

        -- Set permissions to "view_video" on existing users and sessions to preserve their
        -- behavior. Newly created users won't have prepopulated permissions like this.
        update user set permissions = X'0801';
        update user_session set permissions = X'0801';

        alter table camera rename to old_camera;
        create table camera (
          id integer primary key,
          uuid blob unique not null check (length(uuid) = 16),
          short_name text not null,
          description text,
          onvif_host text,
          username text,
          password text
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
        from
          old_camera;

        alter table stream rename to old_stream;
        create table stream (
          id integer primary key,
          camera_id integer not null references camera (id),
          sample_file_dir_id integer references sample_file_dir (id),
          type text not null check (type in ('main', 'sub')),
          record integer not null check (record in (1, 0)),
          rtsp_url text not null,
          retain_bytes integer not null check (retain_bytes >= 0),
          flush_if_sec integer not null,
          next_recording_id integer not null check (next_recording_id >= 0),
          unique (camera_id, type)
        );
        insert into stream
        select
          s.id,
          s.camera_id,
          s.sample_file_dir_id,
          s.type,
          s.record,
          'rtsp://' || c.onvif_host || s.rtsp_path as rtsp_url,
          retain_bytes,
          flush_if_sec,
          next_recording_id
        from
          old_stream s join camera c on (s.camera_id = c.id);

        alter table recording rename to old_recording;
        create table recording (
          composite_id integer primary key,
          open_id integer not null,
          stream_id integer not null references stream (id),
          run_offset integer not null,
          flags integer not null,
          sample_file_bytes integer not null check (sample_file_bytes > 0),
          start_time_90k integer not null check (start_time_90k > 0),
          duration_90k integer not null
              check (duration_90k >= 0 and duration_90k < 5*60*90000),
          video_samples integer not null check (video_samples > 0),
          video_sync_samples integer not null check (video_sync_samples > 0),
          video_sample_entry_id integer references video_sample_entry (id),
          check (composite_id >> 32 = stream_id)
        );
        insert into recording select
          composite_id,
          open_id,
          stream_id,
          run_offset,
          flags,
          sample_file_bytes,
          start_time_90k,
          duration_90k,
          video_samples,
          video_sync_samples,
          video_sample_entry_id
        from old_recording;
        drop index recording_cover;
        create index recording_cover on recording (
          stream_id,
          start_time_90k,
          open_id,
          duration_90k,
          video_samples,
          video_sync_samples,
          video_sample_entry_id,
          sample_file_bytes,
          run_offset,
          flags
        );

        alter table recording_integrity rename to old_recording_integrity;
        create table recording_integrity (
          composite_id integer primary key references recording (composite_id),
          local_time_delta_90k integer,
          local_time_since_open_90k integer,
          wall_time_delta_90k integer,
          sample_file_sha1 blob check (length(sample_file_sha1) <= 20)
        );
        insert into recording_integrity select * from old_recording_integrity;

        alter table recording_playback rename to old_recording_playback;
        create table recording_playback (
          composite_id integer primary key references recording (composite_id),
          video_index blob not null check (length(video_index) > 0)
        );
        insert into recording_playback select * from old_recording_playback;

        drop table old_recording_playback;
        drop table old_recording_integrity;
        drop table old_recording;
        drop table old_stream;
        drop table old_camera;

        -- This was supposed to be present in version 2, but the upgrade procedure used to miss it.
        -- Catch up so we know a version 4 database is right.
        create index if not exists user_session_uid on user_session (user_id);
    "#)?;
    Ok(())
}
