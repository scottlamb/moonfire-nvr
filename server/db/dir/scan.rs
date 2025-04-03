// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2026 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

use std::ffi::CStr;

use base::Error;
use nix::fcntl::AtFlags;

use crate::CompositeId;

use super::{Worker, WorkerCtx};

impl WorkerCtx<'_> {
    pub fn iterator(&self) -> Result<Iterator<'_>, Error> {
        Ok(Iterator {
            worker: self.0,
            dir: self.0.dir.opendir()?.into_iter(),
        })
    }
}

pub struct Iterator<'w> {
    worker: &'w Worker,
    dir: nix::dir::OwningIter,
}

impl Iterator<'_> {
    #[allow(clippy::should_implement_trait)] // `std::iter::Iterator::Item` can't borrow from `Self`.
    pub fn next(&mut self) -> Option<Result<File<'_>, nix::Error>> {
        loop {
            return match self.dir.next() {
                Some(Ok(entry)) => {
                    if matches!(entry.file_name().to_bytes(), b"." | b".." | b"meta") {
                        continue;
                    }
                    Some(Ok(File {
                        worker: self.worker,
                        entry,
                    }))
                }
                Some(Err(err)) => Some(Err(err)),
                None => None,
            };
        }
    }
}

pub struct File<'w> {
    worker: &'w Worker,
    entry: nix::dir::Entry,
}

impl File<'_> {
    pub fn recording_id(&self) -> Result<CompositeId, &CStr> {
        let file_name = self.entry.file_name();
        super::parse_id(file_name.to_bytes()).map_err(|_| file_name)
    }

    pub fn size(&self) -> Result<u64, nix::Error> {
        nix::sys::stat::fstatat(self.worker.dir.0, self.entry.file_name(), AtFlags::empty())
            .map(|stat| stat.st_size as u64)
    }
}
