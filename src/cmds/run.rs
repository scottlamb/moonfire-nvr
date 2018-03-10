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
use db::{self, dir, writer};
use failure::Error;
use fnv::FnvHashMap;
use futures::{Future, Stream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use stream;
use streamer;
use tokio_core::reactor;
use tokio_signal::unix::{Signal, SIGINT, SIGTERM};
use web;

// These are used in a hack to get the name of the current time zone (e.g. America/Los_Angeles).
// They seem to be correct for Linux and OS X at least.
const LOCALTIME_PATH: &'static str = "/etc/localtime";
const ZONEINFO_PATH: &'static str = "/usr/share/zoneinfo/";

const USAGE: &'static str = r#"
Usage: moonfire-nvr run [options]

Options:
    -h, --help             Show this message.
    --db-dir=DIR           Set the directory holding the SQLite3 index database.
                           This is typically on a flash device.
                           [default: /var/lib/moonfire-nvr/db]
    --ui-dir=DIR           Set the directory with the user interface files (.html, .js, etc).
                           [default: /usr/local/lib/moonfire-nvr/ui]
    --http-addr=ADDR       Set the bind address for the unencrypted HTTP server.
                           [default: 0.0.0.0:8080]
    --read-only            Forces read-only mode / disables recording.
"#;

#[derive(Debug, Deserialize)]
struct Args {
    flag_db_dir: String,
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

fn resolve_zone() -> String {
    let p = ::std::fs::read_link(LOCALTIME_PATH).expect("unable to read localtime symlink");
    let p = p.to_str().expect("localtime symlink destination must be valid UTF-8");
    if !p.starts_with(ZONEINFO_PATH) {
        panic!("Expected {} to point to a path within {}; actually points to {}",
               LOCALTIME_PATH, ZONEINFO_PATH, p);
    }
    p[ZONEINFO_PATH.len()..].into()
}

struct Syncer {
    dir: Arc<dir::SampleFileDir>,
    channel: writer::SyncerChannel<::std::fs::File>,
    join: thread::JoinHandle<()>,
}

pub fn run() -> Result<(), Error> {
    let args: Args = super::parse_args(USAGE)?;
    let clocks = Arc::new(clock::RealClocks{});
    let (_db_dir, conn) = super::open_conn(
        &args.flag_db_dir,
        if args.flag_read_only { super::OpenMode::ReadOnly } else { super::OpenMode::ReadWrite })?;
    let db = Arc::new(db::Database::new(clocks.clone(), conn, !args.flag_read_only).unwrap());
    info!("Database is loaded.");

    {
        let mut l = db.lock();
        let dirs_to_open: Vec<_> =
            l.streams_by_id().values().filter_map(|s| s.sample_file_dir_id).collect();
        l.open_sample_file_dirs(&dirs_to_open)?;
    }
    info!("Directories are opened.");

    let s = web::Service::new(db.clone(), Some(&args.flag_ui_dir), resolve_zone())?;

    // Start a streamer for each stream.
    let shutdown_streamers = Arc::new(AtomicBool::new(false));
    let mut streamers = Vec::new();
    let syncers = if !args.flag_read_only {
        let l = db.lock();
        let mut dirs = FnvHashMap::with_capacity_and_hasher(
            l.sample_file_dirs_by_id().len(), Default::default());
        let streams = l.streams_by_id().len();
        let env = streamer::Environment {
            db: &db,
            clocks: clocks.clone(),
            opener: &*stream::FFMPEG,
            shutdown: &shutdown_streamers,
        };

        // Get the directories that need syncers.
        for stream in l.streams_by_id().values() {
            if let (Some(id), true) = (stream.sample_file_dir_id, stream.record) {
                dirs.entry(id).or_insert_with(|| {
                    let d = l.sample_file_dirs_by_id().get(&id).unwrap();
                    info!("Starting syncer for path {}", d.path);
                    d.get().unwrap()
                });
            }
        }

        // Then, with the lock dropped, create syncers.
        drop(l);
        let mut syncers = FnvHashMap::with_capacity_and_hasher(dirs.len(), Default::default());
        for (id, dir) in dirs.drain() {
            let (channel, join) = writer::start_syncer(db.clone(), id)?;
            syncers.insert(id, Syncer {
                dir,
                channel,
                join,
            });
        }

        // Then start up streams.
        let l = db.lock();
        for (i, (id, stream)) in l.streams_by_id().iter().enumerate() {
            if !stream.record {
                continue;
            }
            let camera = l.cameras_by_id().get(&stream.camera_id).unwrap();
            let sample_file_dir_id = match stream.sample_file_dir_id {
                Some(s) => s,
                None => {
                    warn!("Can't record stream {} ({}/{}) because it has no sample file dir",
                          id, camera.short_name, stream.type_.as_str());
                    continue;
                },
            };
            let rotate_offset_sec = streamer::ROTATE_INTERVAL_SEC * i as i64 / streams as i64;
            let syncer = syncers.get(&sample_file_dir_id).unwrap();
            let mut streamer = streamer::Streamer::new(&env, syncer.dir.clone(),
                                                       syncer.channel.clone(), *id, camera, stream,
                                                       rotate_offset_sec,
                                                       streamer::ROTATE_INTERVAL_SEC);
            info!("Starting streamer for {}", streamer.short_name());
            let name = format!("s-{}", streamer.short_name());
            streamers.push(thread::Builder::new().name(name).spawn(move|| {
                streamer.run();
            }).expect("can't create thread"));
        }
        drop(l);
        Some(syncers)
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

    if let Some(mut ss) = syncers {
        // The syncers shut down when all channels to them have been dropped.
        // The database maintains one; and `ss` holds one. Drop both.
        db.lock().clear_on_flush();
        for (_, s) in ss.drain() {
            drop(s.channel);
            s.join.join().unwrap();
        }
    }

    info!("Exiting.");
    Ok(())
}
