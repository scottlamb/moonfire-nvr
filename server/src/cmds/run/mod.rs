// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2022 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use crate::streamer;
use crate::web;
use crate::web::accept::Listener;
use base::clock;
use db::{dir, writer};
use failure::{bail, Error, ResultExt};
use fnv::FnvHashMap;
use hyper::service::{make_service_fn, service_fn};
use log::error;
use log::{info, warn};
use retina::client::SessionGroup;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use structopt::StructOpt;
use tokio::signal::unix::{signal, SignalKind};

use self::config::ConfigFile;

mod config;

#[derive(StructOpt)]
pub struct Args {
    #[structopt(short, long, default_value = "/etc/moonfire-nvr.toml")]
    config: PathBuf,

    /// Open the database in read-only mode and disables recording.
    ///
    /// Note this is incompatible with session authentication; consider adding
    /// a bind with `allow_unauthenticated_permissions` to your config.
    #[structopt(long)]
    read_only: bool,
}

// These are used in a hack to get the name of the current time zone (e.g. America/Los_Angeles).
// They seem to be correct for Linux and macOS at least.
const LOCALTIME_PATH: &str = "/etc/localtime";
const TIMEZONE_PATH: &str = "/etc/timezone";

// Some well-known zone paths looks like the following:
//   /usr/share/zoneinfo/*          for Linux and macOS < High Sierra
//   /var/db/timezone/zoneinfo/*    for macOS High Sierra
//   /etc/zoneinfo/*                for NixOS
fn zoneinfo_name(path: &str) -> Option<&str> {
    path.rsplit_once("/zoneinfo/").map(|(_, name)| name)
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

        if let Some(p) = zoneinfo_name(p) {
            return Ok(p.to_owned());
        }

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
            if let Some(p) = zoneinfo_name(localtime_dest) {
                return Ok(p.to_owned());
            }
            bail!(
                "Unable to resolve {} symlink destination {} to a timezone.",
                LOCALTIME_PATH,
                &localtime_dest
            );
        }
        Err(e) => {
            use ::std::io::ErrorKind;
            if e.kind() != ErrorKind::NotFound && e.kind() != ErrorKind::InvalidInput {
                bail!("Unable to read {} symlink: {}", LOCALTIME_PATH, e);
            }
        }
    };

    // If `TIMEZONE_PATH` is a file, use its contents as the zone name, trimming whitespace.
    match ::std::fs::read_to_string(TIMEZONE_PATH) {
        Ok(z) => Ok(z.trim().to_owned()),
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

fn read_config(path: &Path) -> Result<ConfigFile, Error> {
    let config = std::fs::read(path)?;
    let config = toml::from_slice(&config)?;
    Ok(config)
}

pub fn run(args: Args) -> Result<i32, Error> {
    let config = read_config(&args.config)
        .with_context(|_| format!("unable to read {}", &args.config.display()))?;

    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    if let Some(worker_threads) = config.worker_threads {
        builder.worker_threads(worker_threads);
    }
    let rt = builder.build()?;
    let r = rt.block_on(async_run(args.read_only, &config));

    // tokio normally waits for all spawned tasks to complete, but:
    // * in the graceful shutdown path, we wait for specific tasks with logging.
    // * in the immediate shutdown path, we don't want to wait.
    rt.shutdown_background();

    r
}

async fn async_run(read_only: bool, config: &ConfigFile) -> Result<i32, Error> {
    let (shutdown_tx, shutdown_rx) = base::shutdown::channel();
    let mut shutdown_tx = Some(shutdown_tx);

    tokio::pin! {
        let int = signal(SignalKind::interrupt())?;
        let term = signal(SignalKind::terminate())?;
        let inner = inner(read_only, config, shutdown_rx);
    }

    tokio::select! {
        _ = int.recv() => {
            info!("Received SIGINT; shutting down gracefully. \
                   Send another SIGINT or SIGTERM to shut down immediately.");
            shutdown_tx.take();
        },
        _ = term.recv() => {
            info!("Received SIGTERM; shutting down gracefully. \
                   Send another SIGINT or SIGTERM to shut down immediately.");
            shutdown_tx.take();
        },
        result = &mut inner => return result,
    }

    tokio::select! {
        _ = int.recv() => bail!("immediate shutdown due to second signal (SIGINT)"),
        _ = term.recv() => bail!("immediate shutdown due to second singal (SIGTERM)"),
        result = &mut inner => result,
    }
}

/// Makes a best-effort attempt to prepare a path for binding as a Unix-domain socket.
///
/// Binding to a Unix-domain socket fails with `EADDRINUSE` if the dirent already exists,
/// and the dirent isn't automatically deleted when the previous server closes. Clean up a
/// previous socket. As a defense against misconfiguration, make sure it actually is
/// a socket first.
///
/// This mechanism is inherently racy, but it's expected that the database has already
/// been locked.
fn prepare_unix_socket(p: &Path) {
    use nix::sys::stat::{stat, SFlag};
    let stat = match stat(p) {
        Err(_) => return,
        Ok(s) => s,
    };
    if !SFlag::from_bits_truncate(stat.st_mode).intersects(SFlag::S_IFSOCK) {
        return;
    }
    let _ = nix::unistd::unlink(p);
}

fn make_listener(addr: &config::AddressConfig) -> Result<Listener, Error> {
    let sa: SocketAddr = match addr {
        config::AddressConfig::Ipv4(a) => (*a).into(),
        config::AddressConfig::Ipv6(a) => (*a).into(),
        config::AddressConfig::Unix(p) => {
            prepare_unix_socket(p);
            return Ok(Listener::Unix(
                tokio::net::UnixListener::bind(p)
                    .with_context(|_| format!("unable bind Unix socket {}", p.display()))?,
            ));
        }
    };

    // Go through std::net::TcpListener to avoid needing async. That's there for DNS resolution,
    // but it's unnecessary when starting from a SocketAddr.
    let listener = std::net::TcpListener::bind(&sa)
        .with_context(|_| format!("unable to bind TCP socket {}", &sa))?;
    listener.set_nonblocking(true)?;
    Ok(Listener::Tcp(tokio::net::TcpListener::from_std(listener)?))
}

async fn inner(
    read_only: bool,
    config: &ConfigFile,
    shutdown_rx: base::shutdown::Receiver,
) -> Result<i32, Error> {
    let clocks = clock::RealClocks {};
    let (_db_dir, conn) = super::open_conn(
        &config.db_dir,
        if read_only {
            super::OpenMode::ReadOnly
        } else {
            super::OpenMode::ReadWrite
        },
    )?;
    let db = Arc::new(db::Database::new(clocks, conn, !read_only)?);
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

    // Start a streamer for each stream.
    let mut streamers = Vec::new();
    let mut session_groups_by_camera: FnvHashMap<i32, Arc<retina::client::SessionGroup>> =
        FnvHashMap::default();
    let syncers = if !read_only {
        let l = db.lock();
        let mut dirs = FnvHashMap::with_capacity_and_hasher(
            l.sample_file_dirs_by_id().len(),
            Default::default(),
        );
        let streams = l.streams_by_id().len();
        let env = streamer::Environment {
            db: &db,
            opener: &crate::stream::OPENER,
            shutdown_rx: &shutdown_rx,
        };

        // Get the directories that need syncers.
        for stream in l.streams_by_id().values() {
            if stream.config.mode != db::json::STREAM_MODE_RECORD {
                continue;
            }
            if let Some(id) = stream.sample_file_dir_id {
                dirs.entry(id).or_insert_with(|| {
                    let d = l.sample_file_dirs_by_id().get(&id).unwrap();
                    info!("Starting syncer for path {}", d.path.display());
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
            let (channel, join) = writer::start_syncer(db.clone(), shutdown_rx.clone(), id)?;
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
            let session_group = session_groups_by_camera
                .entry(camera.id)
                .or_insert_with(|| {
                    Arc::new(SessionGroup::default().named(camera.short_name.clone()))
                })
                .clone();
            let mut streamer = streamer::Streamer::new(
                &env,
                syncer.dir.clone(),
                syncer.channel.clone(),
                *id,
                camera,
                stream,
                session_group,
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

    // Start the web interface(s).
    let own_euid = nix::unistd::Uid::effective();
    let web_handles: Result<Vec<_>, Error> = config
        .binds
        .iter()
        .map(|b| {
            let svc = Arc::new(web::Service::new(web::Config {
                db: db.clone(),
                ui_dir: Some(&config.ui_dir),
                allow_unauthenticated_permissions: b
                    .allow_unauthenticated_permissions
                    .clone()
                    .map(db::Permissions::from),
                trust_forward_hdrs: b.trust_forward_headers,
                time_zone_name: time_zone_name.clone(),
                privileged_unix_uid: b.own_uid_is_privileged.then(|| own_euid),
            })?);
            let make_svc = make_service_fn(move |conn: &crate::web::accept::Conn| {
                let conn_data = *conn.data();
                futures::future::ok::<_, std::convert::Infallible>(service_fn({
                    let svc = Arc::clone(&svc);
                    move |req| Arc::clone(&svc).serve(req, conn_data)
                }))
            });
            let listener = make_listener(&b.address)?;
            let server = ::hyper::Server::builder(listener).serve(make_svc);
            let server = server.with_graceful_shutdown(shutdown_rx.future());
            Ok(tokio::spawn(server))
        })
        .collect();
    let web_handles = web_handles?;

    info!("Ready to serve HTTP requests");
    shutdown_rx.as_future().await;

    info!("Shutting down streamers and syncers.");
    tokio::task::spawn_blocking({
        let db = db.clone();
        move || {
            for streamer in streamers.drain(..) {
                if streamer.join().is_err() {
                    log::error!("streamer panicked; look for previous panic message");
                }
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
        }
    })
    .await?;

    db.lock().clear_watches();

    info!("Waiting for HTTP requests to finish.");
    for h in web_handles {
        h.await??;
    }

    info!("Waiting for TEARDOWN requests to complete.");
    for g in session_groups_by_camera.values() {
        if let Err(e) = g.await_teardown().await {
            error!("{}", e);
        }
    }

    info!("Exiting.");
    Ok(0)
}
