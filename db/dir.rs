// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 Scott Lamb <slamb@slamb.org>
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

use crate::db::CompositeId;
use failure::{Error, Fail, bail, format_err};
use libc::c_char;
use log::warn;
use protobuf::Message;
use crate::schema;
use std::ffi;
use std::fs;
use std::io::{self, Read, Write};
use std::mem;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::FromRawFd;
use std::sync::Arc;

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
    pub(crate) fd: Fd,
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
    pub fn open(path: &str, mkdir: bool) -> Result<Fd, io::Error> {
        let cstring = ffi::CString::new(path)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        if mkdir && unsafe { libc::mkdir(cstring.as_ptr(), 0o700) } != 0 {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::AlreadyExists {
                return Err(e.into());
            }
        }
        let fd = unsafe { libc::open(cstring.as_ptr(), libc::O_DIRECTORY | libc::O_RDONLY, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Fd(fd))
    }

    pub(crate) fn sync(&self) -> Result<(), io::Error> {
        let res = unsafe { libc::fsync(self.0) };
        if res < 0 {
            return Err(io::Error::last_os_error())
        }
        Ok(())
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

pub(crate) unsafe fn renameat(from_fd: &Fd, from_path: *const c_char,
                   to_fd: &Fd, to_path: *const c_char) -> Result<(), io::Error> {
    let result = libc::renameat(from_fd.0, from_path, to_fd.0, to_path);
    if result < 0 {
        return Err(io::Error::last_os_error())
    }
    Ok(())
}

/// Reads `dir`'s metadata. If none is found, returns an empty proto.
pub(crate) fn read_meta(dir: &Fd) -> Result<schema::DirMeta, Error> {
    let mut meta = schema::DirMeta::default();
    let p = unsafe { ffi::CStr::from_ptr("meta\0".as_ptr() as *const c_char) };
    let mut f = match unsafe { dir.openat(p.as_ptr(), libc::O_RDONLY, 0) } {
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

/// Write `dir`'s metadata, clobbering existing data.
pub(crate) fn write_meta(dir: &Fd, meta: &schema::DirMeta) -> Result<(), Error> {
    let (tmp_path, final_path) = unsafe {
        (ffi::CStr::from_ptr("meta.tmp\0".as_ptr() as *const c_char),
         ffi::CStr::from_ptr("meta\0".as_ptr() as *const c_char))
    };
    let mut f = unsafe { dir.openat(tmp_path.as_ptr(),
                                    libc::O_CREAT | libc::O_TRUNC | libc::O_WRONLY, 0o600)? };
    meta.write_to_writer(&mut f)?;
    f.sync_all()?;
    unsafe { renameat(&dir, tmp_path.as_ptr(), &dir, final_path.as_ptr())? };
    dir.sync()?;
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
        let dir_meta = read_meta(&s.fd)?;
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

    pub(crate) fn create(path: &str, db_meta: &schema::DirMeta)
                         -> Result<Arc<SampleFileDir>, Error> {
        let s = SampleFileDir::open_self(path, true)?;
        s.fd.lock(libc::LOCK_EX | libc::LOCK_NB)?;
        let old_meta = read_meta(&s.fd)?;

        // Verify metadata. We only care that it hasn't been completely opened.
        // Partial opening by this or another database is fine; we won't overwrite anything.
        if old_meta.last_complete_open.is_some() {
            bail!("Can't create dir at path {}: is already in use:\n{:?}", path, old_meta);
        }
        if !SampleFileDir::is_empty(path)? {
            bail!("Can't create dir at path {} with existing files", path);
        }
        s.write_meta(db_meta)?;
        Ok(s)
    }

    /// Determines if the directory is empty, aside form metadata.
    pub(crate) fn is_empty(path: &str) -> Result<bool, Error> {
        for e in fs::read_dir(path)? {
            let e = e?;
            match e.file_name().as_bytes() {
                b"." | b".." => continue,
                b"meta" | b"meta-tmp" => continue,  // existing metadata is fine.
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    fn open_self(path: &str, create: bool) -> Result<Arc<SampleFileDir>, Error> {
        let fd = Fd::open(path, create)
            .map_err(|e| format_err!("unable to open sample file dir {}: {}", path, e))?;
        Ok(Arc::new(SampleFileDir {
            fd,
        }))
    }

    /// Opens the given sample file for reading.
    pub fn open_file(&self, composite_id: CompositeId) -> Result<fs::File, io::Error> {
        let p = SampleFileDir::get_rel_pathname(composite_id);
        unsafe { self.fd.openat(p.as_ptr(), libc::O_RDONLY, 0) }
    }

    pub fn create_file(&self, composite_id: CompositeId) -> Result<fs::File, io::Error> {
        let p = SampleFileDir::get_rel_pathname(composite_id);
        unsafe { self.fd.openat(p.as_ptr(), libc::O_WRONLY | libc::O_EXCL | libc::O_CREAT, 0o600) }
    }

    pub(crate) fn write_meta(&self, meta: &schema::DirMeta) -> Result<(), Error> {
        write_meta(&self.fd, meta)
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
    pub(crate) fn unlink_file(&self, id: CompositeId) -> Result<(), io::Error> {
        let p = SampleFileDir::get_rel_pathname(id);
        let res = unsafe { libc::unlinkat(self.fd.0, p.as_ptr(), 0) };
        if res < 0 {
            return Err(io::Error::last_os_error())
        }
        Ok(())
    }

    /// Syncs the directory itself.
    pub(crate) fn sync(&self) -> Result<(), io::Error> {
        self.fd.sync()
    }
}

/// Parse a composite id filename.
///
/// These are exactly 16 bytes, lowercase hex.
pub(crate) fn parse_id(id: &[u8]) -> Result<CompositeId, ()> {
    if id.len() != 16 {
        return Err(());
    }
    let mut v: u64 = 0;
    for i in 0..16 {
        v = (v << 4) | match id[i] {
            b @ b'0'..=b'9' => b - b'0',
            b @ b'a'..=b'f' => b - b'a' + 10,
            _ => return Err(()),
        } as u64;
    }
    Ok(CompositeId(v as i64))
}

#[cfg(test)]
mod tests {
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
