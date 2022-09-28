// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

/// Upgrades a version 2 schema to a version 3 schema.
/// Note that a version 2 schema is never actually used; so we know the upgrade from version 1 was
/// completed, and possibly an upgrade from 2 to 3 is half-finished.
use crate::db::{self, SqlUuid};
use crate::dir;
use crate::schema;
use failure::Error;
use rusqlite::params;
use std::convert::TryFrom;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;

/// Opens the sample file dir.
///
/// Makes a couple simplifying assumptions valid for version 2:
/// *   there's only one dir.
/// *   it has a last completed open.
fn open_sample_file_dir(tx: &rusqlite::Transaction) -> Result<Arc<dir::SampleFileDir>, Error> {
    let (p, s_uuid, o_id, o_uuid, db_uuid): (String, SqlUuid, i32, SqlUuid, SqlUuid) = tx
        .query_row(
            r#"
            select
              s.path, s.uuid, s.last_complete_open_id, o.uuid, m.uuid
            from
              sample_file_dir s
              join open o on (s.last_complete_open_id = o.id)
              cross join meta m
            "#,
            params![],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
    let mut meta = schema::DirMeta::default();
    meta.db_uuid.extend_from_slice(&db_uuid.0.as_bytes()[..]);
    meta.dir_uuid.extend_from_slice(&s_uuid.0.as_bytes()[..]);
    {
        let open = meta.last_complete_open.mut_or_insert_default();
        open.id = o_id as u32;
        open.uuid.extend_from_slice(&o_uuid.0.as_bytes()[..]);
    }
    let p = PathBuf::try_from(p)?;
    dir::SampleFileDir::open(&p, &meta)
}

pub fn run(_args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    let d = open_sample_file_dir(tx)?;
    let mut stmt = tx.prepare(
        r#"
        select
          composite_id,
          sample_file_uuid
        from
          recording_playback
        "#,
    )?;
    let mut rows = stmt.query(params![])?;
    while let Some(row) = rows.next()? {
        let id = db::CompositeId(row.get(0)?);
        let sample_file_uuid: SqlUuid = row.get(1)?;
        let from_path = super::UuidPath::from(sample_file_uuid.0);
        let to_path = crate::dir::CompositeIdPath::from(id);
        if let Err(e) = nix::fcntl::renameat(
            Some(d.fd.as_raw_fd()),
            &from_path,
            Some(d.fd.as_raw_fd()),
            &to_path,
        ) {
            if e == nix::Error::ENOENT {
                continue; // assume it was already moved.
            }
            return Err(e.into());
        }
    }

    // These create statements match the schema.sql when version 3 was the latest.
    tx.execute_batch(
        r#"
        alter table recording_playback rename to old_recording_playback;
        create table recording_playback (
          composite_id integer primary key references recording (composite_id),
          video_index blob not null check (length(video_index) > 0)
        );
        insert into recording_playback
        select
          composite_id,
          video_index
        from
          old_recording_playback;
        drop table old_recording_playback;
        drop table old_recording;
        drop table old_camera;
        drop table old_video_sample_entry;
        "#,
    )?;
    Ok(())
}
