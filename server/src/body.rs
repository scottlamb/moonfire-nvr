// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

//! HTTP body implementation using `ARefss<'static, [u8]>` chunks.
//!
//! Moonfire NVR uses this custom chunk type rather than [bytes::Bytes]. This
//! is mostly for historical reasons: we used to use `mmap`-backed chunks.
//! The custom chunk type also helps minimize reference-counting in `mp4::File`
//! as described [here](https://github.com/tokio-rs/bytes/issues/359#issuecomment-640812016),
//! although this is a pretty small optimization.
//!
//! Some day I expect [bytes::Bytes] will expose its vtable (see link above),
//! allowing us to minimize reference-counting without a custom chunk type.

use base::Error;
use reffers::ARefss;
use std::error::Error as StdError;

pub struct Chunk(ARefss<'static, [u8]>);

pub type BoxedError = Box<dyn StdError + Send + Sync>;

pub fn wrap_error(e: Error) -> BoxedError {
    Box::new(e)
}

impl From<ARefss<'static, [u8]>> for Chunk {
    fn from(r: ARefss<'static, [u8]>) -> Self {
        Chunk(r)
    }
}

impl From<&'static [u8]> for Chunk {
    fn from(r: &'static [u8]) -> Self {
        Chunk(ARefss::new(r))
    }
}

impl From<&'static str> for Chunk {
    fn from(r: &'static str) -> Self {
        Chunk(ARefss::new(r.as_bytes()))
    }
}

impl From<String> for Chunk {
    fn from(r: String) -> Self {
        Chunk(ARefss::new(r.into_bytes()))
    }
}

impl From<Vec<u8>> for Chunk {
    fn from(r: Vec<u8>) -> Self {
        Chunk(ARefss::new(r))
    }
}

impl hyper::body::Buf for Chunk {
    fn remaining(&self) -> usize {
        self.0.len()
    }
    fn chunk(&self) -> &[u8] {
        &self.0
    }
    fn advance(&mut self, cnt: usize) {
        self.0 = ::std::mem::replace(&mut self.0, ARefss::new(&[][..])).map(|b| &b[cnt..]);
    }
}

pub type Body = http_serve::Body<Chunk>;
