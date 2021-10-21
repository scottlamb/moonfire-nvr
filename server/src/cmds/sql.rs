// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Subcommand to run a SQLite shell.

use super::OpenMode;
use failure::Error;
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
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

    /// Opens the database in read-only mode and locks it only for shared access.
    ///
    /// This can be run simultaneously with "moonfire-nvr run --read-only".
    #[structopt(long)]
    read_only: bool,

    /// Arguments to pass to sqlite3.
    ///
    /// Use the -- separator to pass sqlite3 options, as in
    /// "moonfire-nvr sql -- -line 'select username from user'".
    #[structopt(parse(from_os_str))]
    arg: Vec<OsString>,
}

pub fn run(args: Args) -> Result<i32, Error> {
    let mode = if args.read_only {
        OpenMode::ReadOnly
    } else {
        OpenMode::ReadWrite
    };
    let _db_dir = super::open_dir(&args.db_dir, mode)?;
    let mut db = OsString::new();
    db.push("file:");
    db.push(&args.db_dir);
    db.push("/db");
    if args.read_only {
        db.push("?mode=ro");
    }
    Err(Command::new("sqlite3")
        .arg(&db)
        .args(&args.arg)
        .exec()
        .into())
}
