// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use crate::streamer;
use crate::web;
use base::clock;
use db::{dir, writer};
use failure::{bail, Error, ResultExt};
use fnv::FnvHashMap;
use futures::future::FutureExt;
use hyper::service::{make_service_fn, service_fn};
use log::{info, warn};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use structopt::StructOpt;
use tokio::signal::unix::{signal, SignalKind};

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

    /// The number of worker threads used by the asynchronous runtime.
    /// Defaults to the number of cores on the system.
    #[structopt(long, value_name = "worker_threads")]
    worker_threads: Option<usize>,

    /// Directory holding user interface files (.html, .js, etc).
    #[structopt(
        long,
        default_value = "/usr/local/lib/moonfire-nvr/ui",
        value_name = "path",
        parse(from_os_str)
    )]
    ui_dir: std::path::PathBuf,

    /// Bind address for unencrypted HTTP server.
    #[structopt(long, default_value = "0.0.0.0:8080", parse(try_from_str))]
    http_addr: std::net::SocketAddr,

    /// Open the database in read-only mode and disables recording.
    ///
    /// Note this is incompatible with authentication, so you'll likely want to specify
    /// --allow_unauthenticated_permissions.
    #[structopt(long)]
    read_only: bool,

    /// Allow unauthenticated access to the web interface, with the given permissions (may be
    /// empty). Should be a text Permissions protobuf such as "view_videos: true".
    ///
    /// Note that even an empty string allows some basic access that would be rejected if the
    /// argument were omitted.
    #[structopt(long, parse(try_from_str = protobuf::text_format::parse_from_str))]
    allow_unauthenticated_permissions: Option<db::Permissions>,

    /// Trust X-Real-IP: and X-Forwarded-Proto: headers on the incoming request.
    ///
    /// Set this only after ensuring your proxy server is configured to set them and that no
    /// untrusted requests bypass the proxy server. You may want to specify
    /// --http-addr=127.0.0.1:8080.
    #[structopt(long)]
    trust_forward_hdrs: bool,

    /// RTSP library to use for fetching the cameras' video stream.
    /// Moonfire NVR is in the process of switching from `ffmpeg` (used since
    /// the beginning of the project) to `retina` (a pure-Rust RTSP library
    /// developed by Moonfire NVR's author).
    #[structopt(long, default_value = "retina", parse(try_from_str))]
    rtsp_library: crate::stream::RtspLibrary,

    #[structopt(long, default_value)]
    rtsp_transport: retina::client::Transport,
}

// These are used in a hack to get the name of the current time zone (e.g. America/Los_Angeles).
// They seem to be correct for Linux and macOS at least.
const LOCALTIME_PATH: &str = "/etc/localtime";
const TIMEZONE_PATH: &str = "/etc/timezone";
const ZONEINFO_PATHS: [&str; 2] = [
    "/usr/share/zoneinfo/",       // Linux, macOS < High Sierra
    "/var/db/timezone/zoneinfo/", // macOS High Sierra
];

fn trim_zoneinfo(path: &str) -> &str {
    for zp in &ZONEINFO_PATHS {
        if let Some(p) = path.strip_prefix(zp) {
            return p;
        }
    }
    path
}

/// Attempt to resolve the timezone of the server.
/// The Javascript running in the browser needs this to match the server's timezone calculations.
fn resolve_zone() -> Result<String, Error> {
    // If the environmental variable `TZ` exists, is valid UTF-8, and doesn't just reference
    // `/etc/localtime/`, use that.
    if let Ok(tz) = ::std::env::var("TZ") {
        let mut p: &str = &tz;

        // Strip of an initial `:` if present. Having `TZ` set in this way is a trick to avoid
        // repeated `tzset` calls:
        // https://blog.packagecloud.io/eng/2017/02/21/set-environment-variable-save-thousands-of-system-calls/
        if p.starts_with(':') {
            p = &p[1..];
        }

        p = trim_zoneinfo(p);

        if !p.starts_with('/') {
            return Ok(p.to_owned());
        }
        if p != LOCALTIME_PATH {
            bail!("Unable to resolve env TZ={} to a timezone.", &tz);
        }
    }

    // If `LOCALTIME_PATH` is a symlink, use that. On some systems, it's instead a copy of the
    // desired timezone, which unfortunately doesn't contain its own name.
    match ::std::fs::read_link(LOCALTIME_PATH) {
        Ok(localtime_dest) => {
            let localtime_dest = match localtime_dest.to_str() {
                Some(d) => d,
                None => bail!("{} symlink destination is invalid UTF-8", LOCALTIME_PATH),
            };
            let p = trim_zoneinfo(localtime_dest);
            if p.starts_with('/') {
                bail!(
                    "Unable to resolve {} symlink destination {} to a timezone.",
                    LOCALTIME_PATH,
                    &localtime_dest
                );
            }
            return Ok(p.to_owned());
        }
        Err(e) => {
            use ::std::io::ErrorKind;
            if e.kind() != ErrorKind::NotFound && e.kind() != ErrorKind::InvalidInput {
                bail!("Unable to read {} symlink: {}", LOCALTIME_PATH, e);
            }
        }
    };

    // If `TIMEZONE_PATH` is a file, use its contents as the zone name.
    match ::std::fs::read_to_string(TIMEZONE_PATH) {
        Ok(z) => Ok(z),
        Err(e) => {
            bail!(
                "Unable to resolve timezone from TZ env, {}, or {}. Last error: {}",
                LOCALTIME_PATH,
                TIMEZONE_PATH,
                e
            );
        }
    }
}

struct Syncer {
    dir: Arc<dir::SampleFileDir>,
    channel: writer::SyncerChannel<::std::fs::File>,
    join: thread::JoinHandle<()>,
}

pub fn run(args: &Args) -> Result<i32, Error> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    if let Some(worker_threads) = args.worker_threads {
        builder.worker_threads(worker_threads);
    }
    builder.build().unwrap().block_on(async_run(args))
}

async fn async_run(args: &Args) -> Result<i32, Error> {
    let clocks = clock::RealClocks {};
    let (_db_dir, conn) = super::open_conn(
        &args.db_dir,
        if args.read_only {
            super::OpenMode::ReadOnly
        } else {
            super::OpenMode::ReadWrite
        },
    )?;
    let db = Arc::new(db::Database::new(clocks, conn, !args.read_only).unwrap());
    info!("Database is loaded.");

    {
        let mut l = db.lock();
        let dirs_to_open: Vec<_> = l
            .streams_by_id()
            .values()
            .filter_map(|s| s.sample_file_dir_id)
            .collect();
        l.open_sample_file_dirs(&dirs_to_open)?;
    }
    info!("Directories are opened.");

    let time_zone_name = resolve_zone()?;
    info!("Resolved timezone: {}", &time_zone_name);
    let svc = Arc::new(web::Service::new(web::Config {
        db: db.clone(),
        ui_dir: Some(&args.ui_dir),
        allow_unauthenticated_permissions: args.allow_unauthenticated_permissions.clone(),
        trust_forward_hdrs: args.trust_forward_hdrs,
        time_zone_name,
    })?);

    // Start a streamer for each stream.
    let shutdown_streamers = Arc::new(AtomicBool::new(false));
    let mut streamers = Vec::new();
    let syncers = if !args.read_only {
        let l = db.lock();
        let mut dirs = FnvHashMap::with_capacity_and_hasher(
            l.sample_file_dirs_by_id().len(),
            Default::default(),
        );
        let streams = l.streams_by_id().len();
        let env = streamer::Environment {
            db: &db,
            opener: args.rtsp_library.opener(),
            transport: args.rtsp_transport,
            shutdown: &shutdown_streamers,
        };

        // Get the directories that need syncers.
        for stream in l.streams_by_id().values() {
            if stream.config.mode != db::json::STREAM_MODE_RECORD {
                continue;
            }
            if let Some(id) = stream.sample_file_dir_id {
                dirs.entry(id).or_insert_with(|| {
                    let d = l.sample_file_dirs_by_id().get(&id).unwrap();
                    info!("Starting syncer for path {}", d.path);
                    d.get().unwrap()
                });
            } else {
                warn!(
                    "Stream {} set to record but has no sample file dir id",
                    stream.id
                );
            }
        }

        // Then, with the lock dropped, create syncers.
        drop(l);
        let mut syncers = FnvHashMap::with_capacity_and_hasher(dirs.len(), Default::default());
        for (id, dir) in dirs.drain() {
            let (channel, join) = writer::start_syncer(db.clone(), id)?;
            syncers.insert(id, Syncer { dir, channel, join });
        }

        // Then start up streams.
        let handle = tokio::runtime::Handle::current();
        let l = db.lock();
        for (i, (id, stream)) in l.streams_by_id().iter().enumerate() {
            if stream.config.mode != db::json::STREAM_MODE_RECORD {
                continue;
            }
            let camera = l.cameras_by_id().get(&stream.camera_id).unwrap();
            let sample_file_dir_id = match stream.sample_file_dir_id {
                Some(s) => s,
                None => {
                    warn!(
                        "Can't record stream {} ({}/{}) because it has no sample file dir",
                        id,
                        camera.short_name,
                        stream.type_.as_str()
                    );
                    continue;
                }
            };
            let rotate_offset_sec = streamer::ROTATE_INTERVAL_SEC * i as i64 / streams as i64;
            let syncer = syncers.get(&sample_file_dir_id).unwrap();
            let mut streamer = streamer::Streamer::new(
                &env,
                syncer.dir.clone(),
                syncer.channel.clone(),
                *id,
                camera,
                stream,
                rotate_offset_sec,
                streamer::ROTATE_INTERVAL_SEC,
            )?;
            info!("Starting streamer for {}", streamer.short_name());
            let name = format!("s-{}", streamer.short_name());
            let handle = handle.clone();
            streamers.push(
                thread::Builder::new()
                    .name(name)
                    .spawn(move || {
                        let _enter = handle.enter();
                        streamer.run();
                    })
                    .expect("can't create thread"),
            );
        }
        drop(l);
        Some(syncers)
    } else {
        None
    };

    // Start the web interface.
    let make_svc = make_service_fn(move |_conn| {
        futures::future::ok::<_, std::convert::Infallible>(service_fn({
            let svc = Arc::clone(&svc);
            move |req| Arc::clone(&svc).serve(req)
        }))
    });
    let server = ::hyper::Server::try_bind(&args.http_addr)
        .with_context(|_| format!("unable to bind --http-addr={}", &args.http_addr))?
        .tcp_nodelay(true)
        .serve(make_svc);

    let mut int = signal(SignalKind::interrupt())?;
    let mut term = signal(SignalKind::terminate())?;
    let shutdown = futures::future::select(Box::pin(int.recv()), Box::pin(term.recv()));

    let (shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel();
    let server = server.with_graceful_shutdown(shutdown_rx.map(|_| ()));
    let server_handle = tokio::spawn(server);

    info!("Ready to serve HTTP requests");
    shutdown.await;
    shutdown_tx.send(()).unwrap();

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

    db.lock().clear_watches();

    info!("Waiting for HTTP requests to finish.");
    server_handle.await??;
    info!("Exiting.");
    Ok(0)
}
