// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2016 Scott Lamb <slamb@slamb.org>
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

use db::dir;
use docopt;
use failure::{Error, Fail};
use libc;
use rusqlite;
use serde::Deserialize;
use std::path::Path;

mod check;
mod config;
mod login;
mod init;
mod run;
mod sql;
mod ts;
mod upgrade;

#[derive(Debug, Deserialize)]
pub enum Command {
    Check,
    Config,
    Login,
    Init,
    Run,
    Sql,
    Ts,
    Upgrade,
}

impl Command {
    pub fn run(&self) -> Result<(), Error> {
        match *self {
            Command::Check => check::run(),
            Command::Config => config::run(),
            Command::Login => login::run(),
            Command::Init => init::run(),
            Command::Run => run::run(),
            Command::Sql => sql::run(),
            Command::Ts => ts::run(),
            Command::Upgrade => upgrade::run(),
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum OpenMode {
    ReadOnly,
    ReadWrite,
    Create
}

/// Locks the directory without opening the database.
/// The returned `dir::Fd` holds the lock and should be kept open as long as the `Connection` is.
fn open_dir(db_dir: &str, mode: OpenMode) -> Result<dir::Fd, Error> {
    let dir = dir::Fd::open(db_dir, mode == OpenMode::Create)?;
    let ro = mode == OpenMode::ReadOnly;
    dir.lock(if ro { libc::LOCK_SH } else { libc::LOCK_EX } | libc::LOCK_NB)
       .map_err(|e| e.context(format!("db dir {:?} already in use; can't get {} lock",
                                      db_dir, if ro { "shared" } else { "exclusive" })))?;
    Ok(dir)
}

/// Locks and opens the database.
/// The returned `dir::Fd` holds the lock and should be kept open as long as the `Connection` is.
fn open_conn(db_dir: &str, mode: OpenMode) -> Result<(dir::Fd, rusqlite::Connection), Error> {
    let dir = open_dir(db_dir, mode)?;
    let conn = rusqlite::Connection::open_with_flags(
        Path::new(&db_dir).join("db"),
        match mode {
            OpenMode::ReadOnly => rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            OpenMode::ReadWrite => rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
            OpenMode::Create => {
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
            },
        } |
        // rusqlite::Connection is not Sync, so there's no reason to tell SQLite3 to use the
        // serialized threading mode.
        rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    Ok((dir, conn))
}

fn parse_args<'a, T>(usage: &str) -> Result<T, Error> where T: ::serde::Deserialize<'a> {
    Ok(docopt::Docopt::new(usage)
                      .and_then(|d| d.deserialize())
                      .unwrap_or_else(|e| e.exit()))
}
