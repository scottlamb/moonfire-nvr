// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

/// Upgrades a version 1 schema to a version 2 schema.
use crate::dir;
use crate::schema::DirMeta;
use base::{bail, Error};
use nix::fcntl::{FlockArg, OFlag};
use nix::sys::stat::Mode;
use rusqlite::{named_params, params};
use std::os::unix::io::AsRawFd;
use uuid::Uuid;

pub fn run(args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    let Some(sample_file_path) = args.sample_file_dir else {
        bail!(
            InvalidArgument,
            msg("--sample-file-dir required when upgrading from schema version 1 to 2."),
        );
    };

    let mut d = nix::dir::Dir::open(
        sample_file_path,
        OFlag::O_DIRECTORY | OFlag::O_RDONLY,
        Mode::empty(),
    )?;
    nix::fcntl::flock(d.as_raw_fd(), FlockArg::LockExclusiveNonblock)?;
    verify_dir_contents(sample_file_path, &mut d, tx)?;

    // These create statements match the schema.sql when version 2 was the latest.
    tx.execute_batch(
        r#"
        create table meta (
          uuid blob not null check (length(uuid) = 16)
        );
        create table open (
          id integer primary key,
          uuid blob unique not null check (length(uuid) = 16),
          start_time_90k integer,
          end_time_90k integer,
          duration_90k integer
        );
        create table sample_file_dir (
          id integer primary key,
          path text unique not null,
          uuid blob unique not null check (length(uuid) = 16),
          last_complete_open_id integer references open (id)
        );
        create table user (
          id integer primary key,
          username unique not null,
          flags integer not null,
          password_hash text,
          password_id integer not null default 0,
          password_failure_count integer not null default 0,
          unix_uid integer
        );
        create table user_session (
          session_id_hash blob primary key not null,
          user_id integer references user (id) not null,
          seed blob not null,
          flags integer not null,
          domain text,
          description text,
          creation_password_id integer,
          creation_time_sec integer not null,
          creation_user_agent text,
          creation_peer_addr blob,
          revocation_time_sec integer,
          revocation_user_agent text,
          revocation_peer_addr blob,
          revocation_reason integer,
          revocation_reason_detail text,
          last_use_time_sec integer,
          last_use_user_agent text,
          last_use_peer_addr blob,
          use_count not null default 0
        ) without rowid;
        create index user_session_uid on user_session (user_id);
        "#,
    )?;
    let db_uuid = ::uuid::Uuid::new_v4();
    let db_uuid_bytes = &db_uuid.as_bytes()[..];
    tx.execute("insert into meta (uuid) values (?)", params![db_uuid_bytes])?;
    let open_uuid = ::uuid::Uuid::new_v4();
    let open_uuid_bytes = &open_uuid.as_bytes()[..];
    tx.execute(
        "insert into open (uuid) values (?)",
        params![open_uuid_bytes],
    )?;
    let open_id = tx.last_insert_rowid() as u32;
    let dir_uuid = ::uuid::Uuid::new_v4();
    let dir_uuid_bytes = &dir_uuid.as_bytes()[..];

    // Write matching metadata to the directory.
    let mut meta = DirMeta::default();
    {
        meta.db_uuid.extend_from_slice(db_uuid_bytes);
        meta.dir_uuid.extend_from_slice(dir_uuid_bytes);
        let open = meta.last_complete_open.mut_or_insert_default();
        open.id = open_id;
        open.uuid.extend_from_slice(open_uuid_bytes);
    }
    dir::write_meta(d.as_raw_fd(), &meta)?;

    let Some(sample_file_path) = sample_file_path.to_str() else {
        bail!(
            InvalidArgument,
            msg(
                "sample file dir {} is not a valid string",
                sample_file_path.display()
            ),
        );
    };
    tx.execute(
        r#"
        insert into sample_file_dir (path,  uuid, last_complete_open_id)
                             values (?,     ?,    ?)
        "#,
        params![sample_file_path, dir_uuid_bytes, open_id],
    )?;

    tx.execute_batch(
        r#"
        drop table reserved_sample_files;
        alter table camera rename to old_camera;
        alter table recording rename to old_recording;
        alter table video_sample_entry rename to old_video_sample_entry;
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
          sample_file_dir_id integer references sample_file_dir (id),
          type text not null check (type in ('main', 'sub')),
          record integer not null check (record in (1, 0)),
          rtsp_path text not null,
          retain_bytes integer not null check (retain_bytes >= 0),
          flush_if_sec integer not null,
          next_recording_id integer not null check (next_recording_id >= 0),
          unique (camera_id, type)
        );

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

        create table recording_integrity (
          composite_id integer primary key references recording (composite_id),
          local_time_delta_90k integer,
          local_time_since_open_90k integer,
          wall_time_delta_90k integer,
          sample_file_sha1 blob check (length(sample_file_sha1) <= 20)
        );

        create table video_sample_entry (
          id integer primary key,
          sha1 blob unique not null check (length(sha1) = 20),
          width integer not null check (width > 0),
          height integer not null check (height > 0),
          rfc6381_codec text not null,
          data blob not null check (length(data) > 86)
        );

        create table garbage (
          sample_file_dir_id integer references sample_file_dir (id),
          composite_id integer,
          primary key (sample_file_dir_id, composite_id)
        ) without rowid;

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
          old_camera.id,
          old_camera.id,
          sample_file_dir.id,
          'main',
          1,
          old_camera.main_rtsp_path,
          old_camera.retain_bytes,
          0,
          old_camera.next_recording_id
        from
          old_camera cross join sample_file_dir;

        -- Insert sub stream (if path is non-empty) using any id.
        insert into stream (camera_id, sample_file_dir_id, type, record, rtsp_path,
                            retain_bytes, flush_if_sec, next_recording_id)
        select
          old_camera.id,
          sample_file_dir.id,
          'sub',
          0,
          old_camera.sub_rtsp_path,
          0,
          90,
          1
        from
          old_camera cross join sample_file_dir
        where
          old_camera.sub_rtsp_path != '';
        "#,
    )?;

    // Add the new video_sample_entry rows, before inserting the recordings referencing them.
    fix_video_sample_entry(tx)?;

    tx.execute_batch(
        r#"
        insert into recording
        select
          r.composite_id,
          r.camera_id,
          o.id,
          r.run_offset,
          r.flags,
          r.sample_file_bytes,
          r.start_time_90k,
          r.duration_90k,
          r.video_samples,
          r.video_sync_samples,
          r.video_sample_entry_id
        from
          old_recording r cross join open o;

        insert into recording_integrity (composite_id, local_time_delta_90k, sample_file_sha1)
        select
          r.composite_id,
          case when r.run_offset > 0 then local_time_delta_90k else null end,
          p.sample_file_sha1
        from
          old_recording r join recording_playback p on (r.composite_id = p.composite_id);
        "#,
    )?;

    Ok(())
}

/// Ensures the sample file directory has the expected contents.
/// Among other problems, this catches a fat-fingered `--sample-file-dir`.
/// The expected contents are:
///
/// *   required: recording uuids.
/// *   optional: reserved sample file uuids.
/// *   optional: meta and meta-tmp from half-completed update attempts.
/// *   forbidden: anything else.
fn verify_dir_contents(
    sample_file_path: &std::path::Path,
    dir: &mut nix::dir::Dir,
    tx: &rusqlite::Transaction,
) -> Result<(), Error> {
    // Build a hash of the uuids found in the directory.
    let n: i64 = tx.query_row(
        r#"
        select
          a.c + b.c
        from
          (select count(*) as c from recording) a,
          (select count(*) as c from reserved_sample_files) b;
        "#,
        params![],
        |r| r.get(0),
    )?;
    let mut files = ::fnv::FnvHashSet::with_capacity_and_hasher(n as usize, Default::default());
    for e in dir.iter() {
        let e = e?;
        let f = e.file_name();
        match f.to_bytes() {
            b"." | b".." => continue,
            b"meta" | b"meta-tmp" => {
                // Ignore metadata files. These might from a half-finished update attempt.
                // If the directory is actually an in-use >v3 format, other contents won't match.
                continue;
            }
            _ => {}
        };
        let s = match f.to_str() {
            Ok(s) => s,
            Err(_) => bail!(
                FailedPrecondition,
                msg("unexpected file {f:?} in {sample_file_path:?}")
            ),
        };
        let uuid = match Uuid::parse_str(s) {
            Ok(u) => u,
            Err(_) => bail!(
                FailedPrecondition,
                msg("unexpected file {f:?} in {sample_file_path:?}")
            ),
        };
        if s != uuid.as_hyphenated().to_string() {
            // non-canonical form.
            bail!(
                FailedPrecondition,
                msg("unexpected file {f:?} in {sample_file_path:?}")
            );
        }
        files.insert(uuid);
    }

    // Iterate through the database and check that everything has a matching file.
    {
        let mut stmt = tx.prepare(r"select sample_file_uuid from recording_playback")?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let uuid: crate::db::SqlUuid = row.get(0)?;
            if !files.remove(&uuid.0) {
                bail!(
                    FailedPrecondition,
                    msg(
                        "{} is missing from dir {}!",
                        uuid.0,
                        sample_file_path.display()
                    ),
                );
            }
        }
    }

    let mut stmt = tx.prepare(r"select uuid from reserved_sample_files")?;
    let mut rows = stmt.query(params![])?;
    while let Some(row) = rows.next()? {
        let uuid: crate::db::SqlUuid = row.get(0)?;
        if files.remove(&uuid.0) {
            // Also remove the garbage file. For historical reasons (version 2 was originally
            // defined as not having a garbage table so still is), do this here rather than with
            // the other path manipulations in v2_to_v3.rs. There's no harm anyway in deleting
            // a garbage file so if the upgrade transation fails this is still a valid and complete
            // version 1 database.
            let p = super::UuidPath::from(uuid.0);
            nix::unistd::unlinkat(
                Some(dir.as_raw_fd()),
                &p,
                nix::unistd::UnlinkatFlags::NoRemoveDir,
            )?;
        }
    }

    if !files.is_empty() {
        bail!(
            FailedPrecondition,
            msg(
                "{} unexpected sample file uuids in dir {}: {:?}!",
                files.len(),
                sample_file_path.display(),
                files,
            ),
        );
    }
    Ok(())
}

fn fix_video_sample_entry(tx: &rusqlite::Transaction) -> Result<(), Error> {
    let mut select = tx.prepare(
        r#"
        select
          id,
          sha1,
          width,
          height,
          data
        from
          old_video_sample_entry
        "#,
    )?;
    let mut insert = tx.prepare(
        r#"
        insert into video_sample_entry values (:id, :sha1, :width, :height, :rfc6381_codec, :data)
        "#,
    )?;
    let mut rows = select.query(params![])?;
    while let Some(row) = rows.next()? {
        let data: Vec<u8> = row.get(4)?;
        insert.execute(named_params! {
            ":id": &row.get::<_, i32>(0)?,
            ":sha1": &row.get::<_, Vec<u8>>(1)?,
            ":width": &row.get::<_, i32>(2)?,
            ":height": &row.get::<_, i32>(3)?,
            ":rfc6381_codec": &rfc6381_codec_from_sample_entry(&data)?,
            ":data": &data,
        })?;
    }
    Ok(())
}

// This previously lived in h264.rs. As of version 1, H.264 is the only supported codec.
fn rfc6381_codec_from_sample_entry(sample_entry: &[u8]) -> Result<String, Error> {
    if sample_entry.len() < 99 || &sample_entry[4..8] != b"avc1" || &sample_entry[90..94] != b"avcC"
    {
        bail!(InvalidArgument, msg("not a valid AVCSampleEntry"));
    }
    let profile_idc = sample_entry[103];
    let constraint_flags_byte = sample_entry[104];
    let level_idc = sample_entry[105];
    Ok(format!(
        "avc1.{profile_idc:02x}{constraint_flags_byte:02x}{level_idc:02x}"
    ))
}
