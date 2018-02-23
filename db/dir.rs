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

//! Sample file directory management.
//!
//! This includes opening files for serving, rotating away old files, and saving new files.

use db::{self, CompositeId};
use failure::{Error, Fail};
use fnv::FnvHashMap;
use libc::{self, c_char};
use parking_lot::Mutex;
use protobuf::{self, Message};
use recording;
use openssl::hash;
use schema;
use std::cmp;
use std::ffi;
use std::fs;
use std::io::{self, Read, Write};
use std::mem;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::FromRawFd;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

/// A sample file directory. Typically one per physical disk drive.
///
/// If the directory is used for writing, the `start_syncer` function should be called to start
/// a background thread. This thread manages deleting files and writing new files. It synces the
/// directory and commits these operations to the database in the correct order to maintain the
/// invariants described in `design/schema.md`.
#[derive(Debug)]
pub struct SampleFileDir {
    /// The open file descriptor for the directory. The worker uses it to create files and sync the
    /// directory. Other threads use it to open sample files for reading during video serving.
    fd: Fd,
}

/// A file descriptor associated with a directory (not necessarily the sample file dir).
#[derive(Debug)]
pub struct Fd(libc::c_int);

impl Drop for Fd {
    fn drop(&mut self) {
        if unsafe { libc::close(self.0) } < 0 {
            let e = io::Error::last_os_error();
            warn!("Unable to close sample file dir: {}", e);
        }
    }
}

impl Fd {
    /// Opens the given path as a directory.
    pub fn open(fd: Option<&Fd>, path: &str, mkdir: bool) -> Result<Fd, io::Error> {
        let fd = fd.map(|fd| fd.0).unwrap_or(libc::AT_FDCWD);
        let cstring = ffi::CString::new(path)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        if mkdir && unsafe { libc::mkdirat(fd, cstring.as_ptr(), 0o700) } != 0 {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::AlreadyExists {
                return Err(e.into());
            }
        }
        let fd = unsafe { libc::openat(fd, cstring.as_ptr(), libc::O_DIRECTORY | libc::O_RDONLY,
                                       0) };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Fd(fd))
    }

    /// Opens a sample file within this directory with the given flags and (if creating) mode.
    unsafe fn openat(&self, p: *const c_char, flags: libc::c_int, mode: libc::c_int)
                     -> Result<fs::File, io::Error> {
        let fd = libc::openat(self.0, p, flags, mode);
        if fd < 0 {
            return Err(io::Error::last_os_error())
        }
        Ok(fs::File::from_raw_fd(fd))
    }

    /// Locks the directory with the specified `flock` operation.
    pub fn lock(&self, operation: libc::c_int) -> Result<(), io::Error> {
        let ret = unsafe { libc::flock(self.0, operation) };
        if ret < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }

    pub fn statfs(&self) -> Result<libc::statvfs, io::Error> {
        unsafe {
            let mut stat: libc::statvfs = mem::zeroed();
            if libc::fstatvfs(self.0, &mut stat) < 0 {
                return Err(io::Error::last_os_error())
            }
            Ok(stat)
        }
    }
}

pub unsafe fn renameat(from_fd: &Fd, from_path: *const c_char,
                   to_fd: &Fd, to_path: *const c_char) -> Result<(), io::Error> {
    let result = libc::renameat(from_fd.0, from_path, to_fd.0, to_path);
    if result < 0 {
        return Err(io::Error::last_os_error())
    }
    Ok(())
}

impl SampleFileDir {
    /// Opens the directory using the given metadata.
    ///
    /// `db_meta.in_progress_open` should be filled if the directory should be opened in read/write
    /// mode; absent in read-only mode.
    pub fn open(path: &str, db_meta: &schema::DirMeta)
                -> Result<Arc<SampleFileDir>, Error> {
        let read_write = db_meta.in_progress_open.is_some();
        let s = SampleFileDir::open_self(path, false)?;
        s.fd.lock(if read_write { libc::LOCK_EX } else { libc::LOCK_SH } | libc::LOCK_NB)?;
        let dir_meta = s.read_meta()?;
        if !SampleFileDir::consistent(db_meta, &dir_meta) {
            bail!("metadata mismatch.\ndb: {:#?}\ndir: {:#?}", db_meta, &dir_meta);
        }
        if db_meta.in_progress_open.is_some() {
            s.write_meta(db_meta)?;
        }
        Ok(s)
    }

    /// Returns true if the existing directory and database metadata are consistent; the directory
    /// is then openable.
    fn consistent(db_meta: &schema::DirMeta, dir_meta: &schema::DirMeta) -> bool {
        if dir_meta.db_uuid != db_meta.db_uuid { return false; }
        if dir_meta.dir_uuid != db_meta.dir_uuid { return false; }

        if db_meta.last_complete_open.is_some() &&
           (db_meta.last_complete_open != dir_meta.last_complete_open &&
            db_meta.last_complete_open != dir_meta.in_progress_open) {
            return false;
        }

        if db_meta.last_complete_open.is_none() && dir_meta.last_complete_open.is_some() {
            return false;
        }

        true
    }

    pub fn create(path: &str, db_meta: &schema::DirMeta) -> Result<Arc<SampleFileDir>, Error> {
        let s = SampleFileDir::open_self(path, true)?;
        s.fd.lock(libc::LOCK_EX | libc::LOCK_NB)?;
        let old_meta = s.read_meta()?;

        // Verify metadata. We only care that it hasn't been completely opened.
        // Partial opening by this or another database is fine; we won't overwrite anything.
        // TODO: consider one exception: if the version 2 upgrade fails at the post_tx step.
        if old_meta.last_complete_open.is_some() {
            bail!("Can't create dir at path {}: is already in use:\n{:?}", path, old_meta);
        }

        s.write_meta(db_meta)?;
        Ok(s)
    }

    fn open_self(path: &str, create: bool) -> Result<Arc<SampleFileDir>, Error> {
        let fd = Fd::open(None, path, create)
            .map_err(|e| format_err!("unable to open sample file dir {}: {}", path, e))?;
        Ok(Arc::new(SampleFileDir {
            fd,
        }))
    }

    /// Opens the given sample file for reading.
    pub fn open_sample_file(&self, composite_id: CompositeId) -> Result<fs::File, io::Error> {
        let p = SampleFileDir::get_rel_pathname(composite_id);
        unsafe { self.fd.openat(p.as_ptr(), libc::O_RDONLY, 0) }
    }

    /// Reads the directory metadata. If none is found, returns an empty proto.
    fn read_meta(&self) -> Result<schema::DirMeta, Error> {
        let mut meta = schema::DirMeta::default();
        let p = unsafe { ffi::CStr::from_ptr("meta\0".as_ptr() as *const c_char) };
        let mut f = match unsafe { self.fd.openat(p.as_ptr(), libc::O_RDONLY, 0) } {
            Err(e) => {
                if e.kind() == ::std::io::ErrorKind::NotFound {
                    return Ok(meta);
                }
                return Err(e.into());
            },
            Ok(f) => f,
        };
        let mut data = Vec::new();
        f.read_to_end(&mut data)?;
        let mut s = protobuf::CodedInputStream::from_bytes(&data);
        meta.merge_from(&mut s).map_err(|e| e.context("Unable to parse metadata proto: {}"))?;
        Ok(meta)
    }

    // TODO: this should be exposed only to the db layer.
    pub fn write_meta(&self, meta: &schema::DirMeta) -> Result<(), Error> {
        let (tmp_path, final_path) = unsafe {
            (ffi::CStr::from_ptr("meta.tmp\0".as_ptr() as *const c_char),
             ffi::CStr::from_ptr("meta\0".as_ptr() as *const c_char))
        };
        let mut f = unsafe { self.fd.openat(tmp_path.as_ptr(),
                                            libc::O_CREAT | libc::O_TRUNC | libc::O_WRONLY,
                                            0o600)? };
        meta.write_to_writer(&mut f)?;
        f.sync_all()?;
        unsafe { renameat(&self.fd, tmp_path.as_ptr(), &self.fd, final_path.as_ptr())? };
        self.sync()?;
        Ok(())
    }

    /// Creates a new writer.
    /// Note this doesn't wait for previous rotation to complete; it's assumed the sample file
    /// directory has sufficient space for a couple recordings per camera in addition to the
    /// cameras' total `retain_bytes`.
    ///
    /// The new recording will continue from `prev` if specified; this should be as returned from
    /// a previous `close` call.
    pub fn create_writer<'a>(&'a self, db: &db::Database, channel: &'a SyncerChannel,
                             prev: Option<PreviousWriter>, stream_id: i32,
                             video_sample_entry_id: i32)
                             -> Result<Writer<'a>, Error> {
        let (id, r) = db.lock().add_recording(stream_id)?;
        let p = SampleFileDir::get_rel_pathname(id);
        let f = unsafe { self.fd.openat(p.as_ptr(), libc::O_WRONLY | libc::O_EXCL | libc::O_CREAT,
                                        0o600) }.unwrap();  // TODO: don't unwrap!
        Writer::open(f, id, r, prev, video_sample_entry_id, channel)
    }

    pub fn statfs(&self) -> Result<libc::statvfs, io::Error> { self.fd.statfs() }

    /// Gets a pathname for a sample file suitable for passing to open or unlink.
    fn get_rel_pathname(id: CompositeId) -> [libc::c_char; 17] {
        let mut buf = [0u8; 17];
        write!(&mut buf[..16], "{:016x}", id.0).expect("can't format id to pathname buf");

        // libc::c_char seems to be i8 on some platforms (Linux/arm) and u8 on others (Linux/amd64).
        unsafe { mem::transmute::<[u8; 17], [libc::c_char; 17]>(buf) }
    }

    /// Unlinks the given sample file within this directory.
    fn unlink(fd: &Fd, id: CompositeId) -> Result<(), io::Error> {
        let p = SampleFileDir::get_rel_pathname(id);
        let res = unsafe { libc::unlinkat(fd.0, p.as_ptr(), 0) };
        if res < 0 {
            return Err(io::Error::last_os_error())
        }
        Ok(())
    }

    /// Syncs the directory itself.
    fn sync(&self) -> Result<(), io::Error> {
        let res = unsafe { libc::fsync(self.fd.0) };
        if res < 0 {
            return Err(io::Error::last_os_error())
        }
        Ok(())
    }
}

/// A command sent to the syncer. These correspond to methods in the `SyncerChannel` struct.
enum SyncerCommand {
    AsyncSaveRecording(CompositeId, Arc<Mutex<db::UncommittedRecording>>, fs::File),
    //AsyncAbandonRecording(CompositeId),
    DatabaseFlushed,
    Flush(mpsc::SyncSender<()>),
}

/// A channel which can be used to send commands to the syncer.
/// Can be cloned to allow multiple threads to send commands.
#[derive(Clone)]
pub struct SyncerChannel(mpsc::Sender<SyncerCommand>);

/// State of the worker thread.
struct Syncer {
    dir_id: i32,
    dir: Arc<SampleFileDir>,
    db: Arc<db::Database>,
}

/// Starts a syncer for the given sample file directory.
///
/// The lock must not be held on `db` when this is called.
///
/// There should be only one syncer per directory, or 0 if operating in read-only mode.
/// This function will perform the initial rotation synchronously, so that it is finished before
/// file writing starts. Afterward the syncing happens in a background thread.
///
/// Returns a `SyncerChannel` which can be used to send commands (and can be cloned freely) and
/// a `JoinHandle` for the syncer thread. At program shutdown, all `SyncerChannel` clones should be
/// dropped and then the handle joined to allow all recordings to be persisted.
///
/// Note that dropping all `SyncerChannel` clones currently includes calling
/// `LockedDatabase::clear_on_flush`, as this function installs a hook to watch database flushes.
/// TODO: add a join wrapper which arranges for the on flush hook to be removed automatically.
pub fn start_syncer(db: Arc<db::Database>, dir_id: i32)
                    -> Result<(SyncerChannel, thread::JoinHandle<()>), Error> {
    let db2 = db.clone();
    let (mut syncer, path) = Syncer::new(&db.lock(), db2, dir_id)?;
    syncer.initial_rotation()?;
    let (snd, rcv) = mpsc::channel();
    db.lock().on_flush(Box::new({
        let snd = snd.clone();
        move || if let Err(e) = snd.send(SyncerCommand::DatabaseFlushed) {
            warn!("Unable to notify syncer for dir {} of flush: {}", dir_id, e);
        }
    }));
    Ok((SyncerChannel(snd),
        thread::Builder::new()
            .name(format!("sync-{}", path))
            .spawn(move || syncer.run(rcv)).unwrap()))
}

pub struct NewLimit {
    pub stream_id: i32,
    pub limit: i64,
}

/// Deletes recordings if necessary to fit within the given new `retain_bytes` limit.
/// Note this doesn't change the limit in the database; it only deletes files.
/// Pass a limit of 0 to delete all recordings associated with a camera.
pub fn lower_retention(db: Arc<db::Database>, dir_id: i32, limits: &[NewLimit])
                       -> Result<(), Error> {
    let db2 = db.clone();
    let (mut syncer, _) = Syncer::new(&db.lock(), db2, dir_id)?;
    syncer.do_rotation(|db| {
        for l in limits {
            let (bytes_before, extra);
            {
                let stream = db.streams_by_id().get(&l.stream_id)
                               .ok_or_else(|| format_err!("no such stream {}", l.stream_id))?;
                bytes_before = stream.sample_file_bytes - stream.bytes_to_delete;
                extra = stream.retain_bytes - l.limit;
            }
            if l.limit >= bytes_before { continue }
            delete_recordings(db, l.stream_id, extra)?;
            let stream = db.streams_by_id().get(&l.stream_id).unwrap();
            info!("stream {}, deleting: {}->{}", l.stream_id, bytes_before,
                  stream.sample_file_bytes - stream.bytes_to_delete);
        }
        Ok(())
    })
}

/// Deletes recordings to bring a stream's disk usage within bounds.
fn delete_recordings(db: &mut db::LockedDatabase, stream_id: i32,
                     extra_bytes_needed: i64) -> Result<(), Error> {
    let bytes_needed = {
        let stream = match db.streams_by_id().get(&stream_id) {
            None => bail!("no stream {}", stream_id),
            Some(s) => s,
        };
        stream.sample_file_bytes - stream.bytes_to_delete + extra_bytes_needed
            - stream.retain_bytes
    };
    let mut bytes_to_delete = 0;
    if bytes_needed <= 0 {
        debug!("{}: have remaining quota of {}", stream_id, -bytes_needed);
        return Ok(());
    }
    let mut n = 0;
    db.delete_oldest_recordings(stream_id, &mut |row| {
        n += 1;
        if bytes_needed >= bytes_to_delete {
            bytes_to_delete += row.sample_file_bytes as i64;
            n += 1;
            return true;
        }
        false
    })?;
    info!("{}: deleting {} bytes in {} recordings ({} bytes needed)",
          stream_id, bytes_to_delete, n, bytes_needed);
    Ok(())
}

impl SyncerChannel {
    /// Asynchronously syncs the given writer, closes it, records it into the database, and
    /// starts rotation.
    fn async_save_recording(&self, id: CompositeId, recording: Arc<Mutex<db::UncommittedRecording>>,
                            f: fs::File) {
        self.0.send(SyncerCommand::AsyncSaveRecording(id, recording, f)).unwrap();
    }

    //fn async_abandon_recording(&self, id: CompositeId) {
    //    self.0.send(SyncerCommand::AsyncAbandonRecording(id)).unwrap();
    //}

    /// For testing: flushes the syncer, waiting for all currently-queued commands to complete.
    pub fn flush(&self) {
        let (snd, rcv) = mpsc::sync_channel(0);
        self.0.send(SyncerCommand::Flush(snd)).unwrap();
        rcv.recv().unwrap_err();  // syncer should just drop the channel, closing it.
    }
}

impl Syncer {
    fn new(l: &db::LockedDatabase, db: Arc<db::Database>, dir_id: i32)
           -> Result<(Self, String), Error> {
        let d = l.sample_file_dirs_by_id()
                 .get(&dir_id)
                 .ok_or_else(|| format_err!("no dir {}", dir_id))?;
        let dir = d.get()?;

        // Abandon files.
        // First, get a list of the streams in question.
        let streams_to_next: FnvHashMap<_, _> =
            l.streams_by_id()
             .iter()
             .filter_map(|(&k, v)| {
                 if v.sample_file_dir_id == Some(dir_id) {
                    Some((k, v.next_recording_id))
                 } else {
                     None
                 }
             })
             .collect();
        let to_abandon = Syncer::list_files_to_abandon(&d.path, streams_to_next)?;
        let mut undeletable = 0;
        for &id in &to_abandon {
            if let Err(e) = SampleFileDir::unlink(&dir.fd, id) {
                if e.kind() == io::ErrorKind::NotFound {
                    warn!("dir: abandoned recording {} already deleted!", id);
                } else {
                    warn!("dir: Unable to unlink abandoned recording {}: {}", id, e);
                    undeletable += 1;
                }
            }
        }
        if undeletable > 0 {
            bail!("Unable to delete {} abandoned recordings.", undeletable);
        }

        Ok((Syncer {
            dir_id,
            dir,
            db,
        }, d.path.clone()))
    }

    /// Lists files which should be "abandoned" (deleted without ever recording in the database)
    /// on opening.
    fn list_files_to_abandon(path: &str, streams_to_next: FnvHashMap<i32, i32>)
                             -> Result<Vec<CompositeId>, Error> {
        let mut v = Vec::new();
        for e in ::std::fs::read_dir(path)? {
            let e = e?;
            let id = match parse_id(e.file_name().as_bytes()) {
                Ok(i) => i,
                Err(_) => continue,
            };
            let next = match streams_to_next.get(&id.stream()) {
                Some(n) => *n,
                None => continue,  // unknown stream.
            };
            if id.recording() >= next {
                v.push(id);
            }
        }
        Ok(v)
    }

    fn run(&mut self, cmds: mpsc::Receiver<SyncerCommand>) {
        loop {
            match cmds.recv() {
                Err(_) => return,  // all senders have closed the channel; shutdown
                Ok(SyncerCommand::AsyncSaveRecording(id, r, f)) => self.save(id, r, f),
                //Ok(SyncerCommand::AsyncAbandonRecording(uuid)) => self.abandon(uuid),
                Ok(SyncerCommand::DatabaseFlushed) => { let _ = self.collect_garbage(true); },
                Ok(SyncerCommand::Flush(_)) => {},  // just drop the supplied sender, closing it.
            };
        }
    }

    /// Rotates files for all streams and deletes stale files from previous runs.
    fn initial_rotation(&mut self) -> Result<(), Error> {
        self.do_rotation(|db| {
            let streams: Vec<i32> = db.streams_by_id().keys().map(|&id| id).collect();
            for &stream_id in &streams {
                delete_recordings(db, stream_id, 0)?;
            }
            Ok(())
        })
    }

    fn do_rotation<F>(&mut self, delete_recordings: F) -> Result<(), Error>
    where F: FnOnce(&mut db::LockedDatabase) -> Result<(), Error> {
        {
            let mut db = self.db.lock();
            delete_recordings(&mut *db)?;
            db.flush("synchronous deletion")?;
        }
        self.collect_garbage(false)?;
        self.db.lock().flush("synchronous garbage collection")
    }

    fn collect_garbage(&mut self, warn_on_missing: bool) -> Result<(), Error> {
        let mut garbage: Vec<_> = {
            let l = self.db.lock();
            let d = match l.sample_file_dirs_by_id().get(&self.dir_id) {
                None => {
                    error!("can't find dir {} in db!", self.dir_id);
                    bail!("can't find dir {} in db!", self.dir_id);
                },
                Some(d) => d,
            };
            d.garbage.iter().map(|id| *id).collect()
        };
        let len_before = garbage.len();
        garbage.retain(|&id| {
            if let Err(e) = SampleFileDir::unlink(&self.dir.fd, id) {
                if e.kind() == io::ErrorKind::NotFound {
                    if warn_on_missing {
                        warn!("dir: recording {} already deleted!", id);
                    }
                } else {
                    warn!("dir: Unable to unlink {}: {}", id, e);
                    return false;
                }
            }
            true
        });
        let res = if len_before > garbage.len() {
            Err(format_err!("Unable to unlink {} files", len_before - garbage.len()))
        } else {
            Ok(())
        };
        if garbage.is_empty() {
            // No progress.
            return res;
        }
        if let Err(e) = self.dir.sync() {
            error!("unable to sync dir: {}", e);
            return res.and(Err(e.into()));
        }
        if let Err(e) = self.db.lock().delete_garbage(self.dir_id, &mut garbage) {
            error!("unable to delete garbage ({} files) for dir {}: {}",
                   self.dir_id, garbage.len(), e);
            return res.and(Err(e.into()));
        }
        res
    }

    /// Saves the given recording and causes rotation to happen.
    /// Note that part of rotation is deferred for the next cycle (saved writing or program startup)
    /// so that there can be only one dir sync and database transaction per save.
    /// Internal helper for `save`. This is separated out so that the question-mark operator
    /// can be used in the many error paths.
    /// TODO: less unwrapping! keep a queue?
    fn save(&mut self, id: CompositeId, recording: Arc<Mutex<db::UncommittedRecording>>,
            f: fs::File) {
        let stream_id = id.stream();

        // Free up a like number of bytes.
        delete_recordings(&mut self.db.lock(), stream_id, 0).unwrap();

        f.sync_all().unwrap();
        self.dir.sync().unwrap();
        recording.lock().synced = true;
        let mut db = self.db.lock();
        let reason = {
            let s = db.streams_by_id().get(&stream_id).unwrap();
            let c = db.cameras_by_id().get(&s.camera_id).unwrap();
            let unflushed = s.unflushed();
            if unflushed < s.flush_if {
                debug!("{}-{}: unflushed={} < if={}, not flushing",
                       c.short_name, s.type_.as_str(), unflushed, s.flush_if);
                return;
            }
            format!("{}-{}: unflushed={} >= if={}",
                    c.short_name, s.type_.as_str(), unflushed, s.flush_if)
        };
        if let Err(e) = db.flush(&reason) {
            error!("flush failure on save: {:?}", e);
        }
    }
}

/// Single-use struct to write a single recording to disk and commit its metadata to the database.
/// Use `SampleFileDir::create_writer` to create a new writer. `Writer` hands off its state to the
/// syncer when done. It either saves the recording to the database (if I/O errors do not prevent
/// this) or marks it as abandoned so that the syncer will attempt to unlink the file.
pub struct Writer<'a>(Option<InnerWriter<'a>>);

/// The state associated with a `Writer`. The indirection is for the `Drop` trait; `close` moves
/// `f` and `index.video_index` out of the `InnerWriter`, which is not allowed on a struct with
/// a `Drop` trait. To avoid this problem, the real state is surrounded by an `Option`. The
/// `Option` should none only after close is called, and thus never in a way visible to callers.
struct InnerWriter<'a> {
    syncer_channel: &'a SyncerChannel,
    f: fs::File,
    r: Arc<Mutex<db::UncommittedRecording>>,
    index: recording::SampleIndexEncoder,
    id: CompositeId,
    corrupt: bool,
    hasher: hash::Hasher,

    /// The end time of the previous segment in this run, if any.
    prev_end: Option<recording::Time>,

    /// The start time of this segment, based solely on examining the local clock after frames in
    /// this segment were received. Frames can suffer from various kinds of delay (initial
    /// buffering, encoding, and network transmission), so this time is set to far in the future on
    /// construction, given a real value on the first packet, and decreased as less-delayed packets
    /// are discovered. See design/time.md for details.
    local_start: recording::Time,

    adjuster: ClockAdjuster,

    video_sample_entry_id: i32,
    run_offset: i32,

    /// A sample which has been written to disk but not added to `index`. Index writes are one
    /// sample behind disk writes because the duration of a sample is the difference between its
    /// pts and the next sample's pts. A sample is flushed when the next sample is written, when
    /// the writer is closed cleanly (the caller supplies the next pts), or when the writer is
    /// closed uncleanly (with a zero duration, which the `.mp4` format allows only at the end).
    unflushed_sample: Option<UnflushedSample>,
}

/// Adjusts durations given by the camera to correct its clock frequency error.
#[derive(Copy, Clone, Debug)]
struct ClockAdjuster {
    /// Every `every_minus_1 + 1` units, add `-ndir`.
    /// Note i32::max_value() disables adjustment.
    every_minus_1: i32,

    /// Should be 1 or -1 (unless disabled).
    ndir: i32,

    /// Keeps accumulated difference from previous values.
    cur: i32,
}

impl ClockAdjuster {
    fn new(local_time_delta: Option<i64>) -> Self {
        // Pick an adjustment rate to correct local_time_delta over the next minute (the
        // desired duration of a single recording). Cap the rate at 500 ppm (which corrects
        // 2,700/90,000ths of a second over a minute) to prevent noticeably speeding up or slowing
        // down playback.
        let (every_minus_1, ndir) = match local_time_delta {
            Some(d) if d <= -2700 => (1999,  1),
            Some(d) if d >=  2700 => (1999, -1),
            Some(d) if d < -60 => ((60 * 90000) / -(d as i32) - 1,  1),
            Some(d) if d > 60  => ((60 * 90000) /  (d as i32) - 1, -1),
            _ => (i32::max_value(), 0),
        };
        ClockAdjuster{
            every_minus_1,
            ndir,
            cur: 0,
        }
    }

    fn adjust(&mut self, mut val: i32) -> i32 {
        self.cur += val;

        // The "val > self.ndir" here is so that if decreasing durations (ndir == 1), we don't
        // cause a duration of 1 to become a duration of 0. It has no effect when increasing
        // durations. (There's no danger of a duration of 0 becoming a duration of 1; cur wouldn't
        // be newly > self.every_minus_1.)
        while self.cur > self.every_minus_1 && val > self.ndir {
            val -= self.ndir;
            self.cur -= self.every_minus_1 + 1;
        }
        val
    }
}

struct UnflushedSample {
    local_time: recording::Time,
    pts_90k: i64,
    len: i32,
    is_key: bool,
}

#[derive(Copy, Clone)]
pub struct PreviousWriter {
    end_time: recording::Time,
    local_time_delta: recording::Duration,
    run_offset: i32,
}

impl<'a> Writer<'a> {
    /// Opens the writer; for use by `SampleFileDir` (which should supply `f`).
    fn open(f: fs::File, id: CompositeId, r: Arc<Mutex<db::UncommittedRecording>>,
            prev: Option<PreviousWriter>,
            video_sample_entry_id: i32, syncer_channel: &'a SyncerChannel) -> Result<Self, Error> {
        Ok(Writer(Some(InnerWriter {
            syncer_channel,
            f,
            r,
            index: recording::SampleIndexEncoder::new(),
            id,
            corrupt: false,
            hasher: hash::Hasher::new(hash::MessageDigest::sha1())?,
            prev_end: prev.map(|p| p.end_time),
            local_start: recording::Time(i64::max_value()),
            adjuster: ClockAdjuster::new(prev.map(|p| p.local_time_delta.0)),
            video_sample_entry_id,
            run_offset: prev.map(|p| p.run_offset + 1).unwrap_or(0),
            unflushed_sample: None,
        })))
    }

    /// Writes a new frame to this segment.
    /// `local_time` should be the local clock's time as of when this packet was received.
    pub fn write(&mut self, pkt: &[u8], local_time: recording::Time, pts_90k: i64,
                 is_key: bool) -> Result<(), Error> {
        let w = self.0.as_mut().unwrap();
        if let Some(unflushed) = w.unflushed_sample.take() {
            let duration = (pts_90k - unflushed.pts_90k) as i32;
            if duration <= 0 {
                bail!("pts not monotonically increasing; got {} then {}",
                      unflushed.pts_90k, pts_90k);
            }
            let duration = w.adjuster.adjust(duration);
            w.index.add_sample(duration, unflushed.len, unflushed.is_key);
            w.extend_local_start(unflushed.local_time);
        }
        let mut remaining = pkt;
        while !remaining.is_empty() {
            let written = match w.f.write(remaining) {
                Ok(b) => b,
                Err(e) => {
                    if remaining.len() < pkt.len() {
                        // Partially written packet. Truncate if possible.
                        // TODO: have the syncer do this / retry it if necessary?
                        if let Err(e2) = w.f.set_len(w.index.sample_file_bytes as u64) {
                            error!("After write to {} failed with {}, truncate failed with {}; \
                                    sample file is corrupt.", w.id, e, e2);
                            w.corrupt = true;
                        }
                    }
                    return Err(Error::from(e));
                },
            };
            remaining = &remaining[written..];
        }
        w.unflushed_sample = Some(UnflushedSample{
            local_time: local_time,
            pts_90k: pts_90k,
            len: pkt.len() as i32,
            is_key: is_key});
        w.hasher.update(pkt)?;
        Ok(())
    }

    /// Cleanly closes the writer, using a supplied pts of the next sample for the last sample's
    /// duration (if known). If `close` is not called, the `Drop` trait impl will close the trait,
    /// swallowing errors and using a zero duration for the last sample.
    pub fn close(mut self, next_pts: Option<i64>) -> Result<PreviousWriter, Error> {
        self.0.take().unwrap().close(next_pts)
    }
}

impl<'a> InnerWriter<'a> {
    fn extend_local_start(&mut self, pkt_local_time: recording::Time) {
        let new = pkt_local_time - recording::Duration(self.index.total_duration_90k as i64);
        self.local_start = cmp::min(self.local_start, new);
    }

    fn close(mut self, next_pts: Option<i64>) -> Result<PreviousWriter, Error> {
        if self.corrupt {
            //self.syncer_channel.async_abandon_recording(self.id);
            bail!("recording {} is corrupt", self.id);
        }
        let unflushed =
            self.unflushed_sample.take()
            .ok_or_else(|| format_err!("recording {} has no packets", self.id))?;
        let duration = self.adjuster.adjust(match next_pts {
            None => 0,
            Some(p) => (p - unflushed.pts_90k) as i32,
        });
        self.index.add_sample(duration, unflushed.len, unflushed.is_key);
        self.extend_local_start(unflushed.local_time);
        let mut sha1_bytes = [0u8; 20];
        sha1_bytes.copy_from_slice(&self.hasher.finish()?[..]);
        let start = self.prev_end.unwrap_or(self.local_start);
        let end = start + recording::Duration(self.index.total_duration_90k as i64);
        let flags = if self.index.has_trailing_zero() { db::RecordingFlags::TrailingZero as i32 }
                    else { 0 };
        let local_start_delta = self.local_start - start;
        let recording = db::RecordingToInsert {
            sample_file_bytes: self.index.sample_file_bytes,
            time: start .. end,
            local_time_delta: local_start_delta,
            video_samples: self.index.video_samples,
            video_sync_samples: self.index.video_sync_samples,
            video_sample_entry_id: self.video_sample_entry_id,
            video_index: self.index.video_index,
            sample_file_sha1: sha1_bytes,
            run_offset: self.run_offset,
            flags: flags,
        };
        self.r.lock().recording = Some(recording);
        self.syncer_channel.async_save_recording(self.id, self.r, self.f);
        Ok(PreviousWriter{
            end_time: end,
            local_time_delta: local_start_delta,
            run_offset: self.run_offset,
        })
    }
}

impl<'a> Drop for Writer<'a> {
    fn drop(&mut self) {
        if let Some(w) = self.0.take() {
            // Swallow any error. The caller should only drop the Writer without calling close()
            // if there's already been an error. The caller should report that. No point in
            // complaining again.
            let _ = w.close(None);
        }
    }
}

/// Parse a composite id filename.
///
/// These are exactly 16 bytes, lowercase hex.
fn parse_id(id: &[u8]) -> Result<CompositeId, ()> {
    if id.len() != 16 {
        return Err(());
    }
    let mut v: u64 = 0;
    for i in 0..16 {
        v = (v << 4) | match id[i] {
            b @ b'0'...b'9' => b - b'0',
            b @ b'a'...b'f' => b - b'a' + 10,
            _ => return Err(()),
        } as u64;
    }
    Ok(CompositeId(v as i64))
}

#[cfg(test)]
mod tests {
    use super::ClockAdjuster;
    use testutil;

    #[test]
    fn adjust() {
        testutil::init();

        // no-ops.
        for v in &[None, Some(0), Some(-10), Some(10)] {
            let mut a = ClockAdjuster::new(*v);
            for _ in 0..1800 {
                assert_eq!(3000, a.adjust(3000), "v={:?}", *v);
            }
        }

        // typical, 100 ppm adjustment.
        let mut a = ClockAdjuster::new(Some(-540));
        let mut total = 0;
        for _ in 0..1800 {
            let new = a.adjust(3000);
            assert!(new == 2999 || new == 3000);
            total += new;
        }
        let expected = 1800*3000 - 540;
        assert!(total == expected || total == expected + 1, "total={} vs expected={}",
                total, expected);

        a = ClockAdjuster::new(Some(540));
        let mut total = 0;
        for _ in 0..1800 {
            let new = a.adjust(3000);
            assert!(new == 3000 || new == 3001);
            total += new;
        }
        let expected = 1800*3000 + 540;
        assert!(total == expected || total == expected + 1, "total={} vs expected={}",
                total, expected);

        // capped at 500 ppm (change of 2,700/90,000ths over 1 minute).
        a = ClockAdjuster::new(Some(-1_000_000));
        total = 0;
        for _ in 0..1800 {
            let new = a.adjust(3000);
            assert!(new == 2998 || new == 2999, "new={}", new);
            total += new;
        }
        let expected = 1800*3000 - 2700;
        assert!(total == expected || total == expected + 1, "total={} vs expected={}",
                total, expected);

        a = ClockAdjuster::new(Some(1_000_000));
        total = 0;
        for _ in 0..1800 {
            let new = a.adjust(3000);
            assert!(new == 3001 || new == 3002, "new={}", new);
            total += new;
        }
        let expected = 1800*3000 + 2700;
        assert!(total == expected || total == expected + 1, "total={} vs expected={}",
                total, expected);
    }

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
}
