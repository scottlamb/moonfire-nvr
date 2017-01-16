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

use chan_signal;
use clock;
use db;
use dir;
use error::Error;
use hyper::server::Server;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use stream;
use streamer;
use web;

const USAGE: &'static str = r#"
Usage: moonfire-nvr run [options]

Options:
    -h, --help             Show this message.
    --db-dir=DIR           Set the directory holding the SQLite3 index database.
                           This is typically on a flash device.
                           [default: /var/lib/moonfire-nvr/db]
    --sample-file-dir=DIR  Set the directory holding video data.
                           This is typically on a hard drive.
                           [default: /var/lib/moonfire-nvr/sample]
    --http-addr=ADDR       Set the bind address for the unencrypted HTTP server.
                           [default: 0.0.0.0:8080]
    --read-only            Forces read-only mode / disables recording.
"#;

#[derive(Debug, RustcDecodable)]
struct Args {
    flag_db_dir: String,
    flag_sample_file_dir: String,
    flag_http_addr: String,
    flag_read_only: bool,
}

pub fn run() -> Result<(), Error> {
    let args: Args = super::parse_args(USAGE)?;

    // Watch for termination signals.
    // This must be started before any threads are spawned (such as the async logger thread) so
    // that signals will be blocked in all threads.
    let signal = chan_signal::notify(&[chan_signal::Signal::INT, chan_signal::Signal::TERM]);
    super::install_logger(true);
    let (_db_dir, conn) = super::open_conn(&args.flag_db_dir, args.flag_read_only)?;
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
    ::std::process::exit(0);
}
