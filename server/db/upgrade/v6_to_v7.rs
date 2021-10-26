// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

/// Upgrades a version 6 schema to a version 7 schema.
use failure::{format_err, Error};
use fnv::FnvHashMap;
use rusqlite::{named_params, params};
use std::convert::TryFrom;
use url::Url;
use uuid::Uuid;

use crate::{
    json::{CameraConfig, GlobalConfig, SignalConfig, SignalTypeConfig},
    SqlUuid,
};

fn copy_meta(tx: &rusqlite::Transaction) -> Result<(), Error> {
    let mut stmt = tx.prepare("select uuid, max_signal_changes from old_meta")?;
    let mut insert = tx.prepare("insert into meta (uuid, config) values (:uuid, :config)")?;
    let mut rows = stmt.query(params![])?;
    while let Some(row) = rows.next()? {
        let uuid: SqlUuid = row.get(0)?;
        let max_signal_changes: Option<i64> = row.get(1)?;
        let config = GlobalConfig {
            max_signal_changes: max_signal_changes
                .map(|s| {
                    u32::try_from(s).map_err(|_| format_err!("max_signal_changes out of range"))
                })
                .transpose()?,
            ..Default::default()
        };
        insert.execute(named_params! {
            ":uuid": uuid,
            ":config": &config,
        })?;
    }

    Ok(())
}

fn copy_signal_types(tx: &rusqlite::Transaction) -> Result<(), Error> {
    let mut types_ = FnvHashMap::default();
    let mut stmt = tx.prepare("select type_uuid, value, name from signal_type_enum")?;
    let mut rows = stmt.query(params![])?;
    while let Some(row) = rows.next()? {
        let type_uuid: SqlUuid = row.get(0)?;
        let value: i32 = row.get(1)?;
        let name: Option<String> = row.get(2)?;
        let type_ = types_
            .entry(type_uuid.0)
            .or_insert_with(SignalTypeConfig::default);
        let value = u8::try_from(value).map_err(|_| format_err!("bad signal type value"))?;
        let value_config = type_.values.entry(value).or_insert_with(Default::default);
        if let Some(n) = name {
            value_config.name = n;
        }
    }
    let mut insert = tx.prepare("insert into signal_type (uuid, config) values (?, ?)")?;
    for (&uuid, config) in &types_ {
        insert.execute(params![SqlUuid(uuid), config])?;
    }
    Ok(())
}

struct Signal {
    uuid: Uuid,
    type_uuid: Uuid,
    config: SignalConfig,
}

fn copy_signals(tx: &rusqlite::Transaction) -> Result<(), Error> {
    let mut signals = FnvHashMap::default();

    // Read from signal table.
    {
        let mut stmt =
            tx.prepare("select id, source_uuid, type_uuid, short_name from old_signal")?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let id: i32 = row.get(0)?;
            let id = u32::try_from(id)?;
            let source_uuid: SqlUuid = row.get(1)?;
            let type_uuid: SqlUuid = row.get(2)?;
            let short_name: String = row.get(3)?;
            signals.insert(
                id,
                Signal {
                    uuid: source_uuid.0,
                    type_uuid: type_uuid.0,
                    config: SignalConfig {
                        short_name,
                        ..Default::default()
                    },
                },
            );
        }
    }

    // Read from the signal_camera table.
    {
        let mut stmt = tx.prepare("select signal_id, camera_id, type from signal_camera")?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let signal_id: i32 = row.get(0)?;
            let signal_id = u32::try_from(signal_id)?;
            let camera_id: i32 = row.get(1)?;
            let type_: i32 = row.get(2)?;
            let signal = signals.get_mut(&signal_id).unwrap();
            signal.config.camera_associations.insert(
                camera_id,
                match type_ {
                    0 => "direct",
                    _ => "indirect",
                }
                .to_owned(),
            );
        }
    }

    let mut insert = tx.prepare(
        r#"
        insert into signal (id,  uuid,  type_uuid,  config)
                    values (:id, :uuid, :type_uuid, :config)
        "#,
    )?;
    for (&id, signal) in &signals {
        insert.execute(named_params! {
            ":id": id,
            ":uuid": SqlUuid(signal.uuid),
            ":type_uuid": SqlUuid(signal.type_uuid),
            ":config": &signal.config,
        })?;
    }

    Ok(())
}

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
        let uuid: SqlUuid = row.get(1)?;
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
        alter table open add boot_uuid check (length(boot_uuid) = 16);
        alter table user add preferences text;
        alter table camera rename to old_camera;
        alter table stream rename to old_stream;
        alter table signal rename to old_signal;
        alter table meta rename to old_meta;

        create table meta (
          uuid blob not null check (length(uuid) = 16),
          config text
        );

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

        create table signal (
          id integer primary key,
          uuid blob unique not null check (length(uuid) = 16),
          type_uuid blob not null references signal_type (uuid)
              check (length(type_uuid) = 16),
          config text
        );

        create table signal_type (
          uuid blob primary key check (length(uuid) = 16),
          config text
        ) without rowid;
    "#,
    )?;
    copy_meta(tx)?;
    copy_cameras(tx)?;
    copy_signal_types(tx)?;
    copy_signals(tx)?;
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
          end_reason text
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
        insert into recording select *, null from old_recording;
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
        drop table old_meta;
        drop table old_signal;
        drop table signal_type_enum;
        drop table signal_camera;
    "#,
    )?;
    Ok(())
}
