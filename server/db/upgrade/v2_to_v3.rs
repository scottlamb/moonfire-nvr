// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

/// Upgrades a version 2 schema to a version 3 schema.
/// Note that a version 2 schema is never actually used; so we know the upgrade from version 1 was
/// completed, and possibly an upgrade from 2 to 3 is half-finished.
use crate::db::{self, SqlUuid};
use crate::dir;
use base::{Error, ErrorKind, FastHashSet};
use rusqlite::params;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

/// Opens the sample file dir.
///
/// Does not populate `garbage_needs_unlink`.
///
/// Makes a couple simplifying assumptions valid for version 2:
/// *   there's only one dir.
/// *   it has a last completed open.
fn open_sample_file_dir(tx: &rusqlite::Transaction) -> Result<dir::Pool, Error> {
    let (p, SqlUuid(dir_uuid), o_id, SqlUuid(o_uuid), SqlUuid(db_uuid)): (String, _, i32, _, _) =
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
    let pool = crate::dir::Pool::new(
        crate::dir::Config {
            path: PathBuf::from(p),
            db_uuid,
            dir_uuid,
            last_complete_open: Some(crate::db::Open {
                id: o_id as u32,
                uuid: o_uuid,
            }),
            current_open: None,
            flusher_notify: Arc::new(tokio::sync::Notify::new()), // dummy
        },
        FastHashSet::default(),
    );
    futures::executor::block_on(pool.open(const { NonZeroUsize::new(1).unwrap() }))?;
    Ok(pool)
}

pub fn run(_args: &super::Args, tx: &rusqlite::Transaction) -> Result<(), Error> {
    let pool = open_sample_file_dir(tx)?;

    let (rename_tx, rename_rx) = std::sync::mpsc::sync_channel(16);

    // In a pool worker, run the renames. Note that `run` starts working
    // eagerly, before waiting on the future.
    let rename_fut = pool.run("rename", |ctx| {
        for (from, to) in rename_rx {
            let from = super::UuidPath::from(from);
            let to = crate::dir::CompositeIdPath::from(to);
            match ctx.rename(&from, &to) {
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    // assume already renamed.
                }
                Err(e) => return Err(e),
                Ok(()) => {}
            }
        }
        Ok(())
    });

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
        let SqlUuid(sample_file_uuid) = row.get(1)?;
        rename_tx
            .send((sample_file_uuid, id))
            .expect("rename_rx not closed");
    }
    drop(rename_tx);
    futures::executor::block_on(rename_fut)?;
    futures::executor::block_on(pool.close())?;

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
