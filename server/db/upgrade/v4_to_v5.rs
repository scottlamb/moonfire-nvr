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
use nix::fcntl::{FlockArg, OFlag};
use nix::sys::stat::Mode;
use protobuf::Message;
use rusqlite::params;
use std::io::{Read, Write};
use std::os::fd::AsFd as _;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;

const FIXED_DIR_META_LEN: usize = 512;

/// Maybe upgrades the `meta` file, returning if an upgrade happened (and thus a sync is needed).
fn maybe_upgrade_meta(dir: &crate::fs::Dir, cfg: &dir::Config) -> Result<bool, Error> {
    let tmp_path = c"meta.tmp";
    let meta_path = c"meta";
    let mut f = crate::fs::openat(
        dir.as_fd().as_raw_fd(),
        meta_path,
        OFlag::O_RDONLY,
        Mode::empty(),
    )?;
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
    cfg.check_consistent(&dir_meta)?;
    let mut f = crate::fs::openat(
        dir.as_fd().as_raw_fd(),
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
        Some(dir.as_fd().as_raw_fd()),
        tmp_path,
        Some(dir.as_fd().as_raw_fd()),
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
fn maybe_cleanup_garbage_uuids(dir: &crate::fs::Dir) -> Result<bool, Error> {
    let mut need_sync = false;
    let mut dir2 = nix::dir::Dir::openat(
        dir.as_fd().as_raw_fd(),
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
                Some(dir.as_fd().as_raw_fd()),
                f,
                nix::unistd::UnlinkatFlags::NoRemoveDir,
            )?;
            need_sync = true;
        }
    }

    Ok(need_sync)
}

pub fn run(_args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    let SqlUuid(db_uuid) =
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
    let flusher_notify = Arc::new(tokio::sync::Notify::new()); // dummy
    while let Some(row) = rows.next()? {
        let path = PathBuf::from(row.get_ref(0)?.as_str()?);
        info!("path: {}", path.display());
        let SqlUuid(dir_uuid) = row.get(1)?;
        let open_id = row.get(2)?;
        let SqlUuid(open_uuid) = row.get(3)?;
        let cfg = crate::dir::Config {
            path,
            db_uuid,
            dir_uuid,
            last_complete_open: Some(crate::db::Open {
                id: open_id,
                uuid: open_uuid,
            }),
            current_open: None,
            flusher_notify: flusher_notify.clone(),
        };

        let dir = crate::fs::Dir::open(&cfg.path, false)?;
        dir.lock(FlockArg::LockExclusiveNonblock)
            .map_err(|e| err!(e, msg("unable to lock dir {}", cfg.path.display())))?;

        let mut need_sync = maybe_upgrade_meta(&dir, &cfg)?;
        if maybe_cleanup_garbage_uuids(&dir)? {
            need_sync = true;
        }

        if need_sync {
            nix::unistd::fsync(dir.0)?;
        }
        info!("done with path: {}", cfg.path.display());
    }
    Ok(())
}
