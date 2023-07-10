// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2019 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

/// Upgrades a version 4 schema to a version 5 schema.
///
/// This just handles the directory meta files. If they're already in the new format, great.
/// Otherwise, verify they are consistent with the database then upgrade them.
use crate::db::SqlUuid;
use crate::{dir, schema};
use base::{bail, err, Error};
use cstr::cstr;
use nix::fcntl::{FlockArg, OFlag};
use nix::sys::stat::Mode;
use protobuf::Message;
use rusqlite::params;
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use tracing::info;
use uuid::Uuid;

const FIXED_DIR_META_LEN: usize = 512;

/// Maybe upgrades the `meta` file, returning if an upgrade happened (and thus a sync is needed).
fn maybe_upgrade_meta(dir: &dir::Fd, db_meta: &schema::DirMeta) -> Result<bool, Error> {
    let tmp_path = cstr!("meta.tmp");
    let meta_path = cstr!("meta");
    let mut f = crate::fs::openat(dir.as_raw_fd(), meta_path, OFlag::O_RDONLY, Mode::empty())?;
    let mut data = Vec::new();
    f.read_to_end(&mut data)?;
    if data.len() == FIXED_DIR_META_LEN {
        return Ok(false);
    }

    let mut s = protobuf::CodedInputStream::from_bytes(&data);
    let mut dir_meta = schema::DirMeta::new();
    dir_meta.merge_from(&mut s).map_err(|e| {
        err!(
            FailedPrecondition,
            msg("unable to parse metadata proto"),
            source(e)
        )
    })?;
    if let Err(e) = dir::SampleFileDir::check_consistent(db_meta, &dir_meta) {
        bail!(
            FailedPrecondition,
            msg("inconsistent db_meta={db_meta:?} dir_meta={dir_meta:?}: {e}"),
        );
    }
    let mut f = crate::fs::openat(
        dir.as_raw_fd(),
        tmp_path,
        OFlag::O_CREAT | OFlag::O_TRUNC | OFlag::O_WRONLY,
        Mode::S_IRUSR | Mode::S_IWUSR,
    )?;
    let mut data = dir_meta
        .write_length_delimited_to_bytes()
        .expect("proto3->vec is infallible");
    if data.len() > FIXED_DIR_META_LEN {
        bail!(
            Internal,
            msg(
                "length-delimited DirMeta message requires {} bytes, over limit of {}",
                data.len(),
                FIXED_DIR_META_LEN,
            ),
        );
    }
    data.resize(FIXED_DIR_META_LEN, 0); // pad to required length.
    f.write_all(&data)?;
    f.sync_all()?;

    nix::fcntl::renameat(
        Some(dir.as_raw_fd()),
        tmp_path,
        Some(dir.as_raw_fd()),
        meta_path,
    )?;
    Ok(true)
}

/// Looks for uuid-based filenames and deletes them.
///
/// The v1->v3 migration failed to remove garbage files prior to 433be217. Let's have a clean slate
/// at v5.
///
/// Returns true if something was done (and thus a sync is needed).
fn maybe_cleanup_garbage_uuids(dir: &dir::Fd) -> Result<bool, Error> {
    let mut need_sync = false;
    let mut dir2 = nix::dir::Dir::openat(
        dir.as_raw_fd(),
        ".",
        OFlag::O_DIRECTORY | OFlag::O_RDONLY,
        Mode::empty(),
    )?;
    for e in dir2.iter() {
        let e = e?;
        let f = e.file_name();
        info!("file: {}", f.to_str().unwrap());
        let f_str = match f.to_str() {
            Ok(f) => f,
            Err(_) => continue,
        };
        if Uuid::parse_str(f_str).is_ok() {
            info!("removing leftover garbage file {}", f_str);
            nix::unistd::unlinkat(
                Some(dir.as_raw_fd()),
                f,
                nix::unistd::UnlinkatFlags::NoRemoveDir,
            )?;
            need_sync = true;
        }
    }

    Ok(need_sync)
}

pub fn run(_args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    let db_uuid: SqlUuid =
        tx.query_row_and_then(r"select uuid from meta", params![], |row| row.get(0))?;
    let mut stmt = tx.prepare(
        r#"
        select
          d.path,
          d.uuid,
          d.last_complete_open_id,
          o.uuid
        from
          sample_file_dir d
          left join open o on (d.last_complete_open_id = o.id);
        "#,
    )?;
    let mut rows = stmt.query(params![])?;
    while let Some(row) = rows.next()? {
        let path = row.get_ref(0)?.as_str()?;
        info!("path: {}", path);
        let dir_uuid: SqlUuid = row.get(1)?;
        let open_id: Option<u32> = row.get(2)?;
        let open_uuid: Option<SqlUuid> = row.get(3)?;
        let mut db_meta = schema::DirMeta::new();
        db_meta.db_uuid.extend_from_slice(&db_uuid.0.as_bytes()[..]);
        db_meta
            .dir_uuid
            .extend_from_slice(&dir_uuid.0.as_bytes()[..]);
        match (open_id, open_uuid) {
            (Some(id), Some(uuid)) => {
                let o = db_meta.last_complete_open.mut_or_insert_default();
                o.id = id;
                o.uuid.extend_from_slice(&uuid.0.as_bytes()[..]);
            }
            (None, None) => {}
            _ => bail!(Internal, msg("open table missing id")),
        }

        let dir = dir::Fd::open(path, false)?;
        dir.lock(FlockArg::LockExclusiveNonblock)
            .map_err(|e| err!(e, msg("unable to lock dir {path}")))?;

        let mut need_sync = maybe_upgrade_meta(&dir, &db_meta)?;
        if maybe_cleanup_garbage_uuids(&dir)? {
            need_sync = true;
        }

        if need_sync {
            dir.sync()?;
        }
        info!("done with path: {}", path);
    }
    Ok(())
}
