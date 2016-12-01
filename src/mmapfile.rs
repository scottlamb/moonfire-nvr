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

//! Memory-mapped file serving.

extern crate memmap;

use error::Result;
use std::fs::File;
use std::io;
use std::ops::Range;

/// Memory-mapped file slice.
/// This struct is meant to be used in constructing an implementation of the `resource::Resource`
/// or `pieces::ContextWriter` traits. The file in question should be immutable, as files shrinking
/// during `mmap` will cause the process to fail with `SIGBUS`. Moonfire NVR sample files satisfy
/// this requirement:
///
///    * They should only be modified by Moonfire NVR itself. Installation instructions encourage
///      creating a dedicated user/group for Moonfire NVR and ensuring only this group has
///      permissions to Moonfire NVR's directories.
///    * Moonfire NVR never modifies sample files after inserting their matching recording entries
///      into the database. They are kept as-is until they are deleted.
pub struct MmapFileSlice {
    f: File,
    range: Range<u64>,
}

impl MmapFileSlice {
    pub fn new(f: File, range: Range<u64>) -> MmapFileSlice {
        MmapFileSlice{f: f, range: range}
    }

    pub fn write_to(&self, range: Range<u64>, out: &mut io::Write) -> Result<()> {
        // TODO: overflow check (in case u64 is larger than usize).
        let r = self.range.start + range.start .. self.range.start + range.end;
        assert!(r.end <= self.range.end,
                "requested={:?} within={:?}", range, self.range);
        let mmap = memmap::Mmap::open_with_offset(
            &self.f, memmap::Protection::Read, r.start as usize, (r.end - r.start) as usize)?;
        unsafe { out.write_all(mmap.as_slice())?; }
        Ok(())
    }
}
