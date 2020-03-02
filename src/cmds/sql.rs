// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2019 The Moonfire NVR Authors
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
use serde::Deserialize;
use std::process::Command;
use super::OpenMode;

static USAGE: &'static str = r#"
Runs a SQLite shell on the Moonfire NVR database with locking.

Usage:

    moonfire-nvr sql [options] [--] [<arg>...]
    moonfire-nvr sql --help

Positional arguments will be passed to sqlite3. Use the -- separator to pass
sqlite3 options, as in "moonfire-nvr sql -- -line 'select username from user'".

Options:

    --db-dir=DIR           Set the directory holding the SQLite3 index database.
                           This is typically on a flash device.
                           [default: /var/lib/moonfire-nvr/db]
    --read-only            Accesses the database in read-only mode.
"#;

#[derive(Debug, Deserialize)]
struct Args {
    flag_db_dir: String,
    flag_read_only: bool,
    arg_arg: Vec<String>,
}

pub fn run() -> Result<(), Error> {
    let args: Args = super::parse_args(USAGE)?;

    let mode = if args.flag_read_only { OpenMode::ReadWrite } else { OpenMode::ReadOnly };
    let _db_dir = super::open_dir(&args.flag_db_dir, mode)?;
    let mut db = format!("file:{}/db", &args.flag_db_dir);
    if args.flag_read_only {
        db.push_str("?mode=ro");
    }
    Command::new("sqlite3").arg(&db).args(&args.arg_arg).status()?;
    Ok(())
}
