// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

//! Reads sample files in a dedicated thread pool.
//!
//! Typically sample files are on spinning disk where IO operations take
//! ~10 ms on success. When disks fail, operations can stall for arbitrarily
//! long. POSIX doesn't have good support for asynchronous disk IO,
//! so it's desirable to do this from a dedicated pool for each disk rather
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
use std::{
    ops::Range,
    pin::Pin,
    task::{Context, Poll},
};

use base::bail;
use base::clock::{RealClocks, TimerGuard};
use base::{err, Error, ErrorKind, ResultExt};
use nix::{fcntl::OFlag, sys::stat::Mode};
use tokio::sync::mpsc;

use crate::CompositeId;

use super::{IoCommand, Pool, Worker};

pub(super) type Sender = mpsc::Sender<Result<SuccessfulRead, Error>>;
pub(super) type Receiver = mpsc::Receiver<Result<SuccessfulRead, Error>>;

pub struct Stream {
    state: StreamState,
    reply_rx: Receiver,
    pool: Pool,
}

enum StreamState {
    Idle(OpenReader),
    Reading,
    Error(Error),
    Fused,
}

impl Stream {
    /// Helper for reading during `poll_next`.
    fn read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Vec<u8>, Error>>> {
        match Pin::new(&mut self.reply_rx).poll_recv(cx) {
            Poll::Ready(None) => {
                self.state = StreamState::Fused;
                Poll::Ready(Some(Err(err!(
                    Internal,
                    msg("reader thread panicked; see logs")
                )
                .build())))
            }
            Poll::Ready(Some(Err(e))) => {
                self.state = StreamState::Fused;
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(Some(Ok(SuccessfulRead {
                chunk,
                file: Some(file),
            }))) => {
                self.state = StreamState::Idle(file);
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Ok(SuccessfulRead { chunk, file: None }))) => {
                self.state = StreamState::Fused;
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Pending => {
                self.state = StreamState::Reading;
                Poll::Pending
            }
        }
    }
}

impl futures::stream::Stream for Stream {
    type Item = Result<Vec<u8>, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match std::mem::replace(&mut self.state, StreamState::Fused) {
            StreamState::Idle(file) => {
                if let Err(e) = self
                    .pool
                    .send(super::is_open, IoCommand::ReadNextChunk { file })
                {
                    return Poll::Ready(Some(Err(e)));
                }

                // Try reading right away. It probably will return pending, but Receiver
                // needs to see the waker.
                self.read(cx)
            }
            StreamState::Error(e) => Poll::Ready(Some(Err(e))),
            StreamState::Reading => self.read(cx),
            StreamState::Fused => Poll::Ready(None),
        }
    }
}

pub(super) fn get_page_mask() -> usize {
    let page_size = usize::try_from(
        nix::unistd::sysconf(nix::unistd::SysconfVar::PAGE_SIZE)
            .expect("PAGE_SIZE fetch must succeed")
            .expect("PAGE_SIZE must be defined"),
    )
    .expect("PAGE_SIZE fits in usize");
    assert_eq!(page_size.count_ones(), 1, "invalid page size {page_size}");
    page_size - 1
}

/// An open, `mmap()`ed file for reading.
///
/// This is only actually used by the IO threads, but ownership is passed
/// around between them and the [`ReadStream`] to avoid maintaining extra data
/// structures.
///
/// At present, no effort is made to ensure this is *dropped* from a worker
/// thread; the assumption is the `munmap` call will not block.
pub(super) struct OpenReader {
    span: tracing::Span,

    composite_id: CompositeId,

    /// The memory-mapped region backed by the file. Valid up to length `map_len`.
    map_ptr: *mut libc::c_void,

    /// The position within the memory mapping. Invariant: `map_pos < map_len`.
    map_pos: usize,

    /// The length of the memory mapping. This may be less than the length of
    /// the file.
    map_len: usize,

    pub(super) reply_tx: Sender,
}

// Rust makes us manually state these because of the `*mut` ptr above.
unsafe impl Send for OpenReader {}
unsafe impl Sync for OpenReader {}

impl Drop for OpenReader {
    fn drop(&mut self) {
        if let Err(e) = unsafe { nix::sys::mman::munmap(self.map_ptr, self.map_len) } {
            // This should never happen.
            tracing::error!(
                "unable to munmap {}, {:?} len {}: {}",
                self.composite_id,
                self.map_ptr,
                self.map_len,
                e
            );
        }
    }
}

pub(super) struct SuccessfulRead {
    chunk: Vec<u8>,

    /// If this is not the final requested chunk, the `OpenFile` for next time.
    file: Option<OpenReader>,
}

impl Pool {
    /// Returns a handle for reading `range` from the given recording.
    ///
    /// The actual OS-level `open` call occurs asynchronously from the directory thread;
    /// thus any errors will be returned on the `Stream` later.
    pub fn open_file(self, composite_id: CompositeId, range: Range<u64>) -> Stream {
        let (reply_tx, reply_rx) = mpsc::channel(1);
        if range.is_empty() {
            return Stream {
                state: StreamState::Fused,
                pool: self,
                reply_rx,
            };
        }
        if let Err(e) = self.send(
            super::is_open,
            IoCommand::OpenForReading {
                span: tracing::Span::current(),
                composite_id,
                range,
                reply_tx,
            },
        ) {
            return Stream {
                state: StreamState::Error(e),
                pool: self,
                reply_rx,
            };
        }
        Stream {
            state: StreamState::Reading,
            pool: self,
            reply_rx,
        }
    }
}

impl Worker {
    pub(super) fn open_for_reading(
        &self,
        span: tracing::Span,
        composite_id: CompositeId,
        range: Range<u64>,
        reply_tx: Sender,
    ) {
        if reply_tx.is_closed() {
            return;
        }
        let tx = reply_tx.clone();
        let _ = tx.try_send(self.open_for_reading_inner(span, composite_id, range, reply_tx));
    }

    pub(super) fn open_for_reading_inner(
        &self,
        span: tracing::Span,
        composite_id: CompositeId,
        range: Range<u64>,
        reply_tx: Sender,
    ) -> Result<SuccessfulRead, Error> {
        let span2 = span.clone();
        let _span_enter = span2.enter();
        let _timer_guard = TimerGuard::new(&RealClocks {}, |location| {
            format!("open {composite_id} at {location}")
        });
        let p = super::CompositeIdPath::from(composite_id);

        // Reader::open_file checks for an empty range, but check again right
        // before the unsafe block to make it easier to audit the safety constraints.
        assert!(range.start < range.end);

        // mmap offsets must be aligned to page size boundaries.
        let unaligned = (range.start as usize) & self.page_mask;
        let offset = libc::off_t::try_from(range.start).expect("range.start fits in off_t")
            - libc::off_t::try_from(unaligned).expect("usize fits in off_t");

        // Recordings from very high bitrate streams could theoretically exceed exhaust a 32-bit
        // machine's address space, causing either this usize::MAX error or mmap
        // failure. If that happens in practice, we'll have to stop mmap()ing
        // the whole range.
        let map_len = usize::try_from(
            range.end - range.start + u64::try_from(unaligned).expect("usize fits in u64"),
        )
        .map_err(|e| {
            err!(
                OutOfRange,
                msg("file {composite_id}'s range {range:?} len exceeds usize::MAX"),
                source(e),
            )
        })?;
        let map_len = std::num::NonZeroUsize::new(map_len).expect("range is non-empty");

        let file =
            crate::fs::openat(self.dir.0, &p, OFlag::O_RDONLY, Mode::empty()).map_err(|e| {
                err!(
                    e,
                    msg("unable to open recording {composite_id} for reading")
                )
            })?;

        // Check the actual on-disk file length. It's an error (a bug or filesystem corruption)
        // for it to be less than the requested read. Check for this now rather than crashing
        // with a SIGBUS or reading bad data at the end of the last page later.
        let metadata = file.metadata().err_kind(ErrorKind::Unknown)?;
        if metadata.len() < u64::try_from(offset).unwrap() + u64::try_from(map_len.get()).unwrap() {
            bail!(
                OutOfRange,
                msg(
                    "file {}, range {:?}, len {}",
                    composite_id,
                    range,
                    metadata.len()
                ),
            );
        }
        let map_ptr = unsafe {
            nix::sys::mman::mmap(
                None,
                map_len,
                nix::sys::mman::ProtFlags::PROT_READ,
                nix::sys::mman::MapFlags::MAP_SHARED,
                Some(&file),
                offset,
            )
        }
        .map_err(|e| {
            err!(
                e,
                msg("mmap failed for {composite_id} off={offset} len={map_len}")
            )
        })?;

        if let Err(err) = unsafe {
            nix::sys::mman::madvise(
                map_ptr,
                map_len.get(),
                nix::sys::mman::MmapAdvise::MADV_SEQUENTIAL,
            )
        } {
            // This shouldn't happen but is "just" a performance problem.
            tracing::warn!(
                %err,
                %composite_id,
                offset,
                map_len,
                "madvise(MADV_SEQUENTIAL) failed",
            );
        }

        Ok(self.read_chunk_inner(OpenReader {
            span,
            composite_id,
            map_ptr,
            map_pos: unaligned,
            map_len: map_len.get(),
            reply_tx,
        }))
    }

    pub(super) fn read_chunk(&self, file: OpenReader) {
        if file.reply_tx.is_closed() {
            return;
        }
        let composite_id = file.composite_id;
        let _guard = TimerGuard::new(&RealClocks {}, |location| {
            format!("read from {composite_id} at {location}")
        });
        let tx = file.reply_tx.clone();
        let span2 = file.span.clone();
        let _enter = span2.enter();
        let _ = tx.try_send(Ok(self.read_chunk_inner(file)));
    }

    pub(super) fn read_chunk_inner(&self, mut file: OpenReader) -> SuccessfulRead {
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
    use std::sync::Arc;

    use futures::TryStreamExt;
    use uuid::Uuid;

    #[tokio::test]
    async fn basic() {
        crate::testutil::init();
        let tmpdir = tempfile::Builder::new()
            .prefix("moonfire-db-test-reader")
            .tempdir()
            .unwrap();
        let one = const { std::num::NonZeroUsize::new(1).unwrap() };
        let pool = crate::dir::Pool::new(
            crate::dir::Config {
                path: tmpdir.path().to_owned(),
                db_uuid: Uuid::now_v7(),
                dir_uuid: Uuid::now_v7(),
                last_complete_open: None,
                current_open: Some(crate::db::Open {
                    uuid: Uuid::now_v7(),
                    id: 1,
                }),
                flusher_notify: Arc::new(tokio::sync::Notify::new()),
            },
            base::FastHashSet::default(),
        );
        pool.open(one).await.unwrap();
        pool.complete_open_for_write().await.unwrap();
        std::fs::write(tmpdir.path().join("0123456789abcdef"), b"blah blah").unwrap();
        let f = pool
            .clone()
            .open_file(crate::CompositeId(0x0123_4567_89ab_cdef), 1..8);
        assert_eq!(f.try_concat().await.unwrap(), b"lah bla");
        pool.close().await.unwrap();
    }
}
