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

/// Upgrades a version 1 schema to a version 2 schema.

use dir;
use failure::Error;
use libc;
use rusqlite;
use schema::DirMeta;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use uuid::Uuid;

pub fn run(args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    let sample_file_path =
        args.flag_sample_file_dir
            .ok_or_else(|| format_err!("--sample-file-dir required when upgrading from \
                                        schema version 1 to 2."))?;

    let d = dir::Fd::open(sample_file_path, false)?;
    d.lock(libc::LOCK_EX | libc::LOCK_NB)?;
    verify_dir_contents(sample_file_path, tx)?;

    // These create statements match the schema.sql when version 2 was the latest.
    tx.execute_batch(r#"
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
          password_failure_count integer not null,
          unix_uid integer
        );
        create table user_session (
          session_id_hash blob primary key not null,
          user_id integer references user (id),
          flags integer not null,
          domain text,
          description text,
          creation_password_id integer,
          creation_peer_addr blob,
          creation_time_sec integer not null,
          creation_user_agent text,
          revocation_time_sec integer,
          revocation_reason text,
          last_use_time_sec integer,
          last_use_user_agent text,
          last_use_peer_addr blob,
          use_count not null
        ) without rowid;
    "#)?;
    let db_uuid = ::uuid::Uuid::new_v4();
    let db_uuid_bytes = &db_uuid.as_bytes()[..];
    tx.execute("insert into meta (uuid) values (?)", &[&db_uuid_bytes])?;
    let open_uuid = ::uuid::Uuid::new_v4();
    let open_uuid_bytes = &open_uuid.as_bytes()[..];
    tx.execute("insert into open (uuid) values (?)", &[&open_uuid_bytes])?;
    let open_id = tx.last_insert_rowid() as u32;
    let dir_uuid = ::uuid::Uuid::new_v4();
    let dir_uuid_bytes = &dir_uuid.as_bytes()[..];

    // Write matching metadata to the directory.
    let mut meta = DirMeta::default();
    {
        meta.db_uuid.extend_from_slice(db_uuid_bytes);
        meta.dir_uuid.extend_from_slice(dir_uuid_bytes);
        let open = meta.mut_last_complete_open();
        open.id = open_id;
        open.uuid.extend_from_slice(&open_uuid_bytes);
    }
    dir::write_meta(&d, &meta)?;

    tx.execute(r#"
        insert into sample_file_dir (path,  uuid, last_complete_open_id)
                             values (?,     ?,    ?)
    "#, &[&sample_file_path, &dir_uuid_bytes, &open_id])?;

    tx.execute_batch(r#"
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
          stream_id integer not null references stream (id),
          open_id integer not null,
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
          r.local_time_delta_90k,
          r.video_samples,
          r.video_sync_samples,
          r.video_sample_entry_id
        from
          old_recording r cross join open o;

        insert into recording_integrity
        select
          r.composite_id,
          case when r.run_offset > 0 then local_time_delta_90k else null end,
          p.sample_file_sha1
        from
          old_recording r join recording_playback p on (r.composite_id = p.composite_id);
    "#)?;

    fix_video_sample_entry(tx)?;

    tx.execute_batch(r#"
        drop table old_camera;
        drop table old_recording;
        drop table old_video_sample_entry;
    "#)?;

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
fn verify_dir_contents(sample_file_path: &str, tx: &rusqlite::Transaction) -> Result<(), Error> {
    // Build a hash of the uuids found in the directory.
    let n: i64 = tx.query_row(r#"
        select
          a.c + b.c
        from
          (select count(*) as c from recording) a,
          (select count(*) as c from reserved_sample_files) b;
    "#, &[], |r| r.get_checked(0))??;
    let mut files = ::fnv::FnvHashSet::with_capacity_and_hasher(n as usize, Default::default());
    for e in fs::read_dir(sample_file_path)? {
        let e = e?;
        let f = e.file_name();
        match f.as_bytes() {
            b"." | b".." => continue,
            b"meta" | b"meta-tmp" => {
                // Ignore metadata files. These might from a half-finished update attempt.
                // If the directory is actually an in-use >v3 format, other contents won't match.
                continue;
            },
            _ => {},
        };
        let s = match f.to_str() {
            Some(s) => s,
            None => bail!("unexpected file {:?} in {:?}", f, sample_file_path),
        };
        let uuid = match Uuid::parse_str(s) {
            Ok(u) => u,
            Err(_) => bail!("unexpected file {:?} in {:?}", f, sample_file_path),
        };
        if s != uuid.hyphenated().to_string() {  // non-canonical form.
            bail!("unexpected file {:?} in {:?}", f, sample_file_path);
        }
        files.insert(uuid);
    }

    // Iterate through the database and check that everything has a matching file.
    {
        let mut stmt = tx.prepare(r"select sample_file_uuid from recording_playback")?;
        let mut rows = stmt.query(&[])?;
        while let Some(row) = rows.next() {
            let row = row?;
            let uuid: ::db::FromSqlUuid = row.get_checked(0)?;
            if !files.remove(&uuid.0) {
                bail!("{} is missing from dir {}!", uuid.0, sample_file_path);
            }
        }
    }

    let mut stmt = tx.prepare(r"select uuid from reserved_sample_files")?;
    let mut rows = stmt.query(&[])?;
    while let Some(row) = rows.next() {
        let row = row?;
        let uuid: ::db::FromSqlUuid = row.get_checked(0)?;
        files.remove(&uuid.0);
    }

    if !files.is_empty() {
        bail!("{} unexpected sample file uuids in dir {}: {:?}!",
              files.len(), sample_file_path, files);
    }
    Ok(())
}

fn fix_video_sample_entry(tx: &rusqlite::Transaction) -> Result<(), Error> {
    let mut select = tx.prepare(r#"
        select
          id,
          sha1,
          width,
          height,
          data
        from
          old_video_sample_entry
    "#)?;
    let mut insert = tx.prepare(r#"
        insert into video_sample_entry values (:id, :sha1, :width, :height, :rfc6381_codec, :data)
    "#)?;
    let mut rows = select.query(&[])?;
    while let Some(row) = rows.next() {
        let row = row?;
        let data: Vec<u8> = row.get_checked(4)?;
        insert.execute_named(&[
            (":id", &row.get_checked::<_, i32>(0)?),
            (":sha1", &row.get_checked::<_, Vec<u8>>(1)?),
            (":width", &row.get_checked::<_, i32>(2)?),
            (":height", &row.get_checked::<_, i32>(3)?),
            (":rfc6381_codec", &rfc6381_codec_from_sample_entry(&data)?),
            (":data", &data),
        ])?;
    }
    Ok(())
}

// This previously lived in h264.rs. As of version 1, H.264 is the only supported codec.
fn rfc6381_codec_from_sample_entry(sample_entry: &[u8]) -> Result<String, Error> {
    if sample_entry.len() < 99 || &sample_entry[4..8] != b"avc1" ||
       &sample_entry[90..94] != b"avcC" {
        bail!("not a valid AVCSampleEntry");
    }
    let profile_idc = sample_entry[103];
    let constraint_flags_byte = sample_entry[104];
    let level_idc = sample_entry[105];
    Ok(format!("avc1.{:02x}{:02x}{:02x}", profile_idc, constraint_flags_byte, level_idc))
}
