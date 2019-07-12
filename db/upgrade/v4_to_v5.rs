// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2019 Scott Lamb <slamb@slamb.org>
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

/// Upgrades a version 4 schema to a version 5 schema.
///
/// This just handles the directory meta files. If they're already in the new format, great.
/// Otherwise, verify they are consistent with the database then upgrade them.

use crate::db::FromSqlUuid;
use crate::{dir, schema};
use cstr::*;
use failure::{Error, Fail, bail};
use nix::fcntl::{FlockArg, OFlag};
use nix::sys::stat::Mode;
use protobuf::{Message, prelude::MessageField};
use rusqlite::params;
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;

const FIXED_DIR_META_LEN: usize = 512;

pub fn run(_args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    let db_uuid: FromSqlUuid =
        tx.query_row_and_then(r"select uuid from meta", params![], |row| row.get(0))?;
    let mut stmt = tx.prepare(r#"
        select
          d.path,
          d.uuid,
          d.last_complete_open_id,
          o.uuid
        from
          sample_file_dir d
          left join open o on (d.last_complete_open_id = o.id);
    "#)?;
    let mut rows = stmt.query(params![])?;
    while let Some(row) = rows.next()? {
        let path = row.get_raw_checked(0)?.as_str()?;
        let dir_uuid: FromSqlUuid = row.get(1)?;
        let open_id: Option<u32> = row.get(2)?;
        let open_uuid: Option<FromSqlUuid> = row.get(3)?;
        let mut db_meta = schema::DirMeta::new();
        db_meta.db_uuid.extend_from_slice(&db_uuid.0.as_bytes()[..]);
        db_meta.dir_uuid.extend_from_slice(&dir_uuid.0.as_bytes()[..]);
        match (open_id, open_uuid) {
            (Some(id), Some(uuid)) => {
                let mut o = db_meta.last_complete_open.mut_message();
                o.id = id;
                o.uuid.extend_from_slice(&uuid.0.as_bytes()[..]);
            },
            (None, None) => {},
            _ => bail!("open table missing id"),
        }

        let dir = dir::Fd::open(path, false)?;
        dir.lock(FlockArg::LockExclusiveNonblock)?;
        let tmp_path = cstr!("meta.tmp");
        let path = cstr!("meta");
        let mut f = dir.openat(path, OFlag::O_RDONLY, Mode::empty())?;
        let mut data = Vec::new();
        f.read_to_end(&mut data)?;
        if data.len() == FIXED_DIR_META_LEN {
            continue;  // already upgraded.
        }
        let mut s = protobuf::CodedInputStream::from_bytes(&data);
        let mut dir_meta = schema::DirMeta::new();
        dir_meta.merge_from(&mut s)
            .map_err(|e| e.context("Unable to parse metadata proto: {}"))?;
        if !dir::SampleFileDir::consistent(&db_meta, &dir_meta) {
            bail!("Inconsistent db_meta={:?} dir_meta={:?}", &db_meta, &dir_meta);
        }
        let mut f = dir.openat(tmp_path, OFlag::O_CREAT | OFlag::O_TRUNC | OFlag::O_WRONLY,
                               Mode::S_IRUSR | Mode::S_IWUSR)?;
        let mut data =
            dir_meta.write_length_delimited_to_bytes().expect("proto3->vec is infallible");
        if data.len() > FIXED_DIR_META_LEN {
            bail!("Length-delimited DirMeta message requires {} bytes, over limit of {}",
                  data.len(), FIXED_DIR_META_LEN);
        }
        data.resize(FIXED_DIR_META_LEN, 0);  // pad to required length.
        f.write_all(&data)?;
        f.sync_all()?;
        nix::fcntl::renameat(dir.as_raw_fd(), tmp_path, dir.as_raw_fd(), path)?;
        dir.sync()?;
    }
    Ok(())
}
