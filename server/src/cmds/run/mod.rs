// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2022 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use crate::streamer;
use crate::web;
use crate::web::accept::Listener;
use base::clock;
use base::err;
use base::FastHashMap;
use base::{bail, Error};
use bpaf::Bpaf;
use db::{dir, writer};
use hyper::service::{make_service_fn, service_fn};
use itertools::Itertools;
use retina::client::SessionGroup;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use tokio::signal::unix::{signal, SignalKind};
use tracing::error;
use tracing::{info, warn};

#[cfg(target_os = "linux")]
use libsystemd::daemon::{notify, NotifyState};

use self::config::ConfigFile;

pub mod config;

/// Runs the server, saving recordings and allowing web access.
#[derive(Bpaf, Debug)]
#[bpaf(command("run"))]
pub struct Args {
    /// Path to configuration file. See `ref/config.md` for config file documentation.
    #[bpaf(short, long, argument("PATH"), fallback("/etc/moonfire-nvr.toml".into()), debug_fallback)]
    config: PathBuf,

    /// Opens the database in read-only mode and disables recording.
    /// Note this is incompatible with session authentication; consider adding
    /// a bind with `allowUnauthenticatedPermissions` to your config.
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
            bail!(
                FailedPrecondition,
                msg("unable to resolve env TZ={tz} to a timezone")
            );
        }
    }

    // If `LOCALTIME_PATH` is a symlink, use that. On some systems, it's instead a copy of the
    // desired timezone, which unfortunately doesn't contain its own name.
    match ::std::fs::read_link(LOCALTIME_PATH) {
        Ok(localtime_dest) => {
            let localtime_dest = match localtime_dest.to_str() {
                Some(d) => d,
                None => bail!(
                    FailedPrecondition,
                    msg("{LOCALTIME_PATH} symlink destination is invalid UTF-8")
                ),
            };
            if let Some(p) = zoneinfo_name(localtime_dest) {
                return Ok(p.to_owned());
            }
            bail!(
                FailedPrecondition,
                msg("unable to resolve {LOCALTIME_PATH} symlink destination {localtime_dest} to a timezone"),
            );
        }
        Err(e) => {
            use ::std::io::ErrorKind;
            if e.kind() != ErrorKind::NotFound && e.kind() != ErrorKind::InvalidInput {
                bail!(e, msg("unable to read {LOCALTIME_PATH} symlink"));
            }
        }
    };

    // If `TIMEZONE_PATH` is a file, use its contents as the zone name, trimming whitespace.
    match ::std::fs::read_to_string(TIMEZONE_PATH) {
        Ok(z) => Ok(z.trim().to_owned()),
        Err(e) => {
            bail!(
                e,
                msg("unable to resolve timezone from TZ env, {LOCALTIME_PATH}, or {TIMEZONE_PATH}"),
            );
        }
    }
}

struct Syncer {
    dir: Arc<dir::SampleFileDir>,
    channel: writer::SyncerChannel<::std::fs::File>,
    join: thread::JoinHandle<()>,
}

#[cfg(target_os = "linux")]
fn get_preopened_sockets() -> Result<FastHashMap<String, Listener>, Error> {
    use libsystemd::activation::IsType as _;
    use std::os::fd::{FromRawFd, IntoRawFd};

    // `receive_descriptors_with_names` errors out if not running under systemd or not using socket
    // activation.
    if std::env::var_os("LISTEN_FDS").is_none() {
        info!("no LISTEN_FDs");
        return Ok(FastHashMap::default());
    }

    let sockets = libsystemd::activation::receive_descriptors_with_names(false)
        .map_err(|e| err!(Unknown, source(e), msg("unable to receive systemd sockets")))?;
    sockets
        .into_iter()
        .map(|(fd, name)| {
            if fd.is_unix() {
                // SAFETY: yes, it's a socket we own.
                let l = unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd.into_raw_fd()) };
                l.set_nonblocking(true)?;
                Ok(Some((
                    name,
                    Listener::Unix(tokio::net::UnixListener::from_std(l)?),
                )))
            } else if fd.is_inet() {
                // SAFETY: yes, it's a socket we own.
                let l = unsafe { std::net::TcpListener::from_raw_fd(fd.into_raw_fd()) };
                l.set_nonblocking(true)?;
                Ok(Some((
                    name,
                    Listener::Tcp(tokio::net::TcpListener::from_std(l)?),
                )))
            } else {
                warn!("ignoring systemd socket {name:?} which is not unix or inet");
                Ok(None)
            }
        })
        .filter_map(Result::transpose)
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn get_preopened_sockets() -> Result<FastHashMap<String, Listener>, Error> {
    Ok(FastHashMap::default())
}

fn read_config(path: &Path) -> Result<ConfigFile, Error> {
    let config = std::fs::read(path)?;
    let config = std::str::from_utf8(&config).map_err(|e| err!(InvalidArgument, source(e)))?;
    let config = toml::from_str(config).map_err(|e| err!(InvalidArgument, source(e)))?;
    Ok(config)
}

pub fn run(args: Args) -> Result<i32, Error> {
    let config = read_config(&args.config).map_err(|e| {
        err!(
            e,
            msg(
                "unable to load config file {}; see documentation in ref/config.md",
                &args.config.display(),
            ),
        )
    })?;

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
        _ = int.recv() => bail!(Cancelled, msg("immediate shutdown due to second signal (SIGINT)")),
        _ = term.recv() => bail!(Cancelled, msg("immediate shutdown due to second singal (SIGTERM)")),
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

fn make_listener(
    addr: &config::AddressConfig,
    #[cfg_attr(not(target_os = "linux"), allow(unused))] preopened: &mut FastHashMap<
        String,
        Listener,
    >,
) -> Result<Listener, Error> {
    let sa: SocketAddr = match addr {
        config::AddressConfig::Ipv4(a) => (*a).into(),
        config::AddressConfig::Ipv6(a) => (*a).into(),
        config::AddressConfig::Unix(p) => {
            prepare_unix_socket(p);
            return Ok(Listener::Unix(tokio::net::UnixListener::bind(p).map_err(
                |e| err!(e, msg("unable bind Unix socket {}", p.display())),
            )?));
        }
        #[cfg(target_os = "linux")]
        config::AddressConfig::Systemd(n) => {
            return preopened.remove(n).ok_or_else(|| {
                err!(
                    NotFound,
                    msg(
                        "can't find systemd socket named {}; available sockets are: {}",
                        n,
                        preopened.keys().join(", ")
                    )
                )
            });
        }
        #[cfg(not(target_os = "linux"))]
        config::AddressConfig::Systemd(_) => {
            bail!(Unimplemented, msg("systemd sockets are Linux-only"))
        }
    };

    // Go through std::net::TcpListener to avoid needing async. That's there for DNS resolution,
    // but it's unnecessary when starting from a SocketAddr.
    let listener = std::net::TcpListener::bind(sa)
        .map_err(|e| err!(e, msg("unable to bind TCP socket {sa}")))?;
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
    let mut session_groups_by_camera: FastHashMap<i32, Arc<retina::client::SessionGroup>> =
        FastHashMap::default();
    let syncers = if !read_only {
        let l = db.lock();
        let mut dirs = FastHashMap::with_capacity_and_hasher(
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
        let mut syncers = FastHashMap::with_capacity_and_hasher(dirs.len(), Default::default());
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
            let span = tracing::info_span!("streamer", stream = streamer.short_name());
            let thread_name = format!("s-{}", streamer.short_name());
            let handle = handle.clone();
            streamers.push(
                thread::Builder::new()
                    .name(thread_name)
                    .spawn(move || {
                        span.in_scope(|| {
                            let _enter_tokio = handle.enter();
                            info!("starting");
                            streamer.run();
                        })
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
    let mut preopened = get_preopened_sockets()?;
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
                privileged_unix_uid: b.own_uid_is_privileged.then_some(own_euid),
            })?);
            let make_svc = make_service_fn(move |conn: &crate::web::accept::Conn| {
                let conn_data = *conn.data();
                futures::future::ok::<_, std::convert::Infallible>(service_fn({
                    let svc = Arc::clone(&svc);
                    move |req| Arc::clone(&svc).serve(req, conn_data)
                }))
            });
            let listener = make_listener(&b.address, &mut preopened)?;
            let server = ::hyper::Server::builder(listener).serve(make_svc);
            let server = server.with_graceful_shutdown(shutdown_rx.future());
            Ok(tokio::spawn(server))
        })
        .collect();
    let web_handles = web_handles?;
    if !preopened.is_empty() {
        warn!(
            "ignoring systemd sockets not referenced in config: {}",
            preopened.keys().join(", ")
        );
    }

    #[cfg(target_os = "linux")]
    {
        if let Err(err) = notify(false, &[NotifyState::Ready]) {
            tracing::warn!(%err, "unable to notify systemd on ready");
        }
    }

    info!("Ready to serve HTTP requests");
    shutdown_rx.as_future().await;

    #[cfg(target_os = "linux")]
    {
        if let Err(err) = notify(false, &[NotifyState::Stopping]) {
            tracing::warn!(%err, "unable to notify systemd on stopping");
        }
    }

    info!("Shutting down streamers and syncers.");
    tokio::task::spawn_blocking({
        let db = db.clone();
        move || {
            for streamer in streamers.drain(..) {
                if streamer.join().is_err() {
                    tracing::error!("streamer panicked; look for previous panic message");
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
    .await
    .map_err(|e| err!(Unknown, source(e)))?;

    info!("Waiting for HTTP requests to finish.");
    for h in web_handles {
        h.await
            .map_err(|e| err!(Unknown, source(e)))?
            .map_err(|e| err!(Unknown, source(e)))?;
    }

    info!("Waiting for TEARDOWN requests to complete.");
    for g in session_groups_by_camera.values() {
        if let Err(err) = g.await_teardown().await {
            error!(%err, "teardown failed");
        }
    }

    info!("Exiting.");
    Ok(0)
}
