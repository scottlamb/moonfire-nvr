// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

//! Sample file directory management.
//!
//! This mostly includes opening a directory and looking for recordings within it.
//! Updates to the directory happen through [crate::writer].

pub mod reader;
pub mod scan;
pub mod writer;

use crate::db::CompositeId;
use crate::schema;
use crate::{coding, fs};
use base::{bail, err, Error};
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{FutureExt as _, TryFutureExt as _};
use nix::{
    fcntl::{FlockArg, OFlag},
    sys::stat::Mode,
    NixPath,
};
use protobuf::{Message, MessageField};
use reader::get_page_mask;
use std::collections::VecDeque;
use std::ffi::CStr;
use std::future::Future;
use std::io::{Read, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use tracing::{error, info_span};
use uuid::Uuid;

pub use writer::WriteStream;

/// The fixed length of a directory's `meta` file.
///
/// See `DirMeta` comments within `proto/schema.proto` for more explanation.
const FIXED_DIR_META_LEN: usize = 512;

#[derive(Debug)]
pub(crate) struct Config {
    pub path: PathBuf,
    pub db_uuid: Uuid,
    pub dir_uuid: Uuid,

    /// The last complete open; this should be specified unless the directory is
    /// being created.
    pub last_complete_open: Option<crate::db::Open>,

    /// The current open, iff the database is open for writing.
    pub current_open: Option<crate::db::Open>,
}

impl Config {
    /// Checks that the config and existing metadata are consistent; the
    /// directory is then openable.
    pub(crate) fn check_consistent(&self, actual_meta: &schema::DirMeta) -> Result<(), Error> {
        self.check_consistent_helper(actual_meta).map_err(|msg| {
            err!(
                FailedPrecondition,
                msg("{msg}\n\nconfig: {self:?}\nmeta: {actual_meta:?}"),
            )
            .build()
        })
    }

    fn check_consistent_helper(&self, actual_meta: &schema::DirMeta) -> Result<(), String> {
        if let Some(o) = self.last_complete_open.as_ref() {
            // If we're expecting the database has ever been completely opened,
            // the directory's metadata must reflect this. It can still say
            // that it was "partially opened" at that version, because updating
            // the database and directory is not atomic.
            if actual_meta.db_uuid != self.db_uuid.as_bytes() {
                return Err("db uuid mismatch".into());
            }
            if actual_meta.dir_uuid != self.dir_uuid.as_bytes() {
                return Err("dir uuid mismatch".into());
            }
            if !matches!(actual_meta.last_complete_open.as_ref(), Some(o2) if o.matches(o2))
                && !matches!(actual_meta.in_progress_open.as_ref(), Some(o2) if o.matches(o2))
            {
                return Err(format!(
                    "not at expected last open {:?}",
                    &self.last_complete_open
                ));
            }
        } else if actual_meta.last_complete_open.is_some() {
            return Err("unexpectedly opened".into());
        }

        Ok(())
    }
}

/// Handle to a sample file directory pool. Typically one pool per physical disk drive.
///
/// If the directory is used for writing, [`crate::writer::start_syncer`] should be
/// called to start a background thread. This thread manages deleting files and
/// writing new files. It synces the directory and commits these operations to
/// the database in the correct order to maintain the invariants described in
/// `design/schema.md`.
#[derive(Clone)]
pub struct Pool(Arc<Shared>);

/// State shared between handles and workers.
struct Shared {
    config: Config,

    /// Notifies workers that work is available.
    worker_notify: std::sync::Condvar,

    inner: Mutex<Inner>,
}

/// Mutable state shared between handles and workers.
#[derive(Default)]
struct Inner {
    state: State,

    /// The number of currently spawned workers, which may be idle.
    live_workers: usize,

    /// The number of workers actually doing work.
    active_workers: usize,

    /// The number of active write streams. Write streams are always closed by
    /// workers to prevent blocking other threads. To allow this, worker threads
    /// are never shut down while there are active write streams.
    write_streams: usize,

    /// The work to be performed.
    work: VecDeque<IoCommand>,
}

struct Worker {
    dir: fs::Dir,
    shared: Arc<Shared>,
    page_mask: usize,
}

/// Command to be performed by a worker thread.
///
/// Arbitrary operations can be performed via the `Run` variant. The writer
/// and reader hot paths have their own enum variants to avoid the
/// allocation/dispatch overhead of `Box<dyn FnOnce>`.
enum IoCommand {
    CollectGarbage {
        span: tracing::Span,
        garbage: Vec<CompositeId>,
        tx: tokio::sync::oneshot::Sender<Result<Vec<CompositeId>, Error>>,
    },

    Run(Box<dyn FnOnce(WorkerCtx<'_>) + Send + 'static>),

    // writer.rs
    CreateFile {
        span: tracing::Span,
        composite_id: CompositeId,
        tx: tokio::sync::oneshot::Sender<Result<writer::WriteStream, Error>>,
    },
    Write {
        span: tracing::Span,
        file: std::fs::File,
        data: Bytes,
        tx: tokio::sync::oneshot::Sender<(std::fs::File, Bytes, Result<usize, std::io::Error>)>,
    },
    Abandon {
        file: std::fs::File,
    },
    SyncAll {
        span: tracing::Span,
        file: std::fs::File,
        tx: tokio::sync::oneshot::Sender<(std::fs::File, Result<(), std::io::Error>)>,
    },

    // reader.rs
    /// Opens a file and reads the first chunk.
    OpenForReading {
        span: tracing::Span,
        composite_id: CompositeId,
        range: std::ops::Range<u64>,
        reply_tx: reader::Sender,
    },
    /// Reads the next chunk of the file.
    ReadNextChunk {
        file: reader::OpenReader,
    },
}

#[derive(Debug, Default)]
pub(crate) enum State {
    /// Closed. No workers are running, no file descriptor is open, and thus no lock.
    #[default]
    Closed,

    /// Transitioning from `Closed` to `OpenStage1` or `Open`. See [`Pool::open`].
    OpeningStage1 {
        completion: futures::future::Shared<futures::future::BoxFuture<'static, Result<(), Error>>>,
    },

    /// Partially opened for writing. Workers are running; file descriptor is open; locked exclusively;
    /// metadata reflects an in-progress open.
    OpenStage1,

    /// Transitioning from `OpenStage1` to `Open`; see [`Pool::complete_open_for_write`].
    OpeningStage2,

    /// Fully open for either reading or writing. Workers are running; file descriptor is open; locked in
    /// the appropriate mode.
    Open,

    /// Transitioning from `Open` to `Closed` with a metadata update between guaranteeing the directory is empty
    /// (last_completed_open is None).
    Deleting,

    /// Transitioning to state `Closed`.
    Closing {
        /// All active waiters will be notified when all workers have completed.
        done: Arc<tokio::sync::Notify>,
    },
}

/// The on-disk filename of a recording file within the sample file directory.
/// This is the [`CompositeId`](crate::db::CompositeId) as 16 hexadigits. It's
/// null-terminated so it can be passed to system calls without copying.
pub(crate) struct CompositeIdPath([u8; 17]);

impl CompositeIdPath {
    pub(crate) fn from(id: CompositeId) -> Self {
        let mut buf = [0u8; 17];
        write!(&mut buf[..16], "{:016x}", id.0).expect("can't format id to pathname buf");
        CompositeIdPath(buf)
    }
}

impl NixPath for CompositeIdPath {
    fn is_empty(&self) -> bool {
        false
    }
    fn len(&self) -> usize {
        16
    }

    fn with_nix_path<T, F>(&self, f: F) -> Result<T, nix::Error>
    where
        F: FnOnce(&CStr) -> T,
    {
        let p = CStr::from_bytes_with_nul(&self.0[..]).expect("no interior nuls");
        Ok(f(p))
    }
}

/// Reads `dir`'s metadata. If none is found, returns an empty proto.
pub(crate) fn read_meta(dir: &fs::Dir) -> Result<schema::DirMeta, Error> {
    let mut meta = schema::DirMeta::default();
    let mut f = match fs::openat(dir.0, c"meta", OFlag::O_RDONLY, Mode::empty()) {
        Err(e) => {
            if e == nix::Error::ENOENT {
                return Ok(meta);
            }
            return Err(e.into());
        }
        Ok(f) => f,
    };
    let mut data = Vec::new();
    f.read_to_end(&mut data)?;
    let (len, pos) = coding::decode_varint32(&data, 0)
        .map_err(|_| err!(DataLoss, msg("Unable to decode varint length in meta file")))?;
    if data.len() != FIXED_DIR_META_LEN || len as usize + pos > FIXED_DIR_META_LEN {
        bail!(
            DataLoss,
            msg(
                "Expected a {}-byte file with a varint length of a DirMeta message; got \
                a {}-byte file with length {}",
                FIXED_DIR_META_LEN,
                data.len(),
                len,
            ),
        );
    }
    let data = &data[pos..pos + len as usize];
    let mut s = protobuf::CodedInputStream::from_bytes(data);
    meta.merge_from(&mut s)
        .map_err(|e| err!(DataLoss, msg("Unable to parse metadata proto"), source(e)))?;
    Ok(meta)
}

/// Writes `dirfd`'s metadata, clobbering existing data.
///
/// This is used both by the pool and by the upgrade code.
pub(crate) fn write_meta(dir: &fs::Dir, meta: &schema::DirMeta) -> Result<(), Error> {
    let mut data = meta
        .write_length_delimited_to_bytes()
        .expect("proto3->vec is infallible");
    if data.len() > FIXED_DIR_META_LEN {
        bail!(
            Internal,
            msg(
                "length-delimited DirMeta message requires {} bytes, over limit of {}",
                data.len(),
                FIXED_DIR_META_LEN,
            ),
        );
    }
    data.resize(FIXED_DIR_META_LEN, 0); // pad to required length.
    let mut f = fs::openat(
        dir.0,
        c"meta",
        OFlag::O_CREAT | OFlag::O_WRONLY,
        Mode::S_IRUSR | Mode::S_IWUSR,
    )
    .map_err(|e| err!(e, msg("unable to open meta file")))?;
    let stat = f
        .metadata()
        .map_err(|e| err!(e, msg("unable to stat meta file")))?;
    if stat.len() == 0 {
        // Need to sync not only the data but also the file metadata and dirent.
        f.write_all(&data)
            .map_err(|e| err!(e, msg("unable to write to meta file")))?;
        f.sync_all()
            .map_err(|e| err!(e, msg("unable to sync meta file")))?;
        nix::unistd::fsync(dir.0).map_err(|e| err!(e, msg("unable to sync dir")))?;
    } else if stat.len() == FIXED_DIR_META_LEN as u64 {
        // Just syncing the data will suffice; existing metadata and dirent are fine.
        f.write_all(&data)
            .map_err(|e| err!(e, msg("unable to write to meta file")))?;
        f.sync_data()
            .map_err(|e| err!(e, msg("unable to sync meta file")))?;
    } else {
        bail!(
            DataLoss,
            msg(
                "existing meta file is {}-byte; expected {}",
                stat.len(),
                FIXED_DIR_META_LEN,
            ),
        );
    }
    Ok(())
}

pub(crate) fn is_open(state: &State) -> bool {
    matches!(state, State::Open)
}

impl Pool {
    /// Returns a closed `Pool` for the given configuration.
    pub(crate) fn new(config: Config) -> Self {
        Self(Arc::new(Shared {
            config,
            worker_notify: Condvar::new(),
            inner: Mutex::new(Inner {
                state: State::Closed,
                live_workers: 0,
                active_workers: 0,
                work: VecDeque::new(),
                write_streams: 0,
            }),
        }))
    }

    pub(crate) fn config(&self) -> &Config {
        &self.0.config
    }

    pub fn path(&self) -> &Path {
        &self.0.config.path
    }

    /// Opens the directory, partially or completely.
    ///
    /// If the database is open for reading only
    /// (`config.current_open.is_none()`), on success the directory will be in
    /// state `Open`. If the database is open for writing, a previously closed
    /// directory will be in state `OpenStage1`. A directory that was already
    /// partially open could remain in state `OpeningStage2` or reach state
    /// `Open`.
    pub(crate) fn open(&self, workers: NonZeroUsize) -> BoxFuture<'static, Result<(), Error>> {
        if self.0.config.last_complete_open.is_none() && self.0.config.current_open.is_none() {
            return futures::future::err(
                err!(
                    FailedPrecondition,
                    msg("can't create a directory in read-only mode")
                )
                .build(),
            )
            .boxed();
        }
        let mut l = self.0.inner.lock().expect("dir is not poisoned");
        match &l.state {
            State::Closed => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let completion = async move { rx.await.expect("worker should send completion") }
                    .boxed()
                    .shared();
                l.state = State::OpeningStage1 {
                    completion: completion.clone(),
                };
                l.live_workers = workers.get();
                drop(l);
                let shared = self.0.clone();

                // Start the workers. To make CPU flame graphs easier to read, start them all with
                // exactly the same stack including the outer closure, using a `LazyLock` to make
                // one of them handle the startup stuff.
                let lazy = Arc::new(std::sync::LazyLock::new(|| Worker::create(shared, tx)));
                for _ in 0..workers.get() {
                    let lazy = lazy.clone();
                    std::thread::Builder::new()
                        .name(format!("dir-{}", self.0.config.path.display()))
                        .spawn(move || Worker::run(lazy))
                        .expect("spawning a thread should succeed");
                }
                completion.boxed()
            }
            State::OpeningStage1 { completion, .. } => completion.clone().boxed(),
            State::OpenStage1 | State::OpeningStage2 | State::Open => {
                futures::future::ok(()).boxed()
            }
            State::Deleting => {
                futures::future::err(err!(FailedPrecondition, msg("directory is deleting")).build())
                    .boxed()
            }
            State::Closing { .. } => {
                futures::future::err(err!(FailedPrecondition, msg("directory is closing")).build())
                    .boxed()
            }
        }
    }

    /// Transitions from state `OpenStage1` to `Open`.
    pub(crate) fn complete_open_for_write(&self) -> BoxFuture<'static, Result<(), Error>> {
        let Some(open) = &self.0.config.current_open else {
            return futures::future::err(err!(FailedPrecondition, msg("read-only")).build())
                .boxed();
        };
        let mut l = self.0.inner.lock().expect("dir is not poisoned");
        match &mut l.state {
            State::OpenStage1 => {}
            State::Open => return futures::future::ok(()).boxed(),
            o => {
                return futures::future::err(
                    err!(
                        FailedPrecondition,
                        msg("directory is in unexpected state {o:?}")
                    )
                    .build(),
                )
                .boxed();
            }
        };
        l.state = State::OpeningStage2;
        drop(l);
        let mut meta = schema::DirMeta::new();
        meta.db_uuid
            .extend_from_slice(self.0.config.db_uuid.as_bytes());
        meta.dir_uuid
            .extend_from_slice(self.0.config.dir_uuid.as_bytes());
        let o = meta.in_progress_open.mut_or_insert_default();
        o.id = open.id;
        o.uuid.extend_from_slice(open.uuid.as_bytes());
        self.run_inner(
            "open_stage2",
            |s| matches!(s, State::OpeningStage2),
            move |ctx| {
                write_meta(&ctx.0.dir, &meta)?;
                let mut l = ctx.0.shared.inner.lock().expect("dir is not poisoned");
                assert!(matches!(l.state, State::OpeningStage2));
                l.state = State::Open;
                Ok(())
            },
        )
        .boxed()
    }

    /// Transitions to state `Closed`.
    ///
    /// Must be in one of: `OpenStage1`, `Open`, `Deleting`, `Closing`, `Closed.`
    pub(crate) async fn close(&self) -> Result<(), Error> {
        let done;
        let done_notify = {
            let mut l = self.0.inner.lock().expect("dir is not poisoned");
            done = match &mut l.state {
                State::OpenStage1 | State::Open => {
                    let done = Arc::new(tokio::sync::Notify::new());
                    l.state = State::Closing { done: done.clone() };
                    if l.write_streams == 0 {
                        self.0.worker_notify.notify_all();
                    }
                    done
                }
                State::Closing { done } => done.clone(),
                o @ State::OpeningStage1 { .. }
                | o @ State::OpeningStage2
                | o @ State::Deleting => {
                    return Err(err!(
                        FailedPrecondition,
                        msg("directory is in unexpected state {o:?}")
                    )
                    .build());
                }
                State::Closed => return Ok(()),
            };

            // Because `Notify::notify_waiters` only notifies *current* waiter futures,
            // create said future *before* dropping the lock and allowing the
            // workers to proceed.
            done.notified()
        };
        done_notify.await;
        Ok(())
    }

    /// Marks the directory as deleted.
    ///
    /// Fails immediately unless open for writing and quiescent (no queued/active commands or write streams).
    /// The returned future may fail later due to IO error. Regardless of the outcome, or if
    /// the returned future is dropped, the directory should eventually reach state `OpenStage1` on
    /// success or `Open` on failure.
    pub(crate) fn mark_deleted(&self) -> Result<impl Future<Output = Result<(), Error>>, Error> {
        let Some(open) = self.0.config.current_open else {
            bail!(
                FailedPrecondition,
                msg(
                    "can only delete directory {} when database is open for write",
                    self.0.config.path.display(),
                )
            );
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let span = info_span!("run", operation_name = "mark_deleted");
        let inner = move |ctx: WorkerCtx<'_>| {
            if !ctx.is_empty()? {
                bail!(
                    FailedPrecondition,
                    msg(
                        "sample file directory {} is not empty",
                        ctx.0.shared.config.path.display(),
                    )
                );
            }
            let mut meta = schema::DirMeta::new();
            meta.db_uuid
                .extend_from_slice(ctx.0.shared.config.db_uuid.as_bytes());
            meta.dir_uuid
                .extend_from_slice(ctx.0.shared.config.dir_uuid.as_bytes());
            let o = meta.last_complete_open.mut_or_insert_default();
            o.uuid = open.uuid.as_bytes().into();
            o.id = open.id;
            write_meta(&ctx.0.dir, &meta)
        };
        let f = move |ctx: WorkerCtx<'_>| {
            let _enter = span.enter();
            let result = inner(ctx);
            {
                let mut l = ctx.0.shared.inner.lock().expect("not poisoned");
                assert!(matches!(l.state, State::Deleting));
                l.state = match result {
                    Ok(_) => State::OpenStage1,
                    Err(_) => State::Open,
                };
            }
            let _ = tx.send(result);
        };
        {
            let mut l = self.0.inner.lock().expect("not poisoned");
            let State::Open = l.state else {
                bail!(
                    FailedPrecondition,
                    msg(
                        "can only delete directory {} when open for write",
                        self.0.config.path.display(),
                    )
                );
            };
            if !l.work.is_empty() || l.active_workers != 0 || l.write_streams > 0 {
                bail!(
                    FailedPrecondition,
                    msg(
                        "can only delete directory {} when quiescent",
                        self.0.config.path.display(),
                    )
                );
            }
            l.state = State::Deleting;
            l.work.push_back(IoCommand::Run(Box::new(f)));
        }
        use futures::FutureExt as _;
        Ok(
            rx.map(|r: Result<_, tokio::sync::oneshot::error::RecvError>| {
                r.expect("worker should not panic")
            }),
        )
    }

    /// Unlinks the given recordings, which have been marked as deleted in the database.
    ///
    /// Returns the ones that were successfully unlinked.
    pub fn collect_garbage(
        &self,
        garbage: Vec<CompositeId>,
    ) -> impl Future<Output = Result<Vec<CompositeId>, Error>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let r = self.send(
            is_open,
            IoCommand::CollectGarbage {
                span: tracing::info_span!("collect_garbage"),
                garbage,
                tx,
            },
        );
        async {
            r?;
            rx.await
                .unwrap_or_else(|_| panic!("worker should not panic"))
        }
    }

    /// Runs a function, which can batch several operations and return an arbitrary result.
    ///
    /// Fails if the pool is not in state `Open`.
    pub fn run<T: Send + 'static>(
        &self,
        operation_name: &str,
        f: impl FnOnce(WorkerCtx<'_>) -> Result<T, Error> + Send + 'static,
    ) -> impl Future<Output = Result<T, Error>> + 'static {
        self.run_inner(operation_name, is_open, f)
    }

    /// Like [`run`] but supports states other than `Open`.
    fn run_inner<T: Send + 'static>(
        &self,
        operation_name: &str,
        state_fn: impl FnOnce(&State) -> bool,
        f: impl FnOnce(WorkerCtx<'_>) -> Result<T, Error> + Send + 'static,
    ) -> impl Future<Output = Result<T, Error>> + 'static {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let span = info_span!("run", operation_name);
        let f = move |ctx: WorkerCtx<'_>| {
            if tx.is_closed() {
                return;
            }
            let _enter = span.enter();
            let result = f(ctx);
            let _ = tx.send(result);
        };
        let r = self.send(state_fn, IoCommand::Run(Box::new(f)));
        async {
            r?;
            rx.unwrap_or_else(|_| panic!("worker should not panic"))
                .await
        }
    }

    /// Sends a command to a worker.
    fn send(&self, state_okay: impl FnOnce(&State) -> bool, cmd: IoCommand) -> Result<(), Error> {
        let mut l = self.0.inner.lock().expect("not poisoned");
        if !state_okay(&l.state) {
            bail!(
                FailedPrecondition,
                msg("worker in unexpected state {:?}", &l.state),
            );
        }
        l.work.push_back(cmd);
        drop(l);
        self.0.worker_notify.notify_one();
        Ok(())
    }

    pub(crate) fn is_open(&self) -> bool {
        matches!(
            self.0.inner.lock().expect("dir is not poisoned").state,
            State::Open
        )
    }
}

impl Worker {
    /// Opens the directory or fails, reporting to the supplied channel.
    fn create(
        shared: Arc<Shared>,
        tx: tokio::sync::oneshot::Sender<Result<(), Error>>,
    ) -> Result<Self, ()> {
        let dir = match Self::open(&shared) {
            Ok(d) => d,
            Err(e) => {
                let _ = tx.send(Err(e));
                let mut l = shared.inner.lock().expect("dir is not poisoned");
                assert!(matches!(l.state, State::OpeningStage1 { .. }));
                l.state = State::Closed;
                return Err(());
            }
        };
        let _ = tx.send(Ok(()));

        let mut l = shared.inner.lock().expect("dir is not poisoned");
        let State::OpeningStage1 { .. } = l.state else {
            panic!("unexpected state: {:?}", l.state);
        };
        l.state = if shared.config.current_open.is_some() {
            State::OpenStage1
        } else {
            State::Open
        };
        drop(l);
        Ok(Self {
            dir,
            shared,
            page_mask: get_page_mask(),
        })
    }

    fn run<F: FnOnce() -> Result<Self, ()>>(lazy: Arc<std::sync::LazyLock<Result<Self, ()>, F>>) {
        let Ok(self_) = std::sync::LazyLock::force(&lazy) else {
            return;
        };

        let shared = self_.shared.clone();
        let mut active_now = false;
        loop {
            let mut l = shared.inner.lock().expect("not poisoned");
            if active_now {
                l.active_workers = l
                    .active_workers
                    .checked_sub(1)
                    .expect("active count is consistent");
            }
            let cmd = loop {
                let inner = &mut *l;
                let Some(cmd) = inner.work.pop_front() else {
                    if let State::Closing { done } = &mut inner.state {
                        if inner.write_streams > 0 {
                            // Can't shut down the pool until the write streams are closed.
                            continue;
                        }
                        drop(lazy);
                        inner.live_workers = inner
                            .live_workers
                            .checked_sub(1)
                            .expect("live_workers is consistent");
                        if inner.live_workers == 0 {
                            done.notify_waiters();
                            l.state = State::Closed;
                        };
                        return;
                    }
                    l = self_.shared.worker_notify.wait(l).expect("not poisoned");
                    continue;
                };
                inner.active_workers += 1;
                active_now = true;
                break cmd;
            };
            drop(l);
            self_.cmd(cmd);
        }
    }

    fn open(shared: &Arc<Shared>) -> Result<fs::Dir, Error> {
        let create = shared.config.last_complete_open.is_none();
        let read_write = shared.config.current_open.is_some();
        let dir = fs::Dir::open(&shared.config.path, create)?;
        dir.lock(if read_write {
            FlockArg::LockExclusiveNonblock
        } else {
            FlockArg::LockSharedNonblock
        })
        .map_err(|e| {
            err!(
                e,
                msg("unable to lock dir {}", shared.config.path.display())
            )
        })?;
        let dir_meta = read_meta(&dir).map_err(|e| err!(e, msg("unable to read meta file")))?;
        shared.config.check_consistent(&dir_meta)?;
        if create && !Self::is_empty(&dir)? {
            bail!(
                FailedPrecondition,
                msg(
                    "can't create dir at path {} with existing files",
                    shared.config.path.display(),
                ),
            );
        }
        if let Some(o) = shared.config.current_open {
            let mut meta = schema::DirMeta::new();
            meta.db_uuid
                .extend_from_slice(shared.config.db_uuid.as_bytes());
            meta.dir_uuid
                .extend_from_slice(shared.config.dir_uuid.as_bytes());
            meta.in_progress_open = MessageField::some(o.into());
            meta.last_complete_open = shared.config.last_complete_open.map(Into::into).into();
            write_meta(&dir, &meta)?;
        }
        Ok(dir)
    }

    fn cmd(&self, cmd: IoCommand) {
        match cmd {
            IoCommand::Run(f) => f(WorkerCtx(self)),
            IoCommand::CollectGarbage { span, garbage, tx } => {
                self.collect_garbage(span, garbage, tx)
            }
            IoCommand::OpenForReading {
                span,
                composite_id,
                range,
                reply_tx,
            } => self.open_for_reading(span, composite_id, range, reply_tx),
            IoCommand::ReadNextChunk { file } => self.read_chunk(file),
            IoCommand::CreateFile {
                span,
                composite_id,
                tx,
            } => self.create_file(span, composite_id, tx),
            IoCommand::Write {
                span,
                file,
                data,
                tx,
            } => self.write(span, file, data, tx),
            IoCommand::Abandon { file } => drop(file),
            IoCommand::SyncAll { span, file, tx } => self.sync_all(span, file, tx),
        }
    }

    /// Determines if the directory is empty, aside from metadata.
    fn is_empty(dir: &fs::Dir) -> Result<bool, Error> {
        let mut dir = dir.opendir()?;
        for e in dir.iter() {
            let e = e?;
            match e.file_name().to_bytes() {
                b"." | b".." => continue,
                b"meta" => continue, // existing metadata is fine.
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    fn collect_garbage(
        &self,
        span: tracing::Span,
        mut garbage: Vec<CompositeId>,
        tx: tokio::sync::oneshot::Sender<Result<Vec<CompositeId>, Error>>,
    ) {
        if tx.is_closed() {
            return;
        }
        let _enter = span.enter();
        garbage.retain(|&id| {
            match self.unlink(id) {
                Ok(()) | Err(nix::Error::ENOENT) => true,
                Err(err) => {
                    error!(%err, "dir {}: unable to unlink recording {}", self.shared.config.path.display(), id);
                    false
                },
            }
        });
        if !garbage.is_empty() {
            if let Err(err) = nix::unistd::fsync(self.dir.0) {
                let _ = tx.send(Err(err.into()));
                return;
            }
        }
        let _ = tx.send(Ok(garbage));
    }

    fn unlink(&self, id: CompositeId) -> Result<(), nix::Error> {
        nix::unistd::unlinkat(
            Some(self.dir.0),
            &CompositeIdPath::from(id),
            nix::unistd::UnlinkatFlags::NoRemoveDir,
        )
    }
}

#[derive(Copy, Clone)]
pub struct WorkerCtx<'w>(&'w Worker);

impl WorkerCtx<'_> {
    pub fn path(&self) -> &Path {
        &self.0.shared.config.path
    }

    pub fn unlink(&self, id: CompositeId) -> Result<(), Error> {
        self.0.unlink(id).map_err(Into::into)
    }

    pub fn rename<P1: ?Sized + NixPath, P2: ?Sized + NixPath>(
        &self,
        from: &P1,
        to: &P2,
    ) -> Result<(), Error> {
        nix::fcntl::renameat(Some(self.0.dir.0), from, Some(self.0.dir.0), to).map_err(Into::into)
    }

    pub(crate) fn sync(&self) -> Result<(), Error> {
        nix::unistd::fsync(self.0.dir.0).map_err(|e| err!(e, msg("unable to sync dir")).build())
    }

    pub(crate) fn is_empty(&self) -> Result<bool, Error> {
        Worker::is_empty(&self.0.dir)
    }

    /// Returns information about the filesystem on which this directory lives.
    pub fn statfs(&self) -> Result<nix::sys::statvfs::Statvfs, Error> {
        nix::sys::statvfs::fstatvfs(&self.0.dir).map_err(Into::into)
    }
}

/// Parses a composite id filename.
///
/// These are exactly 16 bytes, lowercase hex, as created by [CompositeIdPath].
pub(crate) fn parse_id(id: &[u8]) -> Result<CompositeId, ()> {
    if id.len() != 16 {
        return Err(());
    }
    let mut v: u64 = 0;
    for b in id {
        v = (v << 4)
            | match b {
                b @ b'0'..=b'9' => b - b'0',
                b @ b'a'..=b'f' => b - b'a' + 10,
                _ => return Err(()),
            } as u64;
    }
    Ok(CompositeId(v as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_id() {
        use super::parse_id;
        assert_eq!(parse_id(b"0000000000000000").unwrap().0, 0);
        assert_eq!(parse_id(b"0000000100000002").unwrap().0, 0x0000000100000002);
        parse_id(b"").unwrap_err();
        parse_id(b"meta").unwrap_err();
        parse_id(b"0").unwrap_err();
        parse_id(b"000000010000000x").unwrap_err();
    }

    /// Ensures that a DirMeta with all fields filled fits within the maximum size.
    #[test]
    fn max_len_meta() {
        let mut meta = schema::DirMeta::new();
        let fake_uuid = &[0u8; 16][..];
        meta.db_uuid.extend_from_slice(fake_uuid);
        meta.dir_uuid.extend_from_slice(fake_uuid);
        {
            let o = meta.last_complete_open.mut_or_insert_default();
            o.id = u32::MAX;
            o.uuid.extend_from_slice(fake_uuid);
        }
        {
            let o = meta.in_progress_open.mut_or_insert_default();
            o.id = u32::MAX;
            o.uuid.extend_from_slice(fake_uuid);
        }
        let data = meta
            .write_length_delimited_to_bytes()
            .expect("proto3->vec is infallible");
        assert!(
            data.len() <= FIXED_DIR_META_LEN,
            "{} vs {}",
            data.len(),
            FIXED_DIR_META_LEN
        );
    }
}
