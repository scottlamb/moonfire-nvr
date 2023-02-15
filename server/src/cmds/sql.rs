// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Subcommand to run a SQLite shell.

use super::OpenMode;
use bpaf::Bpaf;
use failure::Error;
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

/// Runs a SQLite3 shell on Moonfire NVR's index database.
///
///
/// Note this locks the database to prevent simultaneous access with a running server. The
/// server maintains cached state which could be invalidated otherwise.
#[derive(Bpaf, Debug, PartialEq, Eq)]
#[bpaf(options)]
pub struct Args {
    #[bpaf(external(crate::parse_db_dir))]
    db_dir: PathBuf,

    /// Opens the database in read-only mode and locks it only for shared access.
    ///
    /// This can be run simultaneously with `moonfire-nvr run --read-only`.
    read_only: bool,

    /// Arguments to pass to sqlite3.
    ///
    /// Use the `--` separator to pass sqlite3 options, as in
    /// `moonfire-nvr sql -- -line 'select username from user'`.
    #[bpaf(positional)]
    arg: Vec<OsString>,
}

pub fn subcommand() -> impl bpaf::Parser<Args> {
    crate::subcommand(args(), "sql")
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
        .args(db::db::INTEGRITY_PRAGMAS.iter().flat_map(|p| ["-cmd", p]))
        .arg(&db)
        .args(&args.arg)
        .exec()
        .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args() {
        let args = args()
            .run_inner(bpaf::Args::from(&[
                "--db-dir",
                "/foo/bar",
                "--",
                "-line",
                "select username from user",
            ]))
            .unwrap();
        assert_eq!(
            args,
            Args {
                db_dir: "/foo/bar".into(),
                read_only: false, // default
                arg: vec!["-line".into(), "select username from user".into()],
            }
        );
    }
}
