// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2019-2020 The Moonfire NVR Authors
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

//! Subcommand to run a SQLite shell.

use failure::Error;
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use super::OpenMode;
use structopt::StructOpt;

#[derive(StructOpt)]
pub struct Args {
    /// Directory holding the SQLite3 index database.
    #[structopt(long, default_value = "/var/lib/moonfire-nvr/db", value_name="path",
                parse(from_os_str))]
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

pub fn run(args: &Args) -> Result<i32, Error> {
    let mode = if args.read_only { OpenMode::ReadOnly } else { OpenMode::ReadWrite };
    let _db_dir = super::open_dir(&args.db_dir, mode)?;
    let mut db = OsString::new();
    db.push("file:");
    db.push(&args.db_dir);
    db.push("/db");
    if args.read_only {
        db.push("?mode=ro");
    }
    Err(Command::new("sqlite3").arg(&db).args(&args.arg).exec().into())
}
