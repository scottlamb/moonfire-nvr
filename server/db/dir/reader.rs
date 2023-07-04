// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

//! Reads sample files in a dedicated thread.
//!
//! Typically sample files are on spinning disk where IO operations take
//! ~10 ms on success. When disks fail, operations can stall for arbitrarily
//! long. POSIX doesn't have good support for asynchronous disk IO,
//! so it's desirable to do this from a dedicated thread for each disk rather
//! than stalling the tokio IO threads or (as when using `tokio::fs`) creating
//! unbounded numbers of workers.
//!
//! This also has some minor theoretical efficiency advantages over
//! `tokio::fs::File`:
//! *   it uses `mmap`, which means fewer system calls and a somewhat faster
//!     userspace `memcpy` implementation (see [Why mmap is faster than system
//!     calls](https://sasha-f.medium.com/why-mmap-is-faster-than-system-calls-24718e75ab37).)
//! *   it has fewer thread handoffs because it batches operations on open
//!     (open, fstat, mmap, madvise, close, memcpy first chunk) and close
//!     (memcpy last chunk, munmap).

use std::convert::TryFrom;
use std::future::Future;
use std::os::unix::prelude::AsRawFd;
use std::path::Path;
use std::{
    ops::Range,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use base::bail_t;
use base::clock::{RealClocks, TimerGuard};
use base::{format_err_t, Error, ErrorKind, ResultExt};
use nix::{fcntl::OFlag, sys::stat::Mode};

use crate::CompositeId;

/// Handle for a reader thread, used to send it commands.
///
/// The reader will shut down after the last handle is closed.
#[derive(Clone, Debug)]
pub(super) struct Reader(tokio::sync::mpsc::UnboundedSender<ReaderCommand>);

impl Reader {
    pub(super) fn spawn(path: &Path, dir: Arc<super::Fd>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let page_size = usize::try_from(
            nix::unistd::sysconf(nix::unistd::SysconfVar::PAGE_SIZE)
                .expect("PAGE_SIZE fetch must succeed")
                .expect("PAGE_SIZE must be defined"),
        )
        .expect("PAGE_SIZE fits in usize");
        assert_eq!(page_size.count_ones(), 1, "invalid page size {page_size}");
        std::thread::Builder::new()
            .name(format!("r-{}", path.display()))
            .spawn(move || ReaderInt { dir, page_size }.run(rx))
            .expect("unable to create reader thread");
        Self(tx)
    }

    pub(super) fn open_file(&self, composite_id: CompositeId, range: Range<u64>) -> FileStream {
        if range.is_empty() {
            return FileStream {
                state: FileStreamState::Invalid,
                reader: Reader(self.0.clone()),
            };
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send(ReaderCommand::OpenFile {
            composite_id,
            range,
            tx,
        });
        FileStream {
            state: FileStreamState::Reading(rx),
            reader: Reader(self.0.clone()),
        }
    }

    fn send(&self, cmd: ReaderCommand) {
        self.0
            .send(cmd)
            .map_err(|_| ())
            .expect("reader thread panicked; see logs.");
    }
}

pub struct FileStream {
    state: FileStreamState,
    reader: Reader,
}

type ReadReceiver = tokio::sync::oneshot::Receiver<Result<SuccessfulRead, Error>>;

enum FileStreamState {
    Idle(OpenFile),
    Reading(ReadReceiver),
    Invalid,
}

impl FileStream {
    /// Helper for reading during `poll_next`.
    fn read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut rx: ReadReceiver,
    ) -> Poll<Option<Result<Vec<u8>, Error>>> {
        match Pin::new(&mut rx).poll(cx) {
            Poll::Ready(Err(_)) => {
                self.state = FileStreamState::Invalid;
                Poll::Ready(Some(Err(format_err_t!(
                    Internal,
                    "reader thread panicked; see logs"
                ))))
            }
            Poll::Ready(Ok(Err(e))) => {
                self.state = FileStreamState::Invalid;
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(Ok(Ok(SuccessfulRead {
                chunk,
                file: Some(file),
            }))) => {
                self.state = FileStreamState::Idle(file);
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Ok(Ok(SuccessfulRead { chunk, file: None }))) => {
                self.state = FileStreamState::Invalid;
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Pending => {
                self.state = FileStreamState::Reading(rx);
                Poll::Pending
            }
        }
    }
}

impl futures::stream::Stream for FileStream {
    type Item = Result<Vec<u8>, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match std::mem::replace(&mut self.state, FileStreamState::Invalid) {
            FileStreamState::Idle(file) => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                self.reader.send(ReaderCommand::ReadNextChunk { file, tx });

                // Try reading right away. It probably will return pending, but Receiver
                // needs to see the waker.
                self.read(cx, rx)
            }
            FileStreamState::Reading(rx) => self.read(cx, rx),
            FileStreamState::Invalid => Poll::Ready(None),
        }
    }
}

impl Drop for FileStream {
    fn drop(&mut self) {
        use FileStreamState::{Idle, Invalid};
        if let Idle(file) = std::mem::replace(&mut self.state, Invalid) {
            // This will succeed unless reader has panicked. If that happened,
            // the logfiles will be loud anyway; no need to add additional
            // error messages.
            let _ = self.reader.0.send(ReaderCommand::CloseFile(file));
        }
    }
}

/// An open, `mmap()`ed file.
///
/// This is only actually used by the reader thread, but ownership is passed
/// around between it and the [FileStream] to avoid maintaining extra data
/// structures.
struct OpenFile {
    composite_id: CompositeId,

    /// The memory-mapped region backed by the file. Valid up to length `map_len`.
    map_ptr: *mut libc::c_void,

    /// The position within the memory mapping. Invariant: `map_pos < map_len`.
    map_pos: usize,

    /// The length of the memory mapping. This may be less than the length of
    /// the file.
    map_len: usize,
}

// Rust makes us manually state these because of the `*mut` ptr above.
unsafe impl Send for OpenFile {}
unsafe impl Sync for OpenFile {}

impl Drop for OpenFile {
    fn drop(&mut self) {
        if let Err(e) = unsafe { nix::sys::mman::munmap(self.map_ptr, self.map_len) } {
            // This should never happen.
            log::error!(
                "unable to munmap {}, {:?} len {}: {}",
                self.composite_id,
                self.map_ptr,
                self.map_len,
                e
            );
        }
    }
}

struct SuccessfulRead {
    chunk: Vec<u8>,

    /// If this is not the final requested chunk, the `OpenFile` for next time.
    file: Option<OpenFile>,
}

enum ReaderCommand {
    /// Opens a file and reads the first chunk.
    OpenFile {
        composite_id: CompositeId,
        range: std::ops::Range<u64>,
        tx: tokio::sync::oneshot::Sender<Result<SuccessfulRead, Error>>,
    },

    /// Reads the next chunk of the file.
    ReadNextChunk {
        file: OpenFile,
        tx: tokio::sync::oneshot::Sender<Result<SuccessfulRead, Error>>,
    },

    /// Closes the file early, as when the [FileStream] is dropped before completing.
    CloseFile(OpenFile),
}

struct ReaderInt {
    /// File descriptor of the sample file directory.
    dir: Arc<super::Fd>,

    /// The page size as returned by `sysconf`; guaranteed to be a power of two.
    page_size: usize,
}

impl ReaderInt {
    fn run(self, mut rx: tokio::sync::mpsc::UnboundedReceiver<ReaderCommand>) {
        while let Some(cmd) = rx.blocking_recv() {
            // OpenFile's Drop implementation takes care of closing the file on error paths and
            // the CloseFile operation.
            match cmd {
                ReaderCommand::OpenFile {
                    composite_id,
                    range,
                    tx,
                } => {
                    if tx.is_closed() {
                        // avoid spending effort on expired commands
                        continue;
                    }
                    let _guard = TimerGuard::new(&RealClocks {}, || format!("open {composite_id}"));
                    let _ = tx.send(self.open(composite_id, range));
                }
                ReaderCommand::ReadNextChunk { file, tx } => {
                    if tx.is_closed() {
                        // avoid spending effort on expired commands
                        continue;
                    }
                    let composite_id = file.composite_id;
                    let _guard =
                        TimerGuard::new(&RealClocks {}, || format!("read from {composite_id}"));
                    let _ = tx.send(Ok(self.chunk(file)));
                }
                ReaderCommand::CloseFile(_) => {}
            }
        }
    }

    fn open(&self, composite_id: CompositeId, range: Range<u64>) -> Result<SuccessfulRead, Error> {
        let p = super::CompositeIdPath::from(composite_id);

        // Reader::open_file checks for an empty range, but check again right
        // before the unsafe block to make it easier to audit the safety constraints.
        assert!(range.start < range.end);

        // mmap offsets must be aligned to page size boundaries.
        let unaligned = (range.start as usize) & (self.page_size - 1);
        let offset = libc::off_t::try_from(range.start).expect("range.start fits in off_t")
            - libc::off_t::try_from(unaligned).expect("usize fits in off_t");

        // Recordings from very high bitrate streams could theoretically exceed exhaust a 32-bit
        // machine's address space, causing either this usize::MAX error or mmap
        // failure. If that happens in practice, we'll have to stop mmap()ing
        // the whole range.
        let map_len = usize::try_from(
            range.end - range.start + u64::try_from(unaligned).expect("usize fits in u64"),
        )
        .map_err(|_| {
            format_err_t!(
                OutOfRange,
                "file {}'s range {:?} len exceeds usize::MAX",
                composite_id,
                range
            )
        })?;
        let map_len = std::num::NonZeroUsize::new(map_len).expect("range is non-empty");

        let file = crate::fs::openat(self.dir.0, &p, OFlag::O_RDONLY, Mode::empty())
            .err_kind(ErrorKind::Unknown)?;

        // Check the actual on-disk file length. It's an error (a bug or filesystem corruption)
        // for it to be less than the requested read. Check for this now rather than crashing
        // with a SIGBUS or reading bad data at the end of the last page later.
        let metadata = file.metadata().err_kind(ErrorKind::Unknown)?;
        if metadata.len() < u64::try_from(offset).unwrap() + u64::try_from(map_len.get()).unwrap() {
            bail_t!(
                Internal,
                "file {}, range {:?}, len {}",
                composite_id,
                range,
                metadata.len()
            );
        }
        let map_ptr = unsafe {
            nix::sys::mman::mmap(
                None,
                map_len,
                nix::sys::mman::ProtFlags::PROT_READ,
                nix::sys::mman::MapFlags::MAP_SHARED,
                file.as_raw_fd(),
                offset,
            )
        }
        .map_err(|e| {
            format_err_t!(
                Internal,
                "mmap failed for {} off={} len={}: {}",
                composite_id,
                offset,
                map_len,
                e
            )
        })?;

        if let Err(e) = unsafe {
            nix::sys::mman::madvise(
                map_ptr,
                map_len.get(),
                nix::sys::mman::MmapAdvise::MADV_SEQUENTIAL,
            )
        } {
            // This shouldn't happen but is "just" a performance problem.
            log::warn!(
                "madvise(MADV_SEQUENTIAL) failed for {} off={} len={}: {}",
                composite_id,
                offset,
                map_len,
                e
            );
        }

        Ok(self.chunk(OpenFile {
            composite_id,
            map_ptr,
            map_pos: unaligned,
            map_len: map_len.get(),
        }))
    }

    fn chunk(&self, mut file: OpenFile) -> SuccessfulRead {
        // Read a chunk that's large enough to minimize thread handoffs but
        // short enough to keep memory usage under control. It's hopefully
        // unnecessary to worry about disk seeks; the madvise call should cause
        // the kernel to read ahead.
        let end = std::cmp::min(file.map_len, file.map_pos.saturating_add(1 << 16));
        let mut chunk = Vec::new();
        let len = end.checked_sub(file.map_pos).unwrap();
        chunk.reserve_exact(len);

        // SAFETY: [map_pos, map_pos + len) is verified to be within map_ptr.
        //
        // If the read is out of bounds of the file, we'll get a SIGBUS.
        // That's not a safety violation. It also shouldn't happen because the
        // length was set properly at open time, Moonfire NVR is a closed
        // system (nothing else ever touches its files), and sample files are
        // never truncated (only appended to or unlinked).
        unsafe {
            std::ptr::copy_nonoverlapping(
                file.map_ptr.add(file.map_pos) as *const u8,
                chunk.as_mut_ptr(),
                len,
            );
            chunk.set_len(len);
        }
        let file = if end == file.map_len {
            None
        } else {
            file.map_pos = end;
            Some(file)
        };
        SuccessfulRead { chunk, file }
    }
}

#[cfg(test)]
mod tests {
    use futures::TryStreamExt;

    #[tokio::test]
    async fn basic() {
        crate::testutil::init();
        let tmpdir = tempfile::Builder::new()
            .prefix("moonfire-db-test-reader")
            .tempdir()
            .unwrap();
        let fd = std::sync::Arc::new(super::super::Fd::open(tmpdir.path(), false).unwrap());
        let reader = super::Reader::spawn(tmpdir.path(), fd);
        std::fs::write(tmpdir.path().join("0123456789abcdef"), b"blah blah").unwrap();
        let f = reader.open_file(crate::CompositeId(0x0123_4567_89ab_cdef), 1..8);
        assert_eq!(f.try_concat().await.unwrap(), b"lah bla");
    }
}
