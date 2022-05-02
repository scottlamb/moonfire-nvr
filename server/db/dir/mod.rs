// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

//! Sample file directory management.
//!
//! This mostly includes opening a directory and looking for recordings within it.
//! Updates to the directory happen through [crate::writer].

mod reader;

use crate::coding;
use crate::db::CompositeId;
use crate::schema;
use cstr::cstr;
use failure::{bail, format_err, Error, Fail};
use log::warn;
use nix::sys::statvfs::Statvfs;
use nix::{
    fcntl::{FlockArg, OFlag},
    sys::stat::Mode,
    NixPath,
};
use protobuf::Message;
use std::ffi::CStr;
use std::fs;
use std::io::{Read, Write};
use std::ops::Range;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::Path;
use std::sync::Arc;

/// The fixed length of a directory's `meta` file.
///
/// See `DirMeta` comments within `proto/schema.proto` for more explanation.
const FIXED_DIR_META_LEN: usize = 512;

/// A sample file directory. Typically one per physical disk drive.
///
/// If the directory is used for writing, [crate::writer::start_syncer] should be
/// called to start a background thread. This thread manages deleting files and
/// writing new files. It synces the directory and commits these operations to
/// the database in the correct order to maintain the invariants described in
/// `design/schema.md`.
#[derive(Debug)]
pub struct SampleFileDir {
    /// The open file descriptor for the directory. The worker created by
    /// [crate::writer::start_syncer] uses it to create files and sync the
    /// directory. Other threads use it to open sample files for reading during
    /// video serving.
    pub(crate) fd: Arc<Fd>,

    reader: reader::Reader,
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

/// A file descriptor associated with a directory (not necessarily the sample file dir).
#[derive(Debug)]
pub struct Fd(std::os::unix::io::RawFd);

impl std::os::unix::io::AsRawFd for Fd {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.0
    }
}

impl Drop for Fd {
    fn drop(&mut self) {
        if let Err(e) = nix::unistd::close(self.0) {
            warn!("Unable to close sample file dir: {}", e);
        }
    }
}

impl Fd {
    /// Opens the given path as a directory.
    pub fn open<P: ?Sized + NixPath>(path: &P, mkdir: bool) -> Result<Fd, nix::Error> {
        if mkdir {
            match nix::unistd::mkdir(path, nix::sys::stat::Mode::S_IRWXU) {
                Ok(()) | Err(nix::Error::EEXIST) => {}
                Err(e) => return Err(e),
            }
        }
        let fd = nix::fcntl::open(path, OFlag::O_DIRECTORY | OFlag::O_RDONLY, Mode::empty())?;
        Ok(Fd(fd))
    }

    /// `fsync`s this directory, causing all file metadata to be committed to permanent storage.
    pub(crate) fn sync(&self) -> Result<(), nix::Error> {
        nix::unistd::fsync(self.0)
    }

    /// Locks the directory with the specified `flock` operation.
    pub fn lock(&self, arg: FlockArg) -> Result<(), nix::Error> {
        nix::fcntl::flock(self.0, arg)
    }

    /// Returns information about the filesystem on which this directory lives.
    pub fn statfs(&self) -> Result<nix::sys::statvfs::Statvfs, nix::Error> {
        nix::sys::statvfs::fstatvfs(self)
    }
}

/// Reads `dir`'s metadata. If none is found, returns an empty proto.
pub(crate) fn read_meta(dir: &Fd) -> Result<schema::DirMeta, Error> {
    let mut meta = schema::DirMeta::default();
    let mut f = match crate::fs::openat(dir.0, cstr!("meta"), OFlag::O_RDONLY, Mode::empty()) {
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
        .map_err(|_| format_err!("Unable to decode varint length in meta file"))?;
    if data.len() != FIXED_DIR_META_LEN || len as usize + pos > FIXED_DIR_META_LEN {
        bail!(
            "Expected a {}-byte file with a varint length of a DirMeta message; got \
            a {}-byte file with length {}",
            FIXED_DIR_META_LEN,
            data.len(),
            len
        );
    }
    let data = &data[pos..pos + len as usize];
    let mut s = protobuf::CodedInputStream::from_bytes(&data);
    meta.merge_from(&mut s)
        .map_err(|e| e.context("Unable to parse metadata proto"))?;
    Ok(meta)
}

/// Writes `dirfd`'s metadata, clobbering existing data.
pub(crate) fn write_meta(dirfd: RawFd, meta: &schema::DirMeta) -> Result<(), Error> {
    let mut data = meta
        .write_length_delimited_to_bytes()
        .expect("proto3->vec is infallible");
    if data.len() > FIXED_DIR_META_LEN {
        bail!(
            "Length-delimited DirMeta message requires {} bytes, over limit of {}",
            data.len(),
            FIXED_DIR_META_LEN
        );
    }
    data.resize(FIXED_DIR_META_LEN, 0); // pad to required length.
    let mut f = crate::fs::openat(
        dirfd,
        cstr!("meta"),
        OFlag::O_CREAT | OFlag::O_WRONLY,
        Mode::S_IRUSR | Mode::S_IWUSR,
    )
    .map_err(|e| e.context("Unable to open meta file"))?;
    let stat = f
        .metadata()
        .map_err(|e| e.context("Unable to stat meta file"))?;
    if stat.len() == 0 {
        // Need to sync not only the data but also the file metadata and dirent.
        f.write_all(&data)
            .map_err(|e| e.context("Unable to write to meta file"))?;
        f.sync_all()
            .map_err(|e| e.context("Unable to sync meta file"))?;
        nix::unistd::fsync(dirfd).map_err(|e| e.context("Unable to sync dir"))?;
    } else if stat.len() == FIXED_DIR_META_LEN as u64 {
        // Just syncing the data will suffice; existing metadata and dirent are fine.
        f.write_all(&data)
            .map_err(|e| e.context("Unable to write to meta file"))?;
        f.sync_data()
            .map_err(|e| e.context("Unable to sync meta file"))?;
    } else {
        bail!(
            "Existing meta file is {}-byte; expected {}",
            stat.len(),
            FIXED_DIR_META_LEN
        );
    }
    Ok(())
}

impl SampleFileDir {
    /// Opens the directory using the given metadata.
    ///
    /// `db_meta.in_progress_open` should be filled if the directory should be opened in read/write
    /// mode; absent in read-only mode.
    pub fn open(path: &Path, expected_meta: &schema::DirMeta) -> Result<Arc<SampleFileDir>, Error> {
        let read_write = expected_meta.in_progress_open.is_some();
        let s = SampleFileDir::open_self(path, false)?;
        s.fd.lock(if read_write {
            FlockArg::LockExclusiveNonblock
        } else {
            FlockArg::LockSharedNonblock
        })
        .map_err(|e| e.context(format!("unable to lock dir {}", path.display())))?;
        let dir_meta = read_meta(&s.fd).map_err(|e| e.context("unable to read meta file"))?;
        if let Err(e) = SampleFileDir::check_consistent(expected_meta, &dir_meta) {
            bail!(
                "metadata mismatch: {}.\nexpected:\n{:#?}\n\nactual:\n{:#?}",
                e,
                expected_meta,
                &dir_meta
            );
        }
        if expected_meta.in_progress_open.is_some() {
            s.write_meta(expected_meta)?;
        }
        Ok(s)
    }

    /// Checks that the existing directory and database metadata are consistent; the directory
    /// is then openable.
    pub(crate) fn check_consistent(
        expected_meta: &schema::DirMeta,
        actual_meta: &schema::DirMeta,
    ) -> Result<(), String> {
        if actual_meta.db_uuid != expected_meta.db_uuid {
            return Err("db uuid mismatch".into());
        }
        if actual_meta.dir_uuid != expected_meta.dir_uuid {
            return Err("dir uuid mismatch".into());
        }

        if expected_meta.last_complete_open.is_some()
            && (expected_meta.last_complete_open != actual_meta.last_complete_open
                && expected_meta.last_complete_open != actual_meta.in_progress_open)
        {
            return Err(format!(
                "expected open {:?}; but got {:?} (complete) or {:?} (in progress)",
                &expected_meta.last_complete_open,
                &actual_meta.last_complete_open,
                &actual_meta.in_progress_open,
            ));
        }

        if expected_meta.last_complete_open.is_none() && actual_meta.last_complete_open.is_some() {
            return Err("expected never opened".into());
        }

        Ok(())
    }

    pub(crate) fn create(
        path: &Path,
        db_meta: &schema::DirMeta,
    ) -> Result<Arc<SampleFileDir>, Error> {
        let s = SampleFileDir::open_self(path, true)?;
        s.fd.lock(FlockArg::LockExclusiveNonblock)
            .map_err(|e| e.context(format!("unable to lock dir {}", path.display())))?;
        let old_meta = read_meta(&s.fd)?;

        // Verify metadata. We only care that it hasn't been completely opened.
        // Partial opening by this or another database is fine; we won't overwrite anything.
        if old_meta.last_complete_open.is_some() {
            bail!(
                "Can't create dir at path {}: is already in use:\n{:?}",
                path.display(),
                old_meta
            );
        }
        if !s.is_empty()? {
            bail!(
                "Can't create dir at path {} with existing files",
                path.display()
            );
        }
        s.write_meta(db_meta)?;
        Ok(s)
    }

    pub(crate) fn opendir(&self) -> Result<nix::dir::Dir, nix::Error> {
        nix::dir::Dir::openat(
            self.fd.as_raw_fd(),
            ".",
            OFlag::O_DIRECTORY | OFlag::O_RDONLY,
            Mode::empty(),
        )
    }

    /// Determines if the directory is empty, aside form metadata.
    pub(crate) fn is_empty(&self) -> Result<bool, Error> {
        let mut dir = self.opendir()?;
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

    fn open_self(path: &Path, create: bool) -> Result<Arc<SampleFileDir>, Error> {
        let fd = Arc::new(Fd::open(path, create)?);
        let reader = reader::Reader::spawn(path, fd.clone());
        Ok(Arc::new(SampleFileDir { fd, reader }))
    }

    /// Opens the given sample file for reading.
    pub fn open_file(&self, composite_id: CompositeId, range: Range<u64>) -> reader::FileStream {
        self.reader.open_file(composite_id, range)
    }

    pub fn create_file(&self, composite_id: CompositeId) -> Result<fs::File, nix::Error> {
        let p = CompositeIdPath::from(composite_id);
        crate::fs::openat(
            self.fd.0,
            &p,
            OFlag::O_WRONLY | OFlag::O_EXCL | OFlag::O_CREAT,
            Mode::S_IRUSR | Mode::S_IWUSR,
        )
    }

    pub(crate) fn write_meta(&self, meta: &schema::DirMeta) -> Result<(), Error> {
        write_meta(self.fd.0, meta)
    }

    pub fn statfs(&self) -> Result<Statvfs, nix::Error> {
        self.fd.statfs()
    }

    /// Unlinks the given sample file within this directory.
    pub(crate) fn unlink_file(&self, id: CompositeId) -> Result<(), nix::Error> {
        let p = CompositeIdPath::from(id);
        nix::unistd::unlinkat(Some(self.fd.0), &p, nix::unistd::UnlinkatFlags::NoRemoveDir)
    }

    /// Syncs the directory itself.
    pub(crate) fn sync(&self) -> Result<(), nix::Error> {
        self.fd.sync()
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
            o.id = u32::max_value();
            o.uuid.extend_from_slice(fake_uuid);
        }
        {
            let o = meta.in_progress_open.mut_or_insert_default();
            o.id = u32::max_value();
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
