// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2019 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Filesystem utilities.

use nix::fcntl::OFlag;
use nix::sys::stat::Mode;
use nix::NixPath;
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
