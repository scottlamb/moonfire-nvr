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

use dir;
use docopt;
use error::Error;
use libc;
use rusqlite;
use slog::{self, DrainExt};
use slog_envlogger;
use slog_stdlog;
use slog_term;
use std::path::Path;

mod check;
mod config;
mod init;
mod run;
mod ts;
mod upgrade;

#[derive(Debug, RustcDecodable)]
pub enum Command {
    Check,
    Config,
    Init,
    Run,
    Ts,
    Upgrade,
}

impl Command {
    pub fn run(&self) -> Result<(), Error> {
        match *self {
            Command::Check => check::run(),
            Command::Config => config::run(),
            Command::Init => init::run(),
            Command::Run => run::run(),
            Command::Ts => ts::run(),
            Command::Upgrade => upgrade::run(),
        }
    }
}

/// Initializes logging.
/// `async` should be true only for serving; otherwise logging can block useful work.
/// Sync logging should be preferred for other modes because async apparently is never flushed
/// before the program exits, and partial output from these tools is very confusing.
fn install_logger(async: bool) {
    let drain = slog_term::StreamerBuilder::new().stderr();
    let drain = slog_envlogger::new(if async { drain.async() } else { drain }.full().build());
    slog_stdlog::set_logger(slog::Logger::root(drain.ignore_err(), None)).unwrap();
}

#[derive(PartialEq, Eq)]
enum OpenMode {
    ReadOnly,
    ReadWrite,
    Create
}

/// Locks and opens the database.
/// The returned `dir::Fd` holds the lock and should be kept open as long as the `Connection` is.
fn open_conn(db_dir: &str, mode: OpenMode) -> Result<(dir::Fd, rusqlite::Connection), Error> {
    let dir = dir::Fd::open(db_dir)?;
    let ro = mode == OpenMode::ReadOnly;
    dir.lock(if ro { libc::LOCK_SH } else { libc::LOCK_EX } | libc::LOCK_NB)
       .map_err(|e| Error{description: format!("db dir {:?} already in use; can't get {} lock",
                                               db_dir,
                                               if ro { "shared" } else { "exclusive" }),
                          cause: Some(Box::new(e))})?;
    let conn = rusqlite::Connection::open_with_flags(
        Path::new(&db_dir).join("db"),
        match mode {
            OpenMode::ReadOnly => rusqlite::SQLITE_OPEN_READ_ONLY,
            OpenMode::ReadWrite => rusqlite::SQLITE_OPEN_READ_WRITE,
            OpenMode::Create => rusqlite::SQLITE_OPEN_READ_WRITE | rusqlite::SQLITE_OPEN_CREATE,
        } |
        // rusqlite::Connection is not Sync, so there's no reason to tell SQLite3 to use the
        // serialized threading mode.
        rusqlite::SQLITE_OPEN_NO_MUTEX)?;
    Ok((dir, conn))
}

fn parse_args<T>(usage: &str) -> Result<T, Error> where T: ::rustc_serialize::Decodable {
    Ok(docopt::Docopt::new(usage)
                      .and_then(|d| d.decode())
                      .unwrap_or_else(|e| e.exit()))
}
