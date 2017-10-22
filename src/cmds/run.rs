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

use clock;
use db;
use dir;
use error::Error;
use futures::{Future, Stream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use stream;
use streamer;
use tokio_core::reactor;
use tokio_signal::unix::{Signal, SIGINT, SIGTERM};
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
    --ui-dir=DIR           Set the directory with the user interface files (.html, .js, etc).
                           [default: /usr/local/lib/moonfire-nvr/ui]
    --http-addr=ADDR       Set the bind address for the unencrypted HTTP server.
                           [default: 0.0.0.0:8080]
    --read-only            Forces read-only mode / disables recording.
"#;

#[derive(Debug, Deserialize)]
struct Args {
    flag_db_dir: String,
    flag_sample_file_dir: String,
    flag_http_addr: String,
    flag_ui_dir: String,
    flag_read_only: bool,
}

fn setup_shutdown_future(h: &reactor::Handle) -> Box<Future<Item = (), Error = ()>> {
    let int = Signal::new(SIGINT, h).flatten_stream().into_future();
    let term = Signal::new(SIGTERM, h).flatten_stream().into_future();
    Box::new(int.select(term)
                .map(|_| ())
                .map_err(|_| ()))
}

pub fn run() -> Result<(), Error> {
    let args: Args = super::parse_args(USAGE)?;
    let (_db_dir, conn) = super::open_conn(
        &args.flag_db_dir,
        if args.flag_read_only { super::OpenMode::ReadOnly } else { super::OpenMode::ReadWrite })?;
    let db = Arc::new(db::Database::new(conn).unwrap());
    let dir = dir::SampleFileDir::new(&args.flag_sample_file_dir, db.clone()).unwrap();
    info!("Database is loaded.");

    let s = web::Service::new(db.clone(), dir.clone(), Some(&args.flag_ui_dir))?;

    // Start a streamer for each camera.
    let shutdown_streamers = Arc::new(AtomicBool::new(false));
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
            shutdown: &shutdown_streamers,
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
    let addr = args.flag_http_addr.parse().unwrap();
    let server = ::hyper::server::Http::new()
        .bind(&addr, move || Ok(s.clone()))
        .unwrap();

    let shutdown = setup_shutdown_future(&server.handle());

    info!("Ready to serve HTTP requests");
    server.run_until(shutdown).unwrap();

    info!("Shutting down streamers.");
    shutdown_streamers.store(true, Ordering::SeqCst);
    for streamer in streamers.drain(..) {
        streamer.join().unwrap();
    }

    if let Some((syncer_channel, syncer_join)) = syncer {
        info!("Shutting down syncer.");
        drop(syncer_channel);
        syncer_join.join().unwrap();
    }

    info!("Exiting.");
    Ok(())
}
