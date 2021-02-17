// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors
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

//! Tools for implementing a `http_serve::Entity` body composed from many "slices".

use base::Error;
use futures::{stream, Stream};
use reffers::ARefss;
use std::error::Error as StdError;
use std::pin::Pin;
use sync_wrapper::SyncWrapper;

pub struct Chunk(ARefss<'static, [u8]>);

pub type BoxedError = Box<dyn StdError + Send + Sync>;
pub type BodyStream = Box<dyn Stream<Item = Result<Chunk, BoxedError>> + Send>;

pub fn wrap_error(e: Error) -> BoxedError {
    Box::new(e.compat())
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
        Chunk(ARefss::new(r.into_bytes()).map(|v| &v[..]))
    }
}

impl From<Vec<u8>> for Chunk {
    fn from(r: Vec<u8>) -> Self {
        Chunk(ARefss::new(r).map(|v| &v[..]))
    }
}

impl hyper::body::Buf for Chunk {
    fn remaining(&self) -> usize {
        self.0.len()
    }
    fn chunk(&self) -> &[u8] {
        &*self.0
    }
    fn advance(&mut self, cnt: usize) {
        self.0 = ::std::mem::replace(&mut self.0, ARefss::new(&[][..])).map(|b| &b[cnt..]);
    }
}

// This SyncWrapper stuff is blindly copied from hyper's body type.
// See <https://github.com/hyperium/hyper/pull/2187>, matched by
// <https://github.com/scottlamb/http-serve/pull/18>.
pub struct Body(SyncWrapper<Pin<BodyStream>>);

impl hyper::body::HttpBody for Body {
    type Data = Chunk;
    type Error = BoxedError;

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> std::task::Poll<Option<Result<Self::Data, Self::Error>>> {
        // This is safe because the pin is not structural.
        // https://doc.rust-lang.org/std/pin/#pinning-is-not-structural-for-field
        // (The field _holds_ a pin, but isn't itself pinned.)
        unsafe { self.get_unchecked_mut() }
            .0
            .get_mut()
            .as_mut()
            .poll_next(cx)
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context,
    ) -> std::task::Poll<Result<Option<http::header::HeaderMap>, Self::Error>> {
        std::task::Poll::Ready(Ok(None))
    }
}

impl From<BodyStream> for Body {
    fn from(b: BodyStream) -> Self {
        Body(SyncWrapper::new(Pin::from(b)))
    }
}

impl<C: Into<Chunk>> From<C> for Body {
    fn from(c: C) -> Self {
        Body(SyncWrapper::new(Box::pin(stream::once(
            futures::future::ok(c.into()),
        ))))
    }
}

impl From<Error> for Body {
    fn from(e: Error) -> Self {
        Body(SyncWrapper::new(Box::pin(stream::once(
            futures::future::err(wrap_error(e)),
        ))))
    }
}
