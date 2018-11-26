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

//! Tools for implementing a `http_serve::Entity` body composed from many "slices".

use failure::Error;
use futures::{Stream, stream};
use hyper::body::Payload;
use reffers::ARefs;
use std::error::Error as StdError;

pub struct Chunk(ARefs<'static, [u8]>);

//pub type CompatError = ::failure::Compat<Error>;
pub type BoxedError = Box<StdError + Send + Sync>;
pub type BodyStream = Box<Stream<Item = Chunk, Error = BoxedError> + Send + 'static>;

pub fn wrap_error(e: Error) -> BoxedError {
    Box::new(e.compat())
}

impl From<ARefs<'static, [u8]>> for Chunk {
    fn from(r: ARefs<'static, [u8]>) -> Self { Chunk(r) }
}

impl From<&'static [u8]> for Chunk {
    fn from(r: &'static [u8]) -> Self { Chunk(ARefs::new(r)) }
}

impl From<&'static str> for Chunk {
    fn from(r: &'static str) -> Self { Chunk(ARefs::new(r.as_bytes())) }
}

impl From<String> for Chunk {
    fn from(r: String) -> Self { Chunk(ARefs::new(r.into_bytes()).map(|v| &v[..])) }
}

impl From<Vec<u8>> for Chunk {
    fn from(r: Vec<u8>) -> Self { Chunk(ARefs::new(r).map(|v| &v[..])) }
}

impl ::bytes::Buf for Chunk {
    fn remaining(&self) -> usize { self.0.len() }
    fn bytes(&self) -> &[u8] { &*self.0 }
    fn advance(&mut self, cnt: usize) {
        self.0 = ::std::mem::replace(&mut self.0, ARefs::new(&[][..])).map(|b| &b[cnt..]);
    }
}

pub struct Body(BodyStream);

impl Payload for Body {
    type Data = Chunk;
    type Error = BoxedError;

    fn poll_data(&mut self) -> ::futures::Poll<Option<Self::Data>, Self::Error> {
        self.0.poll()
    }
}

impl From<BodyStream> for Body {
    fn from(b: BodyStream) -> Self { Body(b) }
}

impl<C: Into<Chunk>> From<C> for Body {
    fn from(c: C) -> Self {
        Body(Box::new(stream::once(Ok(c.into()))))
    }
}

impl From<Error> for Body {
    fn from(e: Error) -> Self {
        Body(Box::new(stream::once(Err(wrap_error(e)))))
    }
}

//impl<C: Into<Chunk>> From<C> for Body {
//    fn from(c: C) -> Self {
//        Body(Box::new(stream::once(Ok(c.into()))))
//    }
//}
