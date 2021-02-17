// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use failure::Error;
use log::info;
use std::path::PathBuf;
use structopt::StructOpt;

#[derive(StructOpt)]
pub struct Args {
    /// Directory holding the SQLite3 index database.
    #[structopt(
        long,
        default_value = "/var/lib/moonfire-nvr/db",
        value_name = "path",
        parse(from_os_str)
    )]
    db_dir: PathBuf,
}

pub fn run(args: &Args) -> Result<i32, Error> {
    let (_db_dir, mut conn) = super::open_conn(&args.db_dir, super::OpenMode::Create)?;

    // Check if the database has already been initialized.
    let cur_ver = db::get_schema_version(&conn)?;
    if let Some(v) = cur_ver {
        info!("Database is already initialized with schema version {}.", v);
        return Ok(0);
    }

    // Use WAL mode (which is the most efficient way to preserve database integrity) with a large
    // page size (so reading large recording_playback rows doesn't require as many seeks). Changing
    // the page size requires doing a vacuum in non-WAL mode. This will be cheap on an empty
    // database. https://www.sqlite.org/pragma.html#pragma_page_size
    conn.execute_batch(
        r#"
        pragma journal_mode = delete;
        pragma page_size = 16384;
        vacuum;
        pragma journal_mode = wal;
        "#,
    )?;
    db::init(&mut conn)?;
    info!("Database initialized.");
    Ok(0)
}
