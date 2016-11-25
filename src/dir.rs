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
//! This includes opening files for serving, rotating away old
//! files, and syncing new files to disk.

use db;
use libc;
use recording;
use error::Error;
use std::ffi;
use std::fs;
use std::io::{self, Write};
use std::mem;
use std::os::unix::io::FromRawFd;
use std::sync::{Arc, Mutex, MutexGuard};
use std::sync::mpsc;
use std::thread;
use uuid::Uuid;

/// A sample file directory. This is currently a singleton in production. (Maybe in the future
/// Moonfire will be extended to support multiple directories on different spindles.)
///
/// If the directory is used for writing, the `start_syncer` function should be called to start
/// a background thread. This thread manages deleting files and writing new files. It synces the
/// directory and commits these operations to the database in the correct order to maintain the
/// invariants described in `design/schema.md`.
pub struct SampleFileDir {
    db: Arc<db::Database>,

    /// The open file descriptor for the directory. The worker uses it to create files and sync the
    /// directory. Other threads use it to open sample files for reading during video serving.
    fd: Fd,

    // Lock order: don't acquire mutable.lock() while holding db.lock().
    mutable: Mutex<SharedMutableState>,
}

/// A file descriptor associated with a directory (not necessarily the sample file dir).
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
    pub fn open(path: &str) -> Result<Fd, io::Error> {
        let cstring = ffi::CString::new(path)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let fd = unsafe { libc::open(cstring.as_ptr(), libc::O_DIRECTORY | libc::O_RDONLY, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Fd(fd))
    }

    /// Locks the directory with the specified `flock` operation.
    pub fn lock(&self, operation: libc::c_int) -> Result<(), io::Error> {
        let ret = unsafe { libc::flock(self.0, operation) };
        if ret < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }
}

impl SampleFileDir {
    pub fn new(path: &str, db: Arc<db::Database>) -> Result<Arc<SampleFileDir>, Error> {
        let fd = Fd::open(path)?;
        Ok(Arc::new(SampleFileDir{
            db: db,
            fd: fd,
            mutable: Mutex::new(SharedMutableState{
                next_uuid: None,
            }),
        }))
    }

    /// Opens the given sample file for reading.
    pub fn open_sample_file(&self, uuid: Uuid) -> Result<fs::File, io::Error> {
        self.open_int(uuid, libc::O_RDONLY, 0)
    }

    /// Creates a new writer.
    /// Note this doesn't wait for previous rotation to complete; it's assumed the sample file
    /// directory has sufficient space for a couple recordings per camera in addition to the
    /// cameras' total `retain_bytes`.
    pub fn create_writer(&self, start: recording::Time, local_start: recording::Time,
                         camera_id: i32, video_sample_entry_id: i32)
                         -> Result<recording::Writer, Error> {
        // Grab the next uuid. Typically one is cached—a sync has usually completed since the last
        // writer was created, and syncs ensure `next_uuid` is filled while performing their
        // transaction. But if not, perform an extra database transaction to reserve a new one.
        let uuid = match self.mutable.lock().unwrap().next_uuid.take() {
            Some(u) => u,
            None => {
                info!("Committing extra transaction because there's no cached uuid");
                let mut db = self.db.lock();
                let mut tx = db.tx()?;
                let u = tx.reserve_sample_file()?;
                tx.commit()?;
                u
            },
        };

        let f = match self.open_int(uuid, libc::O_WRONLY | libc::O_EXCL | libc::O_CREAT, 0o600) {
            Ok(f) => f,
            Err(e) => {
                self.mutable.lock().unwrap().next_uuid = Some(uuid);
                return Err(e.into());
            },
        };
        recording::Writer::open(f, uuid, start, local_start, camera_id, video_sample_entry_id)
    }

    /// Opens a sample file within this directory with the given flags and (if creating) mode.
    fn open_int(&self, uuid: Uuid, flags: libc::c_int, mode: libc::c_int)
                -> Result<fs::File, io::Error> {
        let p = SampleFileDir::get_rel_pathname(uuid);
        let fd = unsafe { libc::openat(self.fd.0, p.as_ptr(), flags, mode) };
        if fd < 0 {
            return Err(io::Error::last_os_error())
        }
        unsafe { Ok(fs::File::from_raw_fd(fd)) }
    }

    /// Gets a pathname for a sample file suitable for passing to open or unlink.
    fn get_rel_pathname(uuid: Uuid) -> [libc::c_char; 37] {
        let mut buf = [0u8; 37];
        write!(&mut buf[..36], "{}", uuid.hyphenated()).expect("can't format uuid to pathname buf");

        // libc::c_char seems to be i8 on some platforms (Linux/arm) and u8 on others (Linux/amd64).
        // Transmute, suppressing the warning that happens on platforms in which it's already u8.
        #[allow(useless_transmute)]
        unsafe { mem::transmute::<[u8; 37], [libc::c_char; 37]>(buf) }
    }

    /// Unlinks the given sample file within this directory.
    fn unlink(fd: &Fd, uuid: Uuid) -> Result<(), io::Error> {
        let p = SampleFileDir::get_rel_pathname(uuid);
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
        };
        Ok(())
    }
}

/// State shared between users of the `SampleFileDirectory` struct and the syncer.
struct SharedMutableState {
    next_uuid: Option<Uuid>,
}

/// A command sent to the syncer. These correspond to methods in the `SyncerChannel` struct.
enum SyncerCommand {
    AsyncSaveWriter(db::RecordingToInsert, fs::File),

    #[cfg(test)]
    Flush(mpsc::SyncSender<()>),
}

/// A channel which can be used to send commands to the syncer.
/// Can be cloned to allow multiple threads to send commands.
#[derive(Clone)]
pub struct SyncerChannel(mpsc::Sender<SyncerCommand>);

/// State of the worker thread.
struct SyncerState {
    dir: Arc<SampleFileDir>,
    to_unlink: Vec<Uuid>,
    to_mark_deleted: Vec<Uuid>,
    cmds: mpsc::Receiver<SyncerCommand>,
}

/// Starts a syncer for the given sample file directory.
/// There should be only one syncer per directory, or 0 if operating in read-only mode.
/// This function will perform the initial rotation synchronously, so that it is finished before
/// file writing starts. Afterward the syncing happens in a background thread.
///
/// Returns a `SyncerChannel` which can be used to send commands (and can be cloned freely) and
/// a `JoinHandle` for the syncer thread. At program shutdown, all `SyncerChannel` clones should be
/// removed and then the handle joined to allow all recordings to be persisted.
pub fn start_syncer(dir: Arc<SampleFileDir>)
                    -> Result<(SyncerChannel, thread::JoinHandle<()>), Error> {
    let to_unlink = dir.db.lock().list_reserved_sample_files()?;
    let (snd, rcv) = mpsc::channel();
    let mut state = SyncerState {
        dir: dir,
        to_unlink: to_unlink,
        to_mark_deleted: Vec::new(),
        cmds: rcv,
    };
    state.initial_rotation()?;
    Ok((SyncerChannel(snd),
        thread::Builder::new().name("syncer".into()).spawn(move || state.run()).unwrap()))
}

impl SyncerChannel {
    /// Asynchronously syncs the given writer, closes it, records it into the database, and
    /// starts rotation.
    pub fn async_save_writer(&self, w: recording::Writer) -> Result<(), Error> {
        let (recording, f) = w.close()?;
        self.0.send(SyncerCommand::AsyncSaveWriter(recording, f)).unwrap();
        Ok(())
    }

    /// For testing: flushes the syncer, waiting for all currently-queued commands to complete.
    #[cfg(test)]
    pub fn flush(&self) {
        let (snd, rcv) = mpsc::sync_channel(0);
        self.0.send(SyncerCommand::Flush(snd)).unwrap();
        rcv.recv().unwrap_err();  // syncer should just drop the channel, closing it.
    }
}

impl SyncerState {
    fn run(&mut self) {
        loop {
            match self.cmds.recv() {
                Err(_) => return,  // all senders have closed the channel; shutdown
                Ok(SyncerCommand::AsyncSaveWriter(recording, f)) => self.save_writer(recording, f),

                #[cfg(test)]
                Ok(SyncerCommand::Flush(_)) => {},  // just drop the supplied sender, closing it.
            };
        }
    }

    /// Rotates files for all cameras and deletes stale reserved uuids from previous runs.
    fn initial_rotation(&mut self) -> Result<(), Error> {
        let mut to_delete = Vec::new();
        {
            let mut db = self.dir.db.lock();
            for (camera_id, camera) in db.cameras_by_id() {
                self.get_rows_to_delete(&db, *camera_id, camera, 0, &mut to_delete)?;
            }
            let mut tx = db.tx()?;
            tx.delete_recordings(&to_delete)?;
            tx.commit()?;
        }
        for row in to_delete {
            self.to_unlink.push(row.uuid);
        }
        self.try_unlink();
        if !self.to_unlink.is_empty() {
            return Err(Error::new(format!("failed to unlink {} sample files",
                                          self.to_unlink.len())));
        }
        self.dir.sync()?;
        {
            let mut db = self.dir.db.lock();
            let mut tx = db.tx()?;
            tx.mark_sample_files_deleted(&self.to_mark_deleted)?;
            tx.commit()?;
        }
        self.to_mark_deleted.clear();
        Ok(())
    }

    /// Saves the given writer and causes rotation to happen.
    /// Note that part of rotation is deferred for the next cycle (saved writing or program startup)
    /// so that there can be only one dir sync and database transaction per save.
    fn save_writer(&mut self, recording: db::RecordingToInsert, f: fs::File) {
        if let Err(e) = self.save_writer_helper(&recording, f) {
            error!("camera {}: will discard recording {} due to error while saving: {}",
                   recording.camera_id, recording.sample_file_uuid, e);
            self.to_unlink.push(recording.sample_file_uuid);
            return;
        }
    }

    /// Internal helper for `save_writer`. This is separated out so that the question-mark operator
    /// can be used in the many error paths.
    fn save_writer_helper(&mut self, recording: &db::RecordingToInsert, f: fs::File)
                          -> Result<(), Error> {
        self.try_unlink();
        if !self.to_unlink.is_empty() {
            return Err(Error::new(format!("failed to unlink {} files.", self.to_unlink.len())));
        }
        f.sync_all()?;
        self.dir.sync()?;

        let mut to_delete = Vec::new();
        let mut l = self.dir.mutable.lock().unwrap();
        let mut db = self.dir.db.lock();
        let mut new_next_uuid = l.next_uuid;
        {
            let camera =
                db.cameras_by_id().get(&recording.camera_id)
                  .ok_or_else(|| Error::new(format!("no such camera {}", recording.camera_id)))?;
            self.get_rows_to_delete(&db, recording.camera_id, camera,
                                    recording.sample_file_bytes as i64, &mut to_delete)?;
        }
        let mut tx = db.tx()?;
        tx.mark_sample_files_deleted(&self.to_mark_deleted)?;
        tx.delete_recordings(&to_delete)?;
        if new_next_uuid.is_none() {
            new_next_uuid = Some(tx.reserve_sample_file()?);
        }
        tx.insert_recording(recording)?;
        tx.commit()?;
        l.next_uuid = new_next_uuid;

        self.to_mark_deleted.clear();
        self.to_unlink.extend(to_delete.iter().map(|row| row.uuid));
        Ok(())
    }

    /// Gets rows to delete to bring a camera's disk usage within bounds.
    fn get_rows_to_delete(&self, db: &MutexGuard<db::LockedDatabase>, camera_id: i32,
                          camera: &db::Camera, extra_bytes_needed: i64,
                          to_delete: &mut Vec<db::ListOldestSampleFilesRow>) -> Result<(), Error> {
        let bytes_needed = camera.sample_file_bytes + extra_bytes_needed - camera.retain_bytes;
        let mut bytes_to_delete = 0;
        if bytes_needed <= 0 {
            debug!("{}: have remaining quota of {}", camera.short_name, -bytes_needed);
            return Ok(());
        }
        let mut n = 0;
        db.list_oldest_sample_files(camera_id, |row| {
            bytes_to_delete += row.sample_file_bytes as i64;
            to_delete.push(row);
            n += 1;
            bytes_needed > bytes_to_delete  // continue as long as more deletions are needed.
        })?;
        if bytes_needed > bytes_to_delete {
            return Err(Error::new(format!("{}: couldn't find enough files to delete: {} left.",
                                          camera.short_name, bytes_needed)));
        }
        info!("{}: deleting {} bytes in {} recordings ({} bytes needed)",
              camera.short_name, bytes_to_delete, n, bytes_needed);
        Ok(())
    }

    /// Tries to unlink all the uuids in `self.to_unlink`. Any which can't be unlinked will
    /// be retained in the vec.
    fn try_unlink(&mut self) {
        let to_mark_deleted = &mut self.to_mark_deleted;
        let fd = &self.dir.fd;
        self.to_unlink.retain(|uuid| {
            if let Err(e) = SampleFileDir::unlink(fd, *uuid) {
                if e.kind() == io::ErrorKind::NotFound {
                    warn!("dir: Sample file {} already deleted!", uuid.hyphenated());
                    to_mark_deleted.push(*uuid);
                    false
                } else {
                    warn!("dir: Unable to unlink {}: {}", uuid.hyphenated(), e);
                    true
                }
            } else {
                to_mark_deleted.push(*uuid);
                false
            }
        });
    }
}
