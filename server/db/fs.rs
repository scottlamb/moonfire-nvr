// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2019 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Filesystem utilities.

use nix::fcntl::{FlockArg, OFlag};
use nix::sys::stat::Mode;
use nix::unistd::UnlinkatFlags;
use nix::NixPath;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::io::{FromRawFd, RawFd};

/// Opens the given `path` within `dirfd` with the specified flags.
pub fn openat<P: ?Sized + NixPath>(
    dirfd: RawFd,
    path: &P,
    oflag: OFlag,
    mode: Mode,
) -> Result<std::fs::File, nix::Error> {
    let fd = nix::fcntl::openat(dirfd, path, oflag, mode)?;
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// A file descriptor associated with a directory (not necessarily the sample file dir).
#[derive(Debug)]
pub struct Dir(pub std::os::unix::io::RawFd);

impl AsFd for Dir {
    fn as_fd(&self) -> std::os::unix::prelude::BorrowedFd<'_> {
        unsafe { BorrowedFd::borrow_raw(self.0) }
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        if let Err(err) = nix::unistd::close(self.0) {
            tracing::warn!(%err, "unable to close directory");
        }
    }
}

impl Dir {
    /// Opens the given path as a directory.
    pub fn open<P: ?Sized + NixPath>(path: &P, mkdir: bool) -> Result<Dir, nix::Error> {
        if mkdir {
            match nix::unistd::mkdir(path, nix::sys::stat::Mode::S_IRWXU) {
                Ok(()) | Err(nix::Error::EEXIST) => {}
                Err(e) => return Err(e),
            }
        }
        let fd = nix::fcntl::open(path, OFlag::O_DIRECTORY | OFlag::O_RDONLY, Mode::empty())?;
        Ok(Dir(fd))
    }

    /// Locks the directory with the specified `flock` operation.
    pub fn lock(&self, arg: FlockArg) -> Result<(), nix::Error> {
        nix::fcntl::flock(self.0, arg)
    }

    /// Returns information about the filesystem on which this directory lives.
    pub fn statfs(&self) -> Result<nix::sys::statvfs::Statvfs, nix::Error> {
        nix::sys::statvfs::fstatvfs(self)
    }

    pub fn unlink<P: ?Sized + NixPath>(
        &self,
        path: &P,
        flags: UnlinkatFlags,
    ) -> Result<(), nix::Error> {
        nix::unistd::unlinkat(Some(self.0), path, flags)
    }

    pub fn opendir(&self) -> Result<nix::dir::Dir, nix::Error> {
        nix::dir::Dir::openat(
            self.0,
            ".",
            OFlag::O_DIRECTORY | OFlag::O_RDONLY,
            Mode::empty(),
        )
    }
}
