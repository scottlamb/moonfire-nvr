// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2016 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Tools for implementing a `http_serve::Entity` body composed from many "slices".

use std::fmt;
use std::ops::Range;
use std::pin::Pin;

use crate::body::{wrap_error, BoxedError};
use base::format_err_t;
use failure::{bail, Error};
use futures::{stream, stream::StreamExt, Stream};
use tracing_futures::Instrument;

/// Gets a byte range given a context argument.
/// Each `Slice` instance belongs to a single `Slices`.
pub trait Slice: fmt::Debug + Sized + Sync + 'static {
    type Ctx: Send + Sync + Clone;
    type Chunk: Send + Sync;

    /// The byte position (relative to the start of the `Slices`) of the end of this slice,
    /// exclusive. Note the starting position (and thus length) are inferred from the previous
    /// slice. Must remain the same for the lifetime of the `Slice`.
    fn end(&self) -> u64;

    /// Gets the body bytes indicated by `r`, which is relative to this slice's start.
    /// The additional argument `ctx` is as supplied to the `Slices`.
    /// The additional argument `l` is the length of this slice, as determined by the `Slices`.
    fn get_range(
        &self,
        ctx: &Self::Ctx,
        r: Range<u64>,
        len: u64,
    ) -> Box<dyn Stream<Item = Result<Self::Chunk, BoxedError>> + Sync + Send>;

    fn get_slices(ctx: &Self::Ctx) -> &Slices<Self>;
}

/// Helper to serve byte ranges from a body which is broken down into many "slices".
/// This is used to implement `.mp4` serving in `mp4::File` from `mp4::Slice` enums.
pub struct Slices<S>
where
    S: Slice,
{
    /// The total byte length of the `Slices`.
    /// Equivalent to `self.slices.back().map(|s| s.end()).unwrap_or(0)`; kept for convenience and
    /// to avoid a branch.
    len: u64,

    /// 0 or more slices of this file.
    slices: Vec<S>,
}

impl<S> fmt::Debug for Slices<S>
where
    S: Slice,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} slices with overall length {}:",
            self.slices.len(),
            self.len
        )?;
        let mut start = 0;
        for (i, s) in self.slices.iter().enumerate() {
            let end = s.end();
            write!(
                f,
                "\ni {:7}: range [{:12}, {:12}) len {:12}: {:?}",
                i,
                start,
                end,
                end - start,
                s
            )?;
            start = end;
        }
        Ok(())
    }
}

impl<S> Slices<S>
where
    S: Slice,
{
    pub fn new() -> Self {
        Slices {
            len: 0,
            slices: Vec::new(),
        }
    }

    /// Reserves space for at least `additional` more slices to be appended.
    pub fn reserve(&mut self, additional: usize) {
        self.slices.reserve(additional)
    }

    /// Appends the given slice, which must have end > the Slices's current len.
    pub fn append(&mut self, slice: S) -> Result<(), Error> {
        if slice.end() <= self.len {
            bail!(
                "end {} <= len {} while adding slice {:?} to slices:\n{:?}",
                slice.end(),
                self.len,
                slice,
                self
            );
        }
        self.len = slice.end();
        self.slices.push(slice);
        Ok(())
    }

    /// Returns the total byte length of all slices.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Returns the number of slices.
    pub fn num(&self) -> usize {
        self.slices.len()
    }

    /// Writes `range` to `out`.
    /// This interface mirrors `http_serve::Entity::write_to`, with the additional `ctx` argument.
    pub fn get_range(
        &self,
        ctx: &S::Ctx,
        range: Range<u64>,
    ) -> Box<dyn Stream<Item = Result<S::Chunk, BoxedError>> + Sync + Send> {
        #[allow(clippy::suspicious_operation_groupings)]
        if range.start > range.end || range.end > self.len {
            return Box::new(stream::once(futures::future::err(wrap_error(
                format_err_t!(
                    Internal,
                    "Bad range {:?} for slice of length {}",
                    range,
                    self.len
                ),
            ))));
        }

        // Binary search for the first slice of the range to write, determining its index and
        // (from the preceding slice) the start of its range.
        let (i, slice_start) = match self.slices.binary_search_by_key(&range.start, |s| s.end()) {
            Ok(i) => (i + 1, self.slices[i].end()), // desired start == slice i's end; first is i+1!
            Err(i) if i == 0 => (0, 0),             // desired start < slice 0's end; first is 0.
            Err(i) => (i, self.slices[i - 1].end()), // desired start < slice i's end; first is i.
        };

        // Iterate through and write each slice until the end.

        let start_pos = range.start - slice_start;
        let bodies = stream::unfold(
            (ctx.clone(), i, start_pos, slice_start),
            move |(c, i, start_pos, slice_start)| {
                let (body, min_end);
                {
                    let self_ = S::get_slices(&c);
                    if i == self_.slices.len() {
                        return futures::future::ready(None);
                    }
                    let s = &self_.slices[i];
                    if range.end == slice_start + start_pos {
                        return futures::future::ready(None);
                    }
                    let s_end = s.end();
                    min_end = ::std::cmp::min(range.end, s_end);
                    let l = s_end - slice_start;
                    body = s.get_range(&c, start_pos..min_end - slice_start, l);
                };
                futures::future::ready(Some((Pin::from(body), (c, i + 1, 0, min_end))))
            },
        );
        Box::new(bodies.flatten().in_current_span())
    }
}

#[cfg(test)]
mod tests {
    use super::{Slice, Slices};
    use crate::body::BoxedError;
    use db::testutil;
    use futures::stream::{self, Stream, TryStreamExt};
    use std::ops::Range;
    use std::pin::Pin;

    #[derive(Debug, Eq, PartialEq)]
    pub struct FakeChunk {
        slice: &'static str,
        range: Range<u64>,
    }

    #[derive(Debug)]
    pub struct FakeSlice {
        end: u64,
        name: &'static str,
    }

    impl Slice for FakeSlice {
        type Ctx = &'static Slices<FakeSlice>;
        type Chunk = FakeChunk;

        fn end(&self) -> u64 {
            self.end
        }

        fn get_range(
            &self,
            _ctx: &&'static Slices<FakeSlice>,
            r: Range<u64>,
            _l: u64,
        ) -> Box<dyn Stream<Item = Result<FakeChunk, BoxedError>> + Send + Sync> {
            Box::new(stream::once(futures::future::ok(FakeChunk {
                slice: self.name,
                range: r,
            })))
        }

        fn get_slices(ctx: &&'static Slices<FakeSlice>) -> &'static Slices<Self> {
            ctx
        }
    }

    #[rustfmt::skip]
    static SLICES: once_cell::sync::Lazy<Slices<FakeSlice>> = once_cell::sync::Lazy::new(|| {
        let mut s = Slices::new();
        s.append(FakeSlice { end: 5,                    name: "a" }).unwrap();
        s.append(FakeSlice { end: 5 + 13,               name: "b" }).unwrap();
        s.append(FakeSlice { end: 5 + 13 + 7,           name: "c" }).unwrap();
        s.append(FakeSlice { end: 5 + 13 + 7 + 17,      name: "d" }).unwrap();
        s.append(FakeSlice { end: 5 + 13 + 7 + 17 + 19, name: "e" }).unwrap();
        s
    });

    async fn get_range(r: Range<u64>) -> Vec<FakeChunk> {
        Pin::from(SLICES.get_range(&&*SLICES, r))
            .try_collect()
            .await
            .unwrap()
    }

    #[test]
    pub fn size() {
        testutil::init();
        assert_eq!(5 + 13 + 7 + 17 + 19, SLICES.len());
    }

    #[tokio::test]
    pub async fn exact_slice() {
        // Test writing exactly slice b.
        testutil::init();
        let out = get_range(5..18).await;
        assert_eq!(
            &[FakeChunk {
                slice: "b",
                range: 0..13
            }],
            &out[..]
        );
    }

    #[tokio::test]
    pub async fn offset_first() {
        // Test writing part of slice a.
        testutil::init();
        let out = get_range(1..3).await;
        assert_eq!(
            &[FakeChunk {
                slice: "a",
                range: 1..3
            }],
            &out[..]
        );
    }

    #[tokio::test]
    pub async fn offset_mid() {
        // Test writing part of slice b, all of slice c, and part of slice d.
        testutil::init();
        let out = get_range(17..26).await;
        #[rustfmt::skip]
        assert_eq!(
            &[
                FakeChunk { slice: "b", range: 12..13 },
                FakeChunk { slice: "c", range: 0..7 },
                FakeChunk { slice: "d", range: 0..1 },
            ],
            &out[..]
        );
    }

    #[tokio::test]
    pub async fn everything() {
        // Test writing the whole Slices.
        testutil::init();
        let out = get_range(0..61).await;
        #[rustfmt::skip]
        assert_eq!(
            &[
                FakeChunk { slice: "a", range: 0..5 },
                FakeChunk { slice: "b", range: 0..13 },
                FakeChunk { slice: "c", range: 0..7 },
                FakeChunk { slice: "d", range: 0..17 },
                FakeChunk { slice: "e", range: 0..19 },
            ],
            &out[..]
        );
    }

    #[tokio::test]
    pub async fn at_end() {
        testutil::init();
        let out = get_range(61..61).await;
        let empty: &[FakeChunk] = &[];
        assert_eq!(empty, &out[..]);
    }
}
