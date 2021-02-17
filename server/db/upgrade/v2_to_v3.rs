// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors
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

/// Upgrades a version 2 schema to a version 3 schema.
/// Note that a version 2 schema is never actually used; so we know the upgrade from version 1 was
/// completed, and possibly an upgrade from 2 to 3 is half-finished.
use crate::db::{self, FromSqlUuid};
use crate::dir;
use crate::schema;
use failure::Error;
use rusqlite::params;
use std::os::unix::io::AsRawFd;
use std::sync::Arc;

/// Opens the sample file dir.
///
/// Makes a couple simplifying assumptions valid for version 2:
/// *   there's only one dir.
/// *   it has a last completed open.
fn open_sample_file_dir(tx: &rusqlite::Transaction) -> Result<Arc<dir::SampleFileDir>, Error> {
    let (p, s_uuid, o_id, o_uuid, db_uuid): (String, FromSqlUuid, i32, FromSqlUuid, FromSqlUuid) =
        tx.query_row(
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
        let open = meta.last_complete_open.set_default();
        open.id = o_id as u32;
        open.uuid.extend_from_slice(&o_uuid.0.as_bytes()[..]);
    }
    dir::SampleFileDir::open(&p, &meta)
}

pub fn run(_args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    let d = open_sample_file_dir(&tx)?;
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
        let sample_file_uuid: FromSqlUuid = row.get(1)?;
        let from_path = super::UuidPath::from(sample_file_uuid.0);
        let to_path = crate::dir::CompositeIdPath::from(id);
        if let Err(e) = nix::fcntl::renameat(
            Some(d.fd.as_raw_fd()),
            &from_path,
            Some(d.fd.as_raw_fd()),
            &to_path,
        ) {
            if e == nix::Error::Sys(nix::errno::Errno::ENOENT) {
                continue; // assume it was already moved.
            }
            Err(e)?;
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
