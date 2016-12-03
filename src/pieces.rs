// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2016 Scott Lamb <slamb@slamb.org>
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

use error::{Error, Result};
use std::fmt;
use std::io;
use std::marker::PhantomData;
use std::ops::Range;

#[derive(Debug)]
struct SliceInfo<W> {
    end: u64,
    writer: W,
}

pub trait ContextWriter<Ctx> {
    fn write_to(&self, ctx: &Ctx, r: Range<u64>, l: u64, out: &mut io::Write) -> Result<()>;
}

/// Calls `f` with an `io::Write` which delegates to `inner` only for the section defined by `r`.
/// This is useful for easily implementing the `ContextWriter` interface for pieces that generate
/// data on-the-fly rather than simply copying a buffer.
pub fn clip_to_range<F>(r: Range<u64>, l: u64, inner: &mut io::Write, mut f: F) -> Result<()>
where F: FnMut(&mut Vec<u8>) -> Result<()> {
    // Just create a buffer for the whole slice and copy out the relevant portion.
    // One might expect it to be faster to avoid this memory allocation and extra copying, but
    // benchmarks show when making many 4-byte writes it's better to be able to inline many
    // Vec::write_all calls then make one call through traits to hyper's write logic.
    let mut buf = Vec::with_capacity(l as usize);
    f(&mut buf)?;
    inner.write_all(&buf[r.start as usize .. r.end as usize])?;
    Ok(())
}

pub struct Slices<W, C> {
    len: u64,
    slices: Vec<SliceInfo<W>>,
    phantom: PhantomData<C>,
}

impl<W, C> fmt::Debug for Slices<W, C> where W: fmt::Debug {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} slices with overall length {}:", self.slices.len(), self.len)?;
        let mut start = 0;
        for (i, s) in self.slices.iter().enumerate() {
            write!(f, "\ni {:7}: range [{:12}, {:12}) len {:12}: {:?}",
                   i, start, s.end, s.end - start, s.writer)?;
            start = s.end;
        }
        Ok(())
    }
}

impl<W, C> Slices<W, C> where W: ContextWriter<C> {
    pub fn new() -> Slices<W, C> {
        Slices{len: 0, slices: Vec::new(), phantom: PhantomData}
    }

    pub fn reserve(&mut self, additional: usize) {
        self.slices.reserve(additional)
    }

    pub fn append(&mut self, len: u64, writer: W) {
        self.len += len;
        self.slices.push(SliceInfo{end: self.len, writer: writer});
    }

    /// Returns the total byte length of all slices.
    pub fn len(&self) -> u64 { self.len }

    /// Returns the number of slices.
    pub fn num(&self) -> usize { self.slices.len() }

    pub fn write_to(&self, ctx: &C, range: Range<u64>, out: &mut io::Write)
                    -> Result<()> {
        if range.start > range.end || range.end > self.len {
            return Err(Error{
                description: format!("Bad range {:?} for slice of length {}", range, self.len),
                cause: None});
        }

        // Binary search for the first slice of the range to write, determining its index and
        // (from the preceding slice) the start of its range.
        let (mut i, mut slice_start) = match self.slices.binary_search_by_key(&range.start,
                                                                              |s| s.end) {
            Ok(i) if i == self.slices.len() - 1 => return Ok(()),  // at end.
            Ok(i) => (i+1, self.slices[i].end),   // desired start == slice i's end; first is i+1!
            Err(i) if i == 0 => (i, 0),           // desired start < slice 0's end; first is 0.
            Err(i) => (i, self.slices[i-1].end),  // desired start < slice i's end; first is i.
        };

        // Iterate through and write each slice until the end.
        let mut start_pos = range.start - slice_start;
        loop {
            let s = &self.slices[i];
            let l = s.end - slice_start;
            if range.end <= s.end {  // last slice.
                return s.writer.write_to(ctx, start_pos .. range.end - slice_start, l, out);
            }
            s.writer.write_to(ctx, start_pos .. s.end - slice_start, l, out)?;

            // setup next iteration.
            start_pos = 0;
            slice_start = s.end;
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use error::{Error, Result};
    use std::cell::RefCell;
    use std::error::Error as E;
    use std::io::Write;
    use std::ops::Range;
    use std::vec::Vec;
    use super::{ContextWriter, Slices, clip_to_range};

    #[derive(Debug, Eq, PartialEq)]
    pub struct FakeWrite {
        writer: &'static str,
        range: Range<u64>,
    }

    pub struct FakeWriter {
        name: &'static str,
    }

    impl ContextWriter<RefCell<Vec<FakeWrite>>> for FakeWriter {
        fn write_to(&self, ctx: &RefCell<Vec<FakeWrite>>, r: Range<u64>, _l: u64, _out: &mut Write)
                    -> Result<()> {
            ctx.borrow_mut().push(FakeWrite{writer: self.name, range: r});
            Ok(())
        }
    }

    pub fn new_slices() -> Slices<FakeWriter, RefCell<Vec<FakeWrite>>> {
        let mut s = Slices::new();
        s.append(5, FakeWriter{name: "a"});
        s.append(13, FakeWriter{name: "b"});
        s.append(7, FakeWriter{name: "c"});
        s.append(17, FakeWriter{name: "d"});
        s.append(19, FakeWriter{name: "e"});
        s
    }

    #[test]
    pub fn size() {
        assert_eq!(5 + 13 + 7 + 17 + 19, new_slices().len());
    }

    #[test]
    pub fn exact_slice() {
        // Test writing exactly slice b.
        let s = new_slices();
        let w = RefCell::new(Vec::new());
        let mut dummy = Vec::new();
        s.write_to(&w, 5 .. 18, &mut dummy).unwrap();
        assert_eq!(&[FakeWrite{writer: "b", range: 0 .. 13}], &w.borrow()[..]);
    }

    #[test]
    pub fn offset_first() {
        // Test writing part of slice a.
        let s = new_slices();
        let w = RefCell::new(Vec::new());
        let mut dummy = Vec::new();
        s.write_to(&w, 1 .. 3, &mut dummy).unwrap();
        assert_eq!(&[FakeWrite{writer: "a", range: 1 .. 3}], &w.borrow()[..]);
    }

    #[test]
    pub fn offset_mid() {
        // Test writing part of slice b, all of slice c, and part of slice d.
        let s = new_slices();
        let w = RefCell::new(Vec::new());
        let mut dummy = Vec::new();
        s.write_to(&w, 17 .. 26, &mut dummy).unwrap();
        assert_eq!(&[
                   FakeWrite{writer: "b", range: 12 .. 13},
                   FakeWrite{writer: "c", range: 0 .. 7},
                   FakeWrite{writer: "d", range: 0 .. 1},
                   ], &w.borrow()[..]);
    }

    #[test]
    pub fn everything() {
        // Test writing the whole Slices.
        let s = new_slices();
        let w = RefCell::new(Vec::new());
        let mut dummy = Vec::new();
        s.write_to(&w, 0 .. 61, &mut dummy).unwrap();
        assert_eq!(&[
                   FakeWrite{writer: "a", range: 0 .. 5},
                   FakeWrite{writer: "b", range: 0 .. 13},
                   FakeWrite{writer: "c", range: 0 .. 7},
                   FakeWrite{writer: "d", range: 0 .. 17},
                   FakeWrite{writer: "e", range: 0 .. 19},
                   ], &w.borrow()[..]);
    }

    #[test]
    pub fn at_end() {
        let s = new_slices();
        let w = RefCell::new(Vec::new());
        let mut dummy = Vec::new();
        s.write_to(&w, 61 .. 61, &mut dummy).unwrap();
        let empty: &[FakeWrite] = &[];
        assert_eq!(empty, &w.borrow()[..]);
    }

    #[test]
    pub fn test_clip_to_range() {
        let mut out = Vec::new();

        // Simple case: one write with everything.
        clip_to_range(0 .. 5, 5, &mut out, |w| {
            w.write_all(b"01234").unwrap();
            Ok(())
        }).unwrap();
        assert_eq!(b"01234", &out[..]);

        // Same in a few writes.
        out.clear();
        clip_to_range(0 .. 5, 5, &mut out, |w| {
            w.write_all(b"0").unwrap();
            w.write_all(b"123").unwrap();
            w.write_all(b"4").unwrap();
            Ok(())
        }).unwrap();
        assert_eq!(b"01234", &out[..]);

        // Limiting to a prefix.
        out.clear();
        clip_to_range(0 .. 2, 5, &mut out, |w| {
            w.write_all(b"0").unwrap();    // all of this write
            w.write_all(b"123").unwrap();  // some of this write
            w.write_all(b"4").unwrap();    // none of this write
            Ok(())
        }).unwrap();
        assert_eq!(b"01", &out[..]);

        // Limiting to part in the middle.
        out.clear();
        clip_to_range(2 .. 4, 5, &mut out, |w| {
            w.write_all(b"0").unwrap();     // none of this write
            w.write_all(b"1234").unwrap();  // middle of this write
            w.write_all(b"5678").unwrap();  // none of this write
            Ok(())
        }).unwrap();
        assert_eq!(b"23", &out[..]);

        // If the callback returns an error, it should be propagated (fast path or not).
        out.clear();
        assert_eq!(
            clip_to_range(0 .. 4, 4, &mut out, |_| Err(Error::new("some error".to_owned())))
                .unwrap_err().description(),
            "some error");
        out.clear();
        assert_eq!(
            clip_to_range(0 .. 1, 4, &mut out, |_| Err(Error::new("some error".to_owned())))
                .unwrap_err().description(),
            "some error");

        // TODO: if inner.write does a partial write, the next try should start at the correct
        // position.
    }
}
