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

#![cfg_attr(all(feature="nightly", test), feature(test))]

extern crate byteorder;
extern crate core;
#[macro_use] extern crate chan;
extern crate chan_signal;
extern crate docopt;
#[macro_use] extern crate ffmpeg;
extern crate ffmpeg_sys;
extern crate fnv;
extern crate http_entity;
extern crate hyper;
#[macro_use] extern crate lazy_static;
extern crate libc;
#[macro_use] extern crate log;
extern crate lru_cache;
extern crate rusqlite;
extern crate memmap;
#[macro_use] extern crate mime;
extern crate openssl;
extern crate regex;
extern crate rustc_serialize;
extern crate serde;
extern crate serde_json;
extern crate slog;
extern crate slog_envlogger;
extern crate slog_stdlog;
extern crate slog_term;
extern crate smallvec;
extern crate time;
extern crate url;
extern crate uuid;

use hyper::server::Server;
use slog::DrainExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

mod check;
mod clock;
mod coding;
mod db;
mod dir;
mod error;
mod h264;
mod mmapfile;
mod mp4;
mod pieces;
mod recording;
mod stream;
mod streamer;
mod strutil;
#[cfg(test)] mod testutil;
mod upgrade;
mod web;

/// Commandline usage string. This is in the particular format expected by the `docopt` crate.
/// Besides being printed on --help or argument parsing error, it's actually parsed to define the
/// allowed commandline arguments and their defaults.
const USAGE: &'static str = "
Usage: moonfire-nvr [options]
       moonfire-nvr --upgrade [options]
       moonfire-nvr --check [options]
       moonfire-nvr (--help | --version)

Options:
    -h, --help             Show this message.
    --version              Show the version of moonfire-nvr.
    --db-dir=DIR           Set the directory holding the SQLite3 index database.
                           This is typically on a flash device.
                           [default: /var/lib/moonfire-nvr/db]
    --sample-file-dir=DIR  Set the directory holding video data.
                           This is typically on a hard drive.
                           [default: /var/lib/moonfire-nvr/sample]
    --http-addr=ADDR       Set the bind address for the unencrypted HTTP server.
                           [default: 0.0.0.0:8080]
    --read-only            Forces read-only mode / disables recording.
    --preset-journal=MODE  With --upgrade, resets the SQLite journal_mode to
                           the specified mode prior to the upgrade. The default,
                           delete, is recommended. off is very dangerous but
                           may be desirable in some circumstances. See
                           guide/schema.md for more information. The journal
                           mode will be reset to wal after the upgrade.
                           [default: delete]
    --no-vacuum            With --upgrade, skips the normal post-upgrade vacuum
                           operation.
";

/// Commandline arguments corresponding to `USAGE`; automatically filled by the `docopt` crate.
#[derive(RustcDecodable)]
struct Args {
    flag_db_dir: String,
    flag_sample_file_dir: String,
    flag_http_addr: String,
    flag_read_only: bool,
    flag_check: bool,
    flag_upgrade: bool,
    flag_no_vacuum: bool,
    flag_preset_journal: String,
}

fn main() {
    // Parse commandline arguments.
    let version = "Moonfire NVR 0.1.0".to_owned();
    let args: Args = docopt::Docopt::new(USAGE)
                                    .and_then(|d| d.version(Some(version)).decode())
                                    .unwrap_or_else(|e| e.exit());

    // Watch for termination signals.
    // This must be started before any threads are spawned (such as the async logger thread) so
    // that signals will be blocked in all threads.
    let signal = chan_signal::notify(&[chan_signal::Signal::INT, chan_signal::Signal::TERM]);

    // Initialize logging.
    // Use async logging for serving because otherwise it blocks useful work.
    // Use sync logging for other modes because async apparently is never flushed before the
    // program exits, and partial output from these tools is very confusing.
    let drain = slog_term::StreamerBuilder::new();
    let drain = slog_envlogger::new(if args.flag_upgrade || args.flag_check { drain }
                                    else { drain.async() }.full().build());
    slog_stdlog::set_logger(slog::Logger::root(drain.ignore_err(), None)).unwrap();

    // Open the database and populate cached state.
    let db_dir = dir::Fd::open(&args.flag_db_dir).unwrap();
    db_dir.lock(if args.flag_read_only { libc::LOCK_SH } else { libc::LOCK_EX } | libc::LOCK_NB)
        .unwrap();
    let conn = rusqlite::Connection::open_with_flags(
        Path::new(&args.flag_db_dir).join("db"),
        if args.flag_read_only {
            rusqlite::SQLITE_OPEN_READ_ONLY
        } else {
            rusqlite::SQLITE_OPEN_READ_WRITE
        } |
        // rusqlite::Connection is not Sync, so there's no reason to tell SQLite3 to use the
        // serialized threading mode.
        rusqlite::SQLITE_OPEN_NO_MUTEX).unwrap();

    if args.flag_upgrade {
        upgrade::run(conn, &args.flag_preset_journal, args.flag_no_vacuum).unwrap();
    } else if args.flag_check {
        check::run(conn, &args.flag_sample_file_dir).unwrap();
    } else {
        run(args, conn, &signal);
    }
}

fn run(args: Args, conn: rusqlite::Connection, signal: &chan::Receiver<chan_signal::Signal>) {
    let db = Arc::new(db::Database::new(conn).unwrap());
    let dir = dir::SampleFileDir::new(&args.flag_sample_file_dir, db.clone()).unwrap();
    info!("Database is loaded.");

    // Start a streamer for each camera.
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut streamers = Vec::new();
    let syncer = if !args.flag_read_only {
        let (syncer_channel, syncer_join) = dir::start_syncer(dir.clone()).unwrap();
        let l = db.lock();
        let cameras = l.cameras_by_id().len();
        let env = streamer::Environment{
            db: &db,
            dir: &dir,
            clocks: &clock::REAL,
            opener: &*stream::FFMPEG,
            shutdown: &shutdown,
        };
        for (i, (id, camera)) in l.cameras_by_id().iter().enumerate() {
            let rotate_offset_sec = streamer::ROTATE_INTERVAL_SEC * i as i64 / cameras as i64;
            let mut streamer = streamer::Streamer::new(&env, syncer_channel.clone(), *id, camera,
                                                       rotate_offset_sec,
                                                       streamer::ROTATE_INTERVAL_SEC);
            let name = format!("stream-{}", streamer.short_name());
            streamers.push(thread::Builder::new().name(name).spawn(move|| {
                streamer.run();
            }).expect("can't create thread"));
        }
        Some((syncer_channel, syncer_join))
    } else { None };

    // Start the web interface.
    let server = Server::http(args.flag_http_addr.as_str()).unwrap();
    let h = web::Handler::new(db.clone(), dir.clone());
    let _guard = server.handle(h);
    info!("Ready to serve HTTP requests");

    // Wait for a signal and shut down.
    chan_select! {
        signal.recv() -> signal => info!("Received signal {:?}; shutting down streamers.", signal),
    }
    shutdown.store(true, Ordering::SeqCst);
    for streamer in streamers.drain(..) {
        streamer.join().unwrap();
    }
    if let Some((syncer_channel, syncer_join)) = syncer {
        info!("Shutting down syncer.");
        drop(syncer_channel);
        syncer_join.join().unwrap();
    }
    info!("Exiting.");
    // TODO: drain the logger.
    std::process::exit(0);
}
