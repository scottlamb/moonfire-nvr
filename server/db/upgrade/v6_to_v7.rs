// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

/// Upgrades a version 6 schema to a version 7 schema.
use failure::Error;
use rusqlite::{named_params, params};
use url::Url;

use crate::{json::CameraConfig, FromSqlUuid};

fn copy_cameras(tx: &rusqlite::Transaction) -> Result<(), Error> {
    let mut insert = tx.prepare(
        r#"
        insert into camera (id,  short_name,  uuid,  config)
                    values (:id, :short_name, :uuid, :config)
        "#,
    )?;

    let mut stmt = tx.prepare(
        r#"
        select
          id,
          uuid,
          short_name,
          description,
          onvif_host,
          username,
          password
        from
          old_camera
        "#,
    )?;
    let mut rows = stmt.query(params![])?;
    while let Some(row) = rows.next()? {
        let id: i32 = row.get(0)?;
        let uuid: FromSqlUuid = row.get(1)?;
        let uuid_bytes = &uuid.0.as_bytes()[..];
        let short_name: String = row.get(2)?;
        let mut description: Option<String> = row.get(3)?;
        let onvif_host: Option<String> = row.get(4)?;
        let mut username: Option<String> = row.get(5)?;
        let mut password: Option<String> = row.get(6)?;
        let config = CameraConfig {
            description: description.take().unwrap_or_default(),
            onvif_base_url: onvif_host
                .map(|h| Url::parse(&format!("rtsp://{}/", h)))
                .transpose()?,
            username: username.take().unwrap_or_default(),
            password: password.take().unwrap_or_default(),
            ..Default::default()
        };
        insert.execute(named_params! {
            ":id": id,
            ":uuid": uuid_bytes,
            ":short_name": short_name,
            ":config": config,
        })?;
    }
    Ok(())
}

fn copy_streams(tx: &rusqlite::Transaction) -> Result<(), Error> {
    let mut insert = tx.prepare(
        r#"
        insert into stream (id,  camera_id,  sample_file_dir_id,  type,  config,  cum_recordings,
                            cum_media_duration_90k,  cum_runs)
                    values (:id, :camera_id, :sample_file_dir_id, :type, :config, :cum_recordings,
                            :cum_media_duration_90k, :cum_runs)
        "#,
    )?;

    let mut stmt = tx.prepare(
        r#"
        select
          id,
          camera_id,
          sample_file_dir_id,
          type,
          record,
          rtsp_url,
          retain_bytes,
          flush_if_sec,
          cum_recordings,
          cum_media_duration_90k,
          cum_runs
        from
          old_stream
        "#,
    )?;
    let mut rows = stmt.query(params![])?;
    while let Some(row) = rows.next()? {
        let id: i32 = row.get(0)?;
        let camera_id: i32 = row.get(1)?;
        let sample_file_dir_id: i32 = row.get(2)?;
        let type_: String = row.get(3)?;
        let record: bool = row.get(4)?;
        let rtsp_url: String = row.get(5)?;
        let retain_bytes: i64 = row.get(6)?;
        let flush_if_sec: u32 = row.get(7)?;
        let cum_recordings: i64 = row.get(8)?;
        let cum_media_duration_90k: i64 = row.get(9)?;
        let cum_runs: i64 = row.get(10)?;
        let config = crate::json::StreamConfig {
            mode: (if record {
                ""
            } else {
                crate::json::STREAM_MODE_RECORD
            })
            .to_owned(),
            url: Some(Url::parse(&rtsp_url)?),
            retain_bytes,
            flush_if_sec,
            ..Default::default()
        };
        insert.execute(named_params! {
            ":id": id,
            ":camera_id": camera_id,
            ":sample_file_dir_id": sample_file_dir_id,
            ":type": type_,
            ":config": config,
            ":cum_recordings": cum_recordings,
            ":cum_media_duration_90k": cum_media_duration_90k,
            ":cum_runs": cum_runs,
        })?;
    }
    Ok(())
}

pub fn run(_args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    tx.execute_batch(
        r#"
    
        alter table user add preferences text;
        alter table camera rename to old_camera;
        alter table stream rename to old_stream;

        create table camera (
          id integer primary key,
          uuid blob unique not null check (length(uuid) = 16),
          short_name text not null,
          config text not null
        );

        create table stream (
          id integer primary key,
          camera_id integer not null references camera (id),
          sample_file_dir_id integer references sample_file_dir (id),
          type text not null check (type in ('main', 'sub', 'ext')),
          config text not null,
          cum_recordings integer not null check (cum_recordings >= 0),
          cum_media_duration_90k integer not null check (cum_media_duration_90k >= 0),
          cum_runs integer not null check (cum_runs >= 0),
          unique (camera_id, type)
        );
    "#,
    )?;
    copy_cameras(tx)?;
    copy_streams(tx)?;
    tx.execute_batch(
        r#"
        drop index recording_cover;

        alter table recording rename to old_recording;
        create table recording (
          composite_id integer primary key,
          open_id integer not null,
          stream_id integer not null references stream (id),
          run_offset integer not null,
          flags integer not null,
          sample_file_bytes integer not null check (sample_file_bytes > 0),
          start_time_90k integer not null check (start_time_90k > 0),
          prev_media_duration_90k integer not null check (prev_media_duration_90k >= 0),
          prev_runs integer not null check (prev_runs >= 0),
          wall_duration_90k integer not null
              check (wall_duration_90k >= 0 and wall_duration_90k < 5*60*90000),
          media_duration_delta_90k integer not null,
          video_samples integer not null check (video_samples > 0),
          video_sync_samples integer not null check (video_sync_samples > 0),
          video_sample_entry_id integer references video_sample_entry (id),
          check (composite_id >> 32 = stream_id)
        );
        create index recording_cover on recording (
          stream_id,
          start_time_90k,
          open_id,
          wall_duration_90k,
          media_duration_delta_90k,
          video_samples,
          video_sync_samples,
          video_sample_entry_id,
          sample_file_bytes,
          run_offset,
          flags
        );
        insert into recording select * from old_recording;
        alter table recording_integrity rename to old_recording_integrity;
        create table recording_integrity (
          composite_id integer primary key references recording (composite_id),
          local_time_delta_90k integer,
          local_time_since_open_90k integer,
          wall_time_delta_90k integer,
          sample_file_blake3 blob check (length(sample_file_blake3) <= 32)
        );
        insert into recording_integrity select * from old_recording_integrity;
        alter table recording_playback rename to old_recording_playback;
        create table recording_playback (
          composite_id integer primary key references recording (composite_id),
          video_index blob not null check (length(video_index) > 0)
        );
        insert into recording_playback select * from old_recording_playback;

        alter table signal_camera rename to old_signal_camera;
        create table signal_camera (
          signal_id integer references signal (id),
          camera_id integer references camera (id),
          type integer not null,
          primary key (signal_id, camera_id)
        ) without rowid;
        drop table old_signal_camera;
        drop table old_recording_playback;
        drop table old_recording_integrity;
        drop table old_recording;
        drop table old_stream; 
        drop table old_camera;
    "#,
    )?;
    Ok(())
}
