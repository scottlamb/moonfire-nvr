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

//! `.mp4` virtual file serving.
//!
//! The `mp4` module builds virtual files representing ISO/IEC 14496-12 (ISO base media format /
//! MPEG-4 / `.mp4`) video. These can be constructed from one or more recordings and are suitable
//! for HTTP range serving or download.

extern crate byteorder;
extern crate time;

use alloc::raw_vec::RawVec;
use byteorder::{BigEndian, ByteOrder, WriteBytesExt};
use db::{Database, ListCameraRecordingsRow, VideoSampleEntry};
use dir;
use error::{Error, Result};
use hyper::header;
use mmapfile;
use mime;
use openssl::crypto::hash;
use pieces;
use pieces::ContextWriter;
use pieces::Slices;
use recording::{self, TIME_UNITS_PER_SEC};
use resource;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::cmp;
use std::io;
use std::ops::Range;
use std::mem;
use std::sync::Arc;
use time::Timespec;

/// This value should be incremented any time a change is made to this file that causes different
/// bytes to be output for a particular set of `Mp4Builder` options. Incrementing this value will
/// cause the etag to change as well.
const FORMAT_VERSION: [u8; 1] = [0x02];

/// An `ftyp` (ISO/IEC 14496-12 section 4.3 `FileType`) box.
const FTYP_BOX: &'static [u8] = &[
    0x00,  0x00,  0x00,  0x20,  // length = 32, sizeof(FTYP_BOX)
    b'f',  b't',  b'y',  b'p',  // type
    b'i',  b's',  b'o',  b'm',  // major_brand
    0x00,  0x00,  0x02,  0x00,  // minor_version
    b'i',  b's',  b'o',  b'm',  // compatible_brands[0]
    b'i',  b's',  b'o',  b'2',  // compatible_brands[1]
    b'a',  b'v',  b'c',  b'1',  // compatible_brands[2]
    b'm',  b'p',  b'4',  b'1',  // compatible_brands[3]
];

/// An `hdlr` (ISO/IEC 14496-12 section 8.4.3 `HandlerBox`) box suitable for a video track.
const VIDEO_HDLR_BOX: &'static [u8] = &[
    0x00, 0x00, 0x00, 0x21,  // length == sizeof(kHdlrBox)
    b'h', b'd', b'l', b'r',  // type == hdlr, ISO/IEC 14496-12 section 8.4.3.
    0x00, 0x00, 0x00, 0x00,  // version + flags
    0x00, 0x00, 0x00, 0x00,  // pre_defined
    b'v', b'i', b'd', b'e',  // handler = vide
    0x00, 0x00, 0x00, 0x00,  // reserved[0]
    0x00, 0x00, 0x00, 0x00,  // reserved[1]
    0x00, 0x00, 0x00, 0x00,  // reserved[2]
    0x00,                    // name, zero-terminated (empty)
];

/// An `hdlr` (ISO/IEC 14496-12 section 8.4.3 `HandlerBox`) box suitable for a subtitle track.
const SUBTITLE_HDLR_BOX: &'static [u8] = &[
    0x00, 0x00, 0x00, 0x21,  // length == sizeof(kHdlrBox)
    b'h', b'd', b'l', b'r',  // type == hdlr, ISO/IEC 14496-12 section 8.4.3.
    0x00, 0x00, 0x00, 0x00,  // version + flags
    0x00, 0x00, 0x00, 0x00,  // pre_defined
    b's', b'b', b't', b'l',  // handler = sbtl
    0x00, 0x00, 0x00, 0x00,  // reserved[0]
    0x00, 0x00, 0x00, 0x00,  // reserved[1]
    0x00, 0x00, 0x00, 0x00,  // reserved[2]
    0x00,                    // name, zero-terminated (empty)
];

/// Part of an `mvhd` (`MovieHeaderBox` version 0, ISO/IEC 14496-12 section 8.2.2), used from
/// `append_mvhd`.
const MVHD_JUNK: &'static [u8] = &[
    0x00, 0x01, 0x00, 0x00,  // rate
    0x01, 0x00,              // volume
    0x00, 0x00,              // reserved
    0x00, 0x00, 0x00, 0x00,  // reserved
    0x00, 0x00, 0x00, 0x00,  // reserved
    0x00, 0x01, 0x00, 0x00,  // matrix[0]
    0x00, 0x00, 0x00, 0x00,  // matrix[1]
    0x00, 0x00, 0x00, 0x00,  // matrix[2]
    0x00, 0x00, 0x00, 0x00,  // matrix[3]
    0x00, 0x01, 0x00, 0x00,  // matrix[4]
    0x00, 0x00, 0x00, 0x00,  // matrix[5]
    0x00, 0x00, 0x00, 0x00,  // matrix[6]
    0x00, 0x00, 0x00, 0x00,  // matrix[7]
    0x40, 0x00, 0x00, 0x00,  // matrix[8]
    0x00, 0x00, 0x00, 0x00,  // pre_defined[0]
    0x00, 0x00, 0x00, 0x00,  // pre_defined[1]
    0x00, 0x00, 0x00, 0x00,  // pre_defined[2]
    0x00, 0x00, 0x00, 0x00,  // pre_defined[3]
    0x00, 0x00, 0x00, 0x00,  // pre_defined[4]
    0x00, 0x00, 0x00, 0x00,  // pre_defined[5]
];

/// Part of a `tkhd` (`TrackHeaderBox` version 0, ISO/IEC 14496-12 section 8.3.2), used from
/// `append_video_tkhd` and `append_subtitle_tkhd`.
const TKHD_JUNK: &'static [u8] = &[
    0x00, 0x00, 0x00, 0x00,  // reserved
    0x00, 0x00, 0x00, 0x00,  // reserved
    0x00, 0x00, 0x00, 0x00,  // layer + alternate_group
    0x00, 0x00, 0x00, 0x00,  // volume + reserved
    0x00, 0x01, 0x00, 0x00,  // matrix[0]
    0x00, 0x00, 0x00, 0x00,  // matrix[1]
    0x00, 0x00, 0x00, 0x00,  // matrix[2]
    0x00, 0x00, 0x00, 0x00,  // matrix[3]
    0x00, 0x01, 0x00, 0x00,  // matrix[4]
    0x00, 0x00, 0x00, 0x00,  // matrix[5]
    0x00, 0x00, 0x00, 0x00,  // matrix[6]
    0x00, 0x00, 0x00, 0x00,  // matrix[7]
    0x40, 0x00, 0x00, 0x00,  // matrix[8]
];

/// Part of a `minf` (`MediaInformationBox`, ISO/IEC 14496-12 section 8.4.4), used from
/// `append_video_minf`.
const VIDEO_MINF_JUNK: &'static [u8] = &[
    b'm', b'i', b'n', b'f',  // type = minf, ISO/IEC 14496-12 section 8.4.4.
    // A vmhd box; the "graphicsmode" and "opcolor" values don't have any
    // meaningful use.
    0x00, 0x00, 0x00, 0x14,  // length == sizeof(kVmhdBox)
    b'v', b'm', b'h', b'd',  // type = vmhd, ISO/IEC 14496-12 section 12.1.2.
    0x00, 0x00, 0x00, 0x01,  // version + flags(1)
    0x00, 0x00, 0x00, 0x00,  // graphicsmode (copy), opcolor[0]
    0x00, 0x00, 0x00, 0x00,  // opcolor[1], opcolor[2]

    // A dinf box suitable for a "self-contained" .mp4 file (no URL/URN
    // references to external data).
    0x00, 0x00, 0x00, 0x24,  // length == sizeof(kDinfBox)
    b'd', b'i', b'n', b'f',  // type = dinf, ISO/IEC 14496-12 section 8.7.1.
    0x00, 0x00, 0x00, 0x1c,  // length
    b'd', b'r', b'e', b'f',  // type = dref, ISO/IEC 14496-12 section 8.7.2.
    0x00, 0x00, 0x00, 0x00,  // version and flags
    0x00, 0x00, 0x00, 0x01,  // entry_count
    0x00, 0x00, 0x00, 0x0c,  // length
    b'u', b'r', b'l', b' ',  // type = url, ISO/IEC 14496-12 section 8.7.2.
    0x00, 0x00, 0x00, 0x01,  // version=0, flags=self-contained
];

/// Part of a `minf` (`MediaInformationBox`, ISO/IEC 14496-12 section 8.4.4), used from
/// `append_subtitle_minf`.
const SUBTITLE_MINF_JUNK: &'static [u8] = &[
    b'm', b'i', b'n', b'f',  // type = minf, ISO/IEC 14496-12 section 8.4.4.
    // A nmhd box.
    0x00, 0x00, 0x00, 0x0c,  // length == sizeof(kNmhdBox)
    b'n', b'm', b'h', b'd',  // type = nmhd, ISO/IEC 14496-12 section 12.1.2.
    0x00, 0x00, 0x00, 0x01,  // version + flags(1)

    // A dinf box suitable for a "self-contained" .mp4 file (no URL/URN
    // references to external data).
    0x00, 0x00, 0x00, 0x24,  // length == sizeof(kDinfBox)
    b'd', b'i', b'n', b'f',  // type = dinf, ISO/IEC 14496-12 section 8.7.1.
    0x00, 0x00, 0x00, 0x1c,  // length
    b'd', b'r', b'e', b'f',  // type = dref, ISO/IEC 14496-12 section 8.7.2.
    0x00, 0x00, 0x00, 0x00,  // version and flags
    0x00, 0x00, 0x00, 0x01,  // entry_count
    0x00, 0x00, 0x00, 0x0c,  // length
    b'u', b'r', b'l', b' ',  // type = url, ISO/IEC 14496-12 section 8.7.2.
    0x00, 0x00, 0x00, 0x01,  // version=0, flags=self-contained
];

/// Part of a `stbl` (`SampleTableBox`, ISO/IEC 14496 section 8.5.1) used from
/// `append_subtitle_stbl`.
const SUBTITLE_STBL_JUNK: &'static [u8] = &[
    b's', b't', b'b', b'l',  // type = stbl, ISO/IEC 14496-12 section 8.5.1.

    // A stsd box.
    0x00, 0x00, 0x00, 0x54,  // length
    b's', b't', b's', b'd',  // type == stsd, ISO/IEC 14496-12 section 8.5.2.
    0x00, 0x00, 0x00, 0x00,  // version + flags
    0x00, 0x00, 0x00, 0x01,  // entry_count == 1

    // SampleEntry, ISO/IEC 14496-12 section 8.5.2.2.
    0x00, 0x00, 0x00, 0x44,  // length
    b't', b'x', b'3', b'g',  // type == tx3g, 3GPP TS 26.245 section 5.16.
    0x00, 0x00, 0x00, 0x00,  // reserved
    0x00, 0x00, 0x00, 0x01,  // reserved, data_reference_index == 1

    // TextSampleEntry
    0x00, 0x00, 0x00, 0x00,  // displayFlags == none
    0x00,                    // horizontal-justification == left
    0x00,                    // vertical-justification == top
    0x00, 0x00, 0x00, 0x00,  // background-color-rgba == transparent

    // TextSampleEntry.BoxRecord
    0x00, 0x00,  // top
    0x00, 0x00,  // left
    0x00, 0x00,  // bottom
    0x00, 0x00,  // right

    // TextSampleEntry.StyleRecord
    0x00, 0x00,              // startChar
    0x00, 0x00,              // endChar
    0x00, 0x01,              // font-ID
    0x00,                    // face-style-flags
    0x12,                    // font-size == 18 px
    0xff, 0xff, 0xff, 0xff,  // text-color-rgba == opaque white

    // TextSampleEntry.FontTableBox
    0x00, 0x00, 0x00, 0x16,  // length
    b'f', b't', b'a', b'b',  // type == ftab, section 5.16
    0x00, 0x01,              // entry-count == 1
    0x00, 0x01,              // font-ID == 1
    0x09,                    // font-name-length == 9
    b'M', b'o', b'n', b'o', b's', b'p', b'a', b'c', b'e',
];

/// Pointers to each static bytestrings.
/// The order here must match the `StaticBytestring` enum.
const STATIC_BYTESTRINGS: [&'static [u8]; 8] = [
    FTYP_BOX,
    VIDEO_HDLR_BOX,
    SUBTITLE_HDLR_BOX,
    MVHD_JUNK,
    TKHD_JUNK,
    VIDEO_MINF_JUNK,
    SUBTITLE_MINF_JUNK,
    SUBTITLE_STBL_JUNK,
];

/// Enumeration of the static bytestrings. The order here must match the `STATIC_BYTESTRINGS`
/// array. The advantage of this enum over direct pointers to the relevant strings is that it
/// fits into a u32 on 64-bit platforms, allowing an `Mp4FileSlice` to fit into 8 bytes.
#[derive(Copy, Clone, Debug)]
enum StaticBytestring {
    FtypBox,
    VideoHdlrBox,
    SubtitleHdlrBox,
    MvhdJunk,
    TkhdJunk,
    VideoMinfJunk,
    SubtitleMinfJunk,
    SubtitleStblJunk,
}

/// The template fed into strtime for a timestamp subtitle. This must produce fixed-length output
/// (see `SUBTITLE_LENGTH`) to allow quick calculation of the total size of the subtitles for
/// a given time range.
const SUBTITLE_TEMPLATE: &'static str = "%Y-%m-%d %H:%M:%S %z";

/// The length of the output of `SUBTITLE_TEMPLATE`.
const SUBTITLE_LENGTH: usize = 25;  // "2015-07-02 17:10:00 -0700".len();

/// Holds the sample indexes for a given video segment: `stts`, `stsz`, and `stss`.
struct Mp4SegmentIndex {
    /// Holds all three sample indexes:
    /// &buf[.. stsz_start] is stts.
    /// &buf[stsz_start .. stss_start] is stsz.
    /// &buf[stss_start ..] is stss.
    buf: Box<[u8]>,
    stsz_start: usize,
    stss_start: usize,
}

impl Mp4SegmentIndex {
    fn stts(&self) -> &[u8] { &self.buf[.. self.stsz_start] }
    fn stsz(&self) -> &[u8] { &self.buf[self.stsz_start .. self.stss_start] }
    fn stss(&self) -> &[u8] { &self.buf[self.stss_start ..] }
}

struct Mp4Segment {
    s: recording::Segment,

    /// Holds the `stts`, `stsz`, and `stss` if they've been generated.
    /// Access only through `with_index`.
    index: RefCell<Option<Mp4SegmentIndex>>,

    /// The 1-indexed frame number in the `Mp4File` of the first frame in this segment.
    first_frame_num: u32,
    num_subtitle_samples: u32,
}

impl Mp4Segment {
    fn with_index<F, R>(&self, db: &Database, f: F) -> Result<R>
    where F: FnOnce(&Mp4SegmentIndex) -> Result<R> {
        let mut i = self.index.borrow_mut();
        if let Some(ref i) = *i {
            return f(i);
        }
        let index = self.build_index(db)?;
        let r = f(&index);
        *i = Some(index);
        r
    }

    fn build_index(&self, db: &Database) -> Result<Mp4SegmentIndex> {
        let s = &self.s;
        let stts_len = mem::size_of::<u32>() * 2 * (s.frames as usize);
        let stsz_len = mem::size_of::<u32>() * s.frames as usize;
        let stss_len = mem::size_of::<u32>() * s.key_frames as usize;
        let len = stts_len + stsz_len + stss_len;
        let mut buf = unsafe { RawVec::with_capacity(len).into_box() };
        {
            let (stts, mut rest) = buf.split_at_mut(stts_len);
            let (stsz, stss) = rest.split_at_mut(stsz_len);
            let mut frame = 0;
            let mut key_frame = 0;
            let mut last_start_and_dur = None;
            s.foreach(db, |it| {
                last_start_and_dur = Some((it.start_90k, it.duration_90k));
                BigEndian::write_u32(&mut stts[8*frame .. 8*frame+4], 1);
                BigEndian::write_u32(&mut stts[8*frame+4 .. 8*frame+8], it.duration_90k as u32);
                BigEndian::write_u32(&mut stsz[4*frame .. 4*frame+4], it.bytes as u32);
                if it.is_key {
                    BigEndian::write_u32(&mut stss[4*key_frame .. 4*key_frame+4],
                                         self.first_frame_num + (frame as u32));
                    key_frame += 1;
                }
                frame += 1;
                Ok(())
            })?;
            assert_eq!(s.frames, frame as i32);
            assert_eq!(s.key_frames, key_frame as i32);

            // Fix up the final frame's duration.
            // Doing this after the fact is more efficient than having a condition on every
            // iteration.
            if let Some((last_start, dur)) = last_start_and_dur {
                BigEndian::write_u32(&mut stts[8*frame-4 ..],
                                     cmp::min(s.desired_range_90k.end - last_start, dur) as u32);
            }
        }
        Ok(Mp4SegmentIndex{
            buf: buf,
            stsz_start: stts_len,
            stss_start: (stts_len + stsz_len),
        })
    }
}

pub struct Mp4FileBuilder {
    /// Segments of video: one per "recording" table entry as they should
    /// appear in the video.
    segments: Vec<Mp4Segment>,
    video_sample_entries: SmallVec<[Arc<VideoSampleEntry>; 1]>,
    next_frame_num: u32,
    duration_90k: u32,
    num_subtitle_samples: u32,
    subtitle_co64_pos: Option<usize>,
    body: BodyState,
    include_timestamp_subtitle_track: bool,
}

/// The portion of `Mp4FileBuilder` which is mutated while building the body of the file.
/// This is separated out from the rest so that it can be borrowed in a loop over
/// `Mp4FileBuilder::segments`; otherwise this would cause a double-self-borrow.
struct BodyState {
    slices: Slices<Mp4FileSlice, Mp4File>,

    /// `self.buf[unflushed_buf_pos .. self.buf.len()]` holds bytes that should be
    /// appended to `slices` before any other slice. See `flush_buf()`.
    unflushed_buf_pos: usize,
    buf: Vec<u8>,
}

#[derive(Debug)]
enum Mp4FileSlice {
    Static(StaticBytestring),  // index into STATIC_BYTESTRINGS
    Buf(u32),                  // index into m.buf
    VideoSampleEntry(u32),     // index into m.video_sample_entries
    Stts(u32),                 // index into m.segments
    Stsz(u32),                 // index into m.segments
    Co64,
    Stss(u32),                 // index into m.segments
    VideoSampleData(u32),      // index into m.segments
    SubtitleSampleData(u32),   // index into m.segments
}

impl ContextWriter<Mp4File> for Mp4FileSlice {
    fn write_to(&self, f: &Mp4File, r: Range<u64>, l: u64, out: &mut io::Write)
                -> Result<()> {
        match *self {
            Mp4FileSlice::Static(off) => {
                let s = STATIC_BYTESTRINGS[off as usize];
                debug!("write static data, range: {:?} slice len: {}", r, s.len());
                let part = &s[r.start as usize .. r.end as usize];
                out.write_all(part)?;
                Ok(())
            },
            Mp4FileSlice::Buf(off) => {
                let off = off as usize;
                debug!("write data from buf starting at offset {}/{}, range: {:?}",
                       off, f.buf.len(), r);
                out.write_all(
                    &f.buf[off+r.start as usize .. off+r.end as usize])?;
                Ok(())
            },
            Mp4FileSlice::VideoSampleEntry(off) => {
                let e = &f.video_sample_entries[off as usize];
                debug!("write video sample entry data, range: {:?} data len: {}", r, e.data.len());
                let part = &e.data[r.start as usize .. r.end as usize];
                out.write_all(part)?;
                Ok(())
            },
            Mp4FileSlice::Stts(index) => {
                debug!("write stts for segment {}/{}, range: {:?}", index, f.segments.len(), r);
                f.write_stts(index as usize, r, l, out)
            },
            Mp4FileSlice::Stsz(index) => {
                debug!("write stsz for segment {}/{}, range: {:?}", index, f.segments.len(), r);
                f.write_stsz(index as usize, r, l, out)
            },
            Mp4FileSlice::Co64 => {
                debug!("write co64, range: {:?}", r);
                f.write_co64(r, l, out)
            },
            Mp4FileSlice::Stss(index) => {
                debug!("write stss for segment {}/{}, range: {:?}", index, f.segments.len(), r);
                f.write_stss(index as usize, r, l, out)
            },
            Mp4FileSlice::VideoSampleData(index) => {
                debug!("write video data for segment {}/{}, range: {:?}",
                       index, f.segments.len(), r);
                f.write_video_sample_data(index as usize, r, out)
            },
            Mp4FileSlice::SubtitleSampleData(index) => {
                debug!("write subtitle data for segment {}/{}, range: {:?}",
                       index, f.segments.len(), r);
                f.write_subtitle_sample_data(index as usize, r, l, out)
            }
        }
    }
}

// Convert from 90kHz units since 1970-01-01 00:00:00 UTC to
// seconds since 1904-01-01 00:00:00 UTC.
fn to_iso14496_timestamp(t: recording::Time) -> u32 { (t.unix_seconds() + 24107 * 86400) as u32 }

// Used only within Mp4FileBuilder::build (and methods it calls internally).
// Writes a box length for everything appended in the supplied scope.
macro_rules! write_length {
    ($_self:ident, $b:block) => {{
        let len_pos = $_self.body.buf.len();
        let len_start = $_self.body.slices.len() + $_self.body.buf.len() as u64 -
                        $_self.body.unflushed_buf_pos as u64;
        $_self.body.append_u32(0);  // placeholder.
        { $b; }
        let len_end = $_self.body.slices.len() + $_self.body.buf.len() as u64 -
                      $_self.body.unflushed_buf_pos as u64;
        BigEndian::write_u32(&mut $_self.body.buf[len_pos .. len_pos + 4],
                             (len_end - len_start) as u32);
    }}
}

fn hex(raw: &[u8]) -> String {
    const HEX_CHARS: [u8; 16] = [b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7',
                                 b'8', b'9', b'a', b'b', b'c', b'd', b'e', b'f'];
    let mut hex = Vec::with_capacity(2 * raw.len());
    for b in raw {
        hex.push(HEX_CHARS[((b & 0xf0) >> 4) as usize]);
        hex.push(HEX_CHARS[( b & 0x0f      ) as usize]);
    }
    unsafe { String::from_utf8_unchecked(hex) }
}

impl Mp4FileBuilder {
    pub fn new() -> Self {
        Mp4FileBuilder{
            segments: Vec::new(),
            video_sample_entries: SmallVec::new(),
            next_frame_num: 1,
            duration_90k: 0,
            num_subtitle_samples: 0,
            subtitle_co64_pos: None,
            body: BodyState{
                slices: Slices::new(),
                buf: Vec::new(),
                unflushed_buf_pos: 0,
            },
            include_timestamp_subtitle_track: false,
        }
    }

    pub fn include_timestamp_subtitle_track(&mut self, b: bool) {
        self.include_timestamp_subtitle_track = b;
    }

    pub fn reserve(&mut self, additional: usize) {
        self.segments.reserve(additional);
    }

    pub fn len(&self) -> usize { self.segments.len() }

    pub fn append(&mut self, row: ListCameraRecordingsRow, rel_range_90k: Range<i32>) {
        self.segments.push(Mp4Segment{
            s: recording::Segment::new(&row, rel_range_90k),
            index: RefCell::new(None),
            first_frame_num: self.next_frame_num,
            num_subtitle_samples: 0,
        });
        self.next_frame_num += row.video_samples as u32;
        if !self.video_sample_entries.iter().any(|e| e.id == row.video_sample_entry.id) {
            self.video_sample_entries.push(row.video_sample_entry);
        }
    }

    pub fn build(mut self, db: Arc<Database>, dir: Arc<dir::SampleFileDir>) -> Result<Mp4File> {
        let mut max_end = None;
        let mut etag = hash::Hasher::new(hash::Type::SHA1)?;
        etag.update(&FORMAT_VERSION[..])?;
        if self.include_timestamp_subtitle_track {
            etag.update(b":ts:")?;
        }
        for s in &mut self.segments {
            s.s.init(&db)?;
            let d = &s.s.desired_range_90k;
            self.duration_90k += (d.end - d.start) as u32;
            let end = s.s.start + recording::Duration(d.end as i64);
            max_end = match max_end {
                None => Some(end),
                Some(v) => Some(cmp::max(v, end)),
            };

            if self.include_timestamp_subtitle_track {
                // Calculate the number of subtitle samples: starting to ending time (rounding up).
                let start_sec = (s.s.start + recording::Duration(d.start as i64)).unix_seconds();
                let end_sec = (s.s.start +
                               recording::Duration(d.end as i64 + TIME_UNITS_PER_SEC - 1))
                              .unix_seconds();
                s.num_subtitle_samples = (end_sec - start_sec) as u32;
                self.num_subtitle_samples += s.num_subtitle_samples;
            }

            // Update the etag to reflect this segment.
            let mut data = [0_u8; 24];
            let mut cursor = io::Cursor::new(&mut data[..]);
            cursor.write_i64::<BigEndian>(s.s.id)?;
            cursor.write_i64::<BigEndian>(s.s.start.0)?;
            cursor.write_i32::<BigEndian>(d.start)?;
            cursor.write_i32::<BigEndian>(d.end)?;
            etag.update(cursor.into_inner())?;
        }
        let max_end = match max_end {
            None => return Err(Error::new("no segments!".to_owned())),
            Some(v) => v,
        };
        let creation_ts = to_iso14496_timestamp(max_end);
        let mut est_slices = 16 + self.video_sample_entries.len() + 4 * self.segments.len();
        if self.include_timestamp_subtitle_track {
            est_slices += 16 + self.segments.len();
        }
        self.body.slices.reserve(est_slices);
        const EST_BUF_LEN: usize = 2048;
        self.body.buf.reserve(EST_BUF_LEN);
        self.body.append_static(StaticBytestring::FtypBox);
        self.append_moov(creation_ts)?;

        // Write the mdat header. Use the large format to support files over 2^32-1 bytes long.
        // Write zeroes for the length as a placeholder; fill it in after it's known.
        // It'd be nice to use the until-EOF form, but QuickTime Player doesn't support it.
        self.body.buf.extend_from_slice(b"\x00\x00\x00\x01mdat\x00\x00\x00\x00\x00\x00\x00\x00");
        let mdat_len_pos = self.body.buf.len() - 8;
        self.body.flush_buf();
        let initial_sample_byte_pos = self.body.slices.len();
        for (i, s) in self.segments.iter().enumerate() {
            let r = s.s.sample_file_range();
            self.body.slices.append(r.end - r.start, Mp4FileSlice::VideoSampleData(i as u32));
        }
        if let Some(p) = self.subtitle_co64_pos {
            BigEndian::write_u64(&mut self.body.buf[p .. p + 8], self.body.slices.len());
            for (i, s) in self.segments.iter().enumerate() {
                self.body.slices.append(
                    s.num_subtitle_samples as u64 *
                    (mem::size_of::<u16>() + SUBTITLE_LENGTH) as u64,
                    Mp4FileSlice::SubtitleSampleData(i as u32));
            }
        }
        // Fill in the length left as a placeholder above. Note the 16 here is the length
        // of the mdat header.
        BigEndian::write_u64(&mut self.body.buf[mdat_len_pos .. mdat_len_pos + 8],
                             16 + self.body.slices.len() - initial_sample_byte_pos);
        if est_slices < self.body.slices.num() {
            warn!("Estimated {} slices; actually were {} slices", est_slices,
                  self.body.slices.num());
        } else {
            debug!("Estimated {} slices; actually were {} slices", est_slices,
                   self.body.slices.num());
        }
        if EST_BUF_LEN < self.body.buf.len() {
            warn!("Estimated {} buf bytes; actually were {}", EST_BUF_LEN, self.body.buf.len());
        } else {
            debug!("Estimated {} buf bytes; actually were {}", EST_BUF_LEN, self.body.buf.len());
        }
        debug!("slices: {:?}", self.body.slices);
        Ok(Mp4File{
            db: db,
            dir: dir,
            segments: self.segments,
            slices: self.body.slices,
            buf: self.body.buf,
            video_sample_entries: self.video_sample_entries,
            initial_sample_byte_pos: initial_sample_byte_pos,
            last_modified: header::HttpDate(time::at(Timespec::new(max_end.unix_seconds(), 0))),
            etag: header::EntityTag::strong(hex(&etag.finish()?)),
        })
    }

    // MovieBox, ISO/IEC 14496-12 section 8.2.1.
    fn append_moov(&mut self, creation_ts: u32) -> Result<()> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"moov");
            self.append_mvhd(creation_ts);
            self.append_video_trak(creation_ts)?;
            if self.include_timestamp_subtitle_track {
                self.append_subtitle_trak(creation_ts);
            }
        });
        Ok(())
    }

    // MovieHeaderBox version 0, ISO/IEC 14496-12 section 8.2.2.
    fn append_mvhd(&mut self, creation_ts: u32) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"mvhd\x00\x00\x00\x00");
            self.body.append_u32(creation_ts);
            self.body.append_u32(creation_ts);
            self.body.append_u32(TIME_UNITS_PER_SEC as u32);
            let d = self.duration_90k;
            self.body.append_u32(d);
            self.body.append_static(StaticBytestring::MvhdJunk);
            // TODO: caption track?
            self.body.append_u32(2);           // next_track_id
        });
    }

    // ISO/IEC 14496-12 section 8.3.1, trak.
    fn append_video_trak(&mut self, creation_ts: u32) -> Result<()> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"trak");
            self.append_video_tkhd(creation_ts);
            self.maybe_append_video_edts()?;
            self.append_video_mdia(creation_ts);
        });
        Ok(())
    }

    // ISO/IEC 14496-12 section 8.3.1, trak.
    fn append_subtitle_trak(&mut self, creation_ts: u32) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"trak");
            self.append_subtitle_tkhd(creation_ts);
            self.append_subtitle_mdia(creation_ts);
        });
    }

    // ISO/IEC 14496-12 section 8.3.2.
    fn append_video_tkhd(&mut self, creation_ts: u32) {
        write_length!(self, {
            // flags 7: track_enabled | track_in_movie | track_in_preview
            self.body.buf.extend_from_slice(b"tkhd\x00\x00\x00\x07");
            self.body.append_u32(creation_ts);
            self.body.append_u32(creation_ts);
            self.body.append_u32(1);  // track_id
            self.body.append_u32(0);  // reserved
            self.body.append_u32(self.duration_90k);
            self.body.append_static(StaticBytestring::TkhdJunk);
            let width = self.video_sample_entries.iter().map(|e| e.width).max().unwrap();
            let height = self.video_sample_entries.iter().map(|e| e.height).max().unwrap();
            self.body.append_u32((width as u32) << 16);
            self.body.append_u32((height as u32) << 16);
        });
    }

    // ISO/IEC 14496-12 section 8.3.2.
    fn append_subtitle_tkhd(&mut self, creation_ts: u32) {
        write_length!(self, {
            // flags 7: track_enabled | track_in_movie | track_in_preview
            self.body.buf.extend_from_slice(b"tkhd\x00\x00\x00\x07");
            self.body.append_u32(creation_ts);
            self.body.append_u32(creation_ts);
            self.body.append_u32(2);  // track_id
            self.body.append_u32(0);  // reserved
            self.body.append_u32(self.duration_90k);
            self.body.append_static(StaticBytestring::TkhdJunk);
            self.body.append_u32(0);  // width, unused.
            self.body.append_u32(0);  // height, unused.
        });
    }

    // ISO/IEC 14496-12 section 8.6.5.
    fn maybe_append_video_edts(&mut self) -> Result<()> {
        #[derive(Debug, Default)]
        struct Entry {
            segment_duration: u64,
            media_time: u64,
        };
        let mut flushed: Vec<Entry> = Vec::new();
        let mut unflushed: Entry = Default::default();
        let mut cur_media_time: u64 = 0;
        for s in &self.segments {
            // The actual range may start before the desired range because it can only start on a
            // key frame. This relationship should hold true:
            // actual start <= desired start < desired end
            let actual = s.s.actual_time_90k();
            let skip = s.s.desired_range_90k.start - actual.start;
            let keep = s.s.desired_range_90k.end - s.s.desired_range_90k.start;
            assert!(skip >= 0 && keep > 0, "desired={}..{} actual={}..{}",
                    s.s.desired_range_90k.start, s.s.desired_range_90k.end,
                    actual.start, actual.end);
            cur_media_time += skip as u64;
            if unflushed.segment_duration + unflushed.media_time == cur_media_time {
                unflushed.segment_duration += keep as u64;
            } else {
                if unflushed.segment_duration > 0 {
                    flushed.push(unflushed);
                }
                unflushed = Entry{
                    segment_duration: keep as u64,
                    media_time: cur_media_time,
                };
            }
            cur_media_time += keep as u64;
        }

        if flushed.is_empty() && unflushed.media_time == 0 {
            return Ok(());  // use implicit one-to-one mapping.
        }

        flushed.push(unflushed);

        debug!("Using edit list: {:?}", flushed);
        write_length!(self, {
            self.body.buf.extend_from_slice(b"edts");
            write_length!(self, {
                // Use version 1 for 64-bit times.
                self.body.buf.extend_from_slice(b"elst\x01\x00\x00\x00");
                self.body.append_u32(flushed.len() as u32);
                for e in &flushed {
                    self.body.append_u64(e.segment_duration);
                    self.body.append_u64(e.media_time);

                    // media_rate_integer + media_rate_fraction: both fixed at 1
                    self.body.buf.extend_from_slice(b"\x00\x01\x00\x01");
                }
            });
        });
        Ok(())
    }

    // ISO/IEC 14496-12 section 8.4.1.
    fn append_video_mdia(&mut self, creation_ts: u32) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"mdia");
            self.append_mdhd(creation_ts);
            self.body.append_static(StaticBytestring::VideoHdlrBox);
            self.append_video_minf();
        });
    }

    // ISO/IEC 14496-12 section 8.4.1.
    fn append_subtitle_mdia(&mut self, creation_ts: u32) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"mdia");
            self.append_mdhd(creation_ts);
            self.body.append_static(StaticBytestring::SubtitleHdlrBox);
            self.append_subtitle_minf();
            // TODO: nmhddinf
        });
    }

    /// Appends a mdhd suitable for either the video or subtitle track.
    /// See ISO/IEC 14496-12 section 8.4.2.
    fn append_mdhd(&mut self, creation_ts: u32) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"mdhd\x00\x00\x00\x00");
            self.body.append_u32(creation_ts);
            self.body.append_u32(creation_ts);
            self.body.append_u32(TIME_UNITS_PER_SEC as u32);
            self.body.append_u32(self.duration_90k);
            self.body.append_u32(0x55c40000);  // language=und + pre_defined
        });
    }

    // ISO/IEC 14496-12 section 8.4.4.
    fn append_video_minf(&mut self) {
        write_length!(self, {
            self.body.append_static(StaticBytestring::VideoMinfJunk);
            self.append_video_stbl();
        });
    }

    // ISO/IEC 14496-12 section 8.4.4.
    fn append_subtitle_minf(&mut self) {
        write_length!(self, {
            self.body.append_static(StaticBytestring::SubtitleMinfJunk);
            self.append_subtitle_stbl();
        });
    }

    // ISO/IEC 14496-12 section 8.5.1.
    fn append_video_stbl(&mut self) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stbl");
            self.append_video_stsd();
            self.append_video_stts();
            self.append_video_stsc();
            self.append_video_stsz();
            self.append_video_co64();
            self.append_video_stss();
        });
    }

    // ISO/IEC 14496-12 section 8.5.1.
    fn append_subtitle_stbl(&mut self) {
        write_length!(self, {
            self.body.append_static(StaticBytestring::SubtitleStblJunk);
            self.append_subtitle_stts();
            self.append_subtitle_stsc();
            self.append_subtitle_stsz();
            self.append_subtitle_co64();
        });
    }

    // ISO/IEC 14496-12 section 8.5.2.
    fn append_video_stsd(&mut self) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stsd\x00\x00\x00\x00");
            let n_entries = self.video_sample_entries.len() as u32;
            self.body.append_u32(n_entries);
            self.body.flush_buf();
            for (i, e) in self.video_sample_entries.iter().enumerate() {
                self.body.slices.append(e.data.len() as u64,
                                        Mp4FileSlice::VideoSampleEntry(i as u32));
            }
        });
    }

    // ISO/IEC 14496-12 section 8.6.1.
    fn append_video_stts(&mut self) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stts\x00\x00\x00\x00");
            let mut entry_count = 0;
            for s in &self.segments {
                entry_count += s.s.frames as u32;
            }
            self.body.append_u32(entry_count);
            self.body.flush_buf();
            for (i, s) in self.segments.iter().enumerate() {
                self.body.slices.append(
                    2 * (mem::size_of::<u32>() as u64) * (s.s.frames as u64),
                    Mp4FileSlice::Stts(i as u32));
            }
        });
    }

    // ISO/IEC 14496-12 section 8.6.1.
    fn append_subtitle_stts(&mut self) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stts\x00\x00\x00\x00");

            let entry_count_pos = self.body.buf.len();
            self.body.append_u32(0);  // placeholder for entry_count

            let mut entry_count = 0;
            for s in &self.segments {
                let r = &s.s.desired_range_90k;
                let start = s.s.start + recording::Duration(r.start as i64);
                let end = s.s.start + recording::Duration(r.end as i64);
                let start_next_sec = recording::Time(
                    start.0 + TIME_UNITS_PER_SEC - (start.0 % TIME_UNITS_PER_SEC));
                if end <= start_next_sec {
                    // Segment doesn't last past the next second.
                    entry_count += 1;
                    self.body.append_u32(1);                       // count
                    self.body.append_u32((end - start).0 as u32);  // duration
                } else {
                    // The first subtitle just lasts until the next second.
                    entry_count += 1;
                    self.body.append_u32(1);                                  // count
                    self.body.append_u32((start_next_sec - start).0 as u32);  // duration

                    // Then there are zero or more "interior" subtitles, one second each.
                    let end_prev_sec = recording::Time(end.0 - (end.0 % TIME_UNITS_PER_SEC));
                    if start_next_sec < end_prev_sec {
                        entry_count += 1;
                        let interior = (end_prev_sec - start_next_sec).0 / TIME_UNITS_PER_SEC;
                        self.body.append_u32(interior as u32);                       // count
                        self.body.append_u32(TIME_UNITS_PER_SEC as u32);  // duration
                    }

                    // Then there's a final subtitle for the remaining fraction of a second.
                    entry_count += 1;
                    self.body.append_u32(1);                              // count
                    self.body.append_u32((end - end_prev_sec).0 as u32);  // duration
                }
            }
            BigEndian::write_u32(&mut self.body.buf[entry_count_pos .. entry_count_pos + 4],
                                 entry_count);
        });
    }

    // ISO/IEC 14496-12 section 8.7.4.
    fn append_video_stsc(&mut self) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stsc\x00\x00\x00\x00");
            self.body.append_u32(self.segments.len() as u32);
            for (i, s) in self.segments.iter().enumerate() {
                self.body.append_u32((i + 1) as u32);
                self.body.append_u32(s.s.frames as u32);

                // Write sample_description_index.
                let i = self.video_sample_entries.iter().position(
                    |e| e.id == s.s.video_sample_entry_id).unwrap();
                self.body.append_u32((i + 1) as u32);
            }
        });
    }

    // ISO/IEC 14496-12 section 8.7.4.
    fn append_subtitle_stsc(&mut self) {
        write_length!(self, {
            self.body.buf.extend_from_slice(
                b"stsc\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x01");
            self.body.append_u32(self.num_subtitle_samples);
            self.body.append_u32(1);
        });
    }

    // ISO/IEC 14496-12 section 8.7.3.
    fn append_video_stsz(&mut self) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stsz\x00\x00\x00\x00\x00\x00\x00\x00");
            let mut entry_count = 0;
            for s in &self.segments {
                entry_count += s.s.frames as u32;
            }
            self.body.append_u32(entry_count);
            self.body.flush_buf();
            for (i, s) in self.segments.iter().enumerate() {
                self.body.slices.append(
                    (mem::size_of::<u32>()) as u64 * (s.s.frames as u64),
                    Mp4FileSlice::Stsz(i as u32));
            }
        });
    }

    // ISO/IEC 14496-12 section 8.7.3.
    fn append_subtitle_stsz(&mut self) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stsz\x00\x00\x00\x00");
            self.body.append_u32((mem::size_of::<u16>() + SUBTITLE_LENGTH) as u32);
            self.body.append_u32(self.num_subtitle_samples);
        });
    }

    // ISO/IEC 14496-12 section 8.7.5.
    fn append_video_co64(&mut self) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"co64\x00\x00\x00\x00");
            self.body.append_u32(self.segments.len() as u32);
            self.body.flush_buf();
            self.body.slices.append(
                (mem::size_of::<u64>()) as u64 * (self.segments.len() as u64),
                Mp4FileSlice::Co64);
        });
    }

    // ISO/IEC 14496-12 section 8.7.5.
    fn append_subtitle_co64(&mut self) {
        write_length!(self, {
            // Write a placeholder; the actual value will be filled in later.
            self.body.buf.extend_from_slice(
                b"co64\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00");
            self.subtitle_co64_pos = Some(self.body.buf.len() - 8);
        });
    }

    // ISO/IEC 14496-12 section 8.6.2.
    fn append_video_stss(&mut self) {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stss\x00\x00\x00\x00");
            let mut entry_count = 0;
            for s in &self.segments {
                entry_count += s.s.key_frames as u32;
            }
            self.body.append_u32(entry_count);
            self.body.flush_buf();
            for (i, s) in self.segments.iter().enumerate() {
                self.body.slices.append(
                    (mem::size_of::<u32>() as u64) * (s.s.key_frames as u64),
                    Mp4FileSlice::Stss(i as u32));
            }
        });
    }
}

impl BodyState {
    fn append_u32(&mut self, v: u32) {
        self.buf.write_u32::<BigEndian>(v).expect("Vec write shouldn't fail");
    }

    fn append_u64(&mut self, v: u64) {
        self.buf.write_u64::<BigEndian>(v).expect("Vec write shouldn't fail");
    }

    fn flush_buf(&mut self) {
        let len = self.buf.len();
        if self.unflushed_buf_pos < len {
            self.slices.append((len - self.unflushed_buf_pos) as u64,
                               Mp4FileSlice::Buf(self.unflushed_buf_pos as u32));
            self.unflushed_buf_pos = len;
        }
    }

    fn append_static(&mut self, which: StaticBytestring) {
        self.flush_buf();
        let s = STATIC_BYTESTRINGS[which as usize];
        self.slices.append(s.len() as u64, Mp4FileSlice::Static(which));
    }
}

pub struct Mp4File {
    db: Arc<Database>,
    dir: Arc<dir::SampleFileDir>,
    segments: Vec<Mp4Segment>,
    slices: Slices<Mp4FileSlice, Mp4File>,
    buf: Vec<u8>,
    video_sample_entries: SmallVec<[Arc<VideoSampleEntry>; 1]>,
    initial_sample_byte_pos: u64,
    last_modified: header::HttpDate,
    etag: header::EntityTag,
}

impl Mp4File {
    fn write_stts(&self, i: usize, r: Range<u64>, _l: u64, out: &mut io::Write)
                  -> Result<()> {
        self.segments[i].with_index(&self.db, |i| {
            out.write_all(&i.stts()[r.start as usize .. r.end as usize])?;
            Ok(())
        })
    }

    fn write_stsz(&self, i: usize, r: Range<u64>, _l: u64, out: &mut io::Write)
                  -> Result<()> {
        self.segments[i].with_index(&self.db, |i| {
            out.write_all(&i.stsz()[r.start as usize .. r.end as usize])?;
            Ok(())
        })
    }

    fn write_co64(&self, r: Range<u64>, l: u64, out: &mut io::Write) -> Result<()> {
        pieces::clip_to_range(r, l, out, |w| {
            let mut pos = self.initial_sample_byte_pos;
            for s in &self.segments {
                w.write_u64::<BigEndian>(pos)?;
                let r = s.s.sample_file_range();
                pos += r.end - r.start;
            }
            Ok(())
        })
    }

    fn write_stss(&self, i: usize, r: Range<u64>, _l: u64, out: &mut io::Write) -> Result<()> {
        self.segments[i].with_index(&self.db, |i| {
            out.write_all(&i.stss()[r.start as usize .. r.end as usize])?;
            Ok(())
        })
    }

    fn write_video_sample_data(&self, i: usize, r: Range<u64>, out: &mut io::Write) -> Result<()> {
        let s = &self.segments[i];
        let f = self.dir.open_sample_file(self.db.lock().get_recording(s.s.id)?.sample_file_uuid)?;
        mmapfile::MmapFileSlice::new(f, s.s.sample_file_range()).write_to(r, out)
    }

    fn write_subtitle_sample_data(&self, i: usize, r: Range<u64>, l: u64, out: &mut io::Write)
                                  -> Result<()> {
        let s = &self.segments[i];
        let d = &s.s.desired_range_90k;
        let start_sec = (s.s.start + recording::Duration(d.start as i64)).unix_seconds();
        let end_sec = (s.s.start + recording::Duration(d.end as i64 + TIME_UNITS_PER_SEC - 1))
                      .unix_seconds();
        pieces::clip_to_range(r, l, out, |w| {
            for ts in start_sec .. end_sec {
                w.write_u16::<BigEndian>(SUBTITLE_LENGTH as u16)?;
                let tm = time::at(time::Timespec{sec: ts, nsec: 0});
                use std::io::Write;
                write!(w, "{}", tm.strftime(SUBTITLE_TEMPLATE)?)?;
            }
            Ok(())
        })?;
        Ok(())
    }
}

impl resource::Resource for Mp4File {
    fn content_type(&self) -> mime::Mime { "video/mp4".parse().unwrap() }
    fn last_modified(&self) -> &header::HttpDate { &self.last_modified }
    fn etag(&self) -> Option<&header::EntityTag> { Some(&self.etag) }
    fn len(&self) -> u64 { self.slices.len() }
    fn write_to(&self, range: Range<u64>, out: &mut io::Write) -> Result<()> {
        self.slices.write_to(self, range, out)
    }
}

#[cfg(test)]
mod tests {
    extern crate tempdir;
    extern crate test;

    use db;
    use dir;
    use ffmpeg;
    use hyper::{self, header};
    use openssl::crypto::hash;
    use recording::{self, TIME_UNITS_PER_SEC};
    use resource::{self, Resource};
    use rusqlite;
    use self::test::Bencher;
    use std::fs;
    use std::io;
    use std::mem;
    use std::path::Path;
    use std::sync::Arc;
    use std::thread;
    use super::*;
    use stream::StreamSource;
    use self::tempdir::TempDir;
    use testutil;
    use uuid::Uuid;

    struct Sha1(hash::Hasher);

    impl Sha1 {
        fn new() -> Sha1 { Sha1(hash::Hasher::new(hash::Type::SHA1).unwrap()) }
        fn finish(mut self) -> Vec<u8> { self.0.finish().unwrap() }
    }

    impl io::Write for Sha1 {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.update(buf).unwrap();
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }

    fn digest(r: &Resource) -> Vec<u8> {
        let mut sha1 = Sha1::new();
        r.write_to(0 .. r.len(), &mut sha1).unwrap();
        sha1.finish()
    }

    lazy_static! {
        static ref TEST_CAMERA_UUID: Uuid =
            Uuid::parse_str("ce2d9bc2-0cd3-4204-9324-7b5ccb07183c").unwrap();
    }

    const TEST_CAMERA_ID: i32 = 1;

    struct TestDb {
        db: Arc<db::Database>,
        dir: Arc<dir::SampleFileDir>,
        syncer_channel: dir::SyncerChannel,
        syncer_join: thread::JoinHandle<()>,
        tmpdir: TempDir,
    }

    // TODO: this seems like an integration test util; move to tests/common/*.rs file?
    fn setup_db() -> TestDb {
        let tmpdir = TempDir::new("mp4-test").unwrap();

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let schema = include_str!("schema.sql");
        conn.execute_batch(schema).unwrap();
        let uuid_bytes = &TEST_CAMERA_UUID.as_bytes()[..];
        conn.execute_named(r#"
            insert into camera (uuid,  short_name,  description,  host,  username,  password,
                                main_rtsp_path,  sub_rtsp_path,  retain_bytes)
                        values (:uuid, :short_name, :description, :host, :username, :password,
                                :main_rtsp_path, :sub_rtsp_path, :retain_bytes)
        "#, &[
            (":uuid", &uuid_bytes),
            (":short_name", &"test camera"),
            (":description", &""),
            (":host", &"test-camera"),
            (":username", &"foo"),
            (":password", &"bar"),
            (":main_rtsp_path", &"/main"),
            (":sub_rtsp_path", &"/sub"),
            (":retain_bytes", &1048576i64),
        ]).unwrap();
        assert_eq!(TEST_CAMERA_ID as i64, conn.last_insert_rowid());
        let db = Arc::new(db::Database::new(conn).unwrap());
        let path = tmpdir.path().to_str().unwrap().to_owned();
        let dir = dir::SampleFileDir::new(&path, db.clone()).unwrap();
        let (syncer_channel, syncer_join) = dir::start_syncer(dir.clone()).unwrap();
        TestDb{
            db: db,
            dir: dir,
            syncer_channel: syncer_channel,
            syncer_join: syncer_join,
            tmpdir: tmpdir,
        }
    }

    fn copy_mp4_to_db(db: &TestDb) {
        let mut input = StreamSource::File("src/testdata/clip.mp4").open().unwrap();

        // 2015-04-26 00:00:00 UTC.
        const START_TIME: recording::Time = recording::Time(1430006400i64 * TIME_UNITS_PER_SEC);
        let extra_data = input.get_extra_data().unwrap();
        let video_sample_entry_id = db.db.lock().insert_video_sample_entry(
            extra_data.width, extra_data.height, &extra_data.sample_entry).unwrap();
        let mut output = db.dir.create_writer(START_TIME, START_TIME, TEST_CAMERA_ID,
                                              video_sample_entry_id).unwrap();
        loop {
            let pkt = match input.get_next() {
                Ok(p) => p,
                Err(ffmpeg::Error::Eof) => { break; },
                Err(e) => { panic!("unexpected input error: {}", e); },
            };
            output.write(pkt.data().expect("packet without data"), pkt.duration() as i32,
                         pkt.is_key()).unwrap();
        }
        db.syncer_channel.async_save_writer(output).unwrap();
        db.syncer_channel.flush();
    }

    fn add_dummy_recordings_to_db(db: &db::Database) {
        let mut data = Vec::new();
        data.extend_from_slice(include_bytes!("testdata/video_sample_index.bin"));
        let mut db = db.lock();
        let video_sample_entry_id = db.insert_video_sample_entry(1920, 1080, &[0u8; 100]).unwrap();
        const START_TIME: recording::Time = recording::Time(1430006400i64 * TIME_UNITS_PER_SEC);
        const DURATION: recording::Duration = recording::Duration(5399985);
        let mut recording = db::RecordingToInsert{
            camera_id: TEST_CAMERA_ID,
            sample_file_bytes: 30104460,
            time: START_TIME .. (START_TIME + DURATION),
            local_time: START_TIME,
            video_samples: 1800,
            video_sync_samples: 60,
            video_sample_entry_id: video_sample_entry_id,
            sample_file_uuid: Uuid::nil(),
            video_index: data,
            sample_file_sha1: [0; 20],
        };
        let mut tx = db.tx().unwrap();
        tx.bypass_reservation_for_testing = true;
        for _ in 0..60 {
            tx.insert_recording(&recording).unwrap();
            recording.time.start += DURATION;
            recording.local_time += DURATION;
            recording.time.end += DURATION;
        }
        tx.commit().unwrap();
    }

    fn create_mp4_from_db(db: Arc<db::Database>, dir: Arc<dir::SampleFileDir>, skip_90k: i32,
                          shorten_90k: i32, include_subtitles: bool) -> Mp4File {
        let mut builder = Mp4FileBuilder::new();
        builder.include_timestamp_subtitle_track(include_subtitles);
        let all_time = recording::Time(i64::min_value()) .. recording::Time(i64::max_value());
        db.lock().list_recordings(TEST_CAMERA_ID, &all_time, |r| {
            let d = r.duration_90k;
            assert!(skip_90k + shorten_90k < d);
            builder.append(r, skip_90k .. d - shorten_90k);
            Ok(())
        }).unwrap();
        builder.build(db, dir).unwrap()
    }

    fn write_mp4(mp4: &Mp4File, dir: &Path) -> String {
        let mut filename = dir.to_path_buf();
        filename.push("clip.new.mp4");
        let mut out = fs::OpenOptions::new().write(true).create_new(true).open(&filename).unwrap();
        mp4.write_to(0 .. mp4.len(), &mut out).unwrap();
        filename.to_str().unwrap().to_string()
    }

    fn compare_mp4s(new_filename: &str, pts_offset: i64, shorten: i64) {
        let mut orig = StreamSource::File("src/testdata/clip.mp4").open().unwrap();
        let mut new = StreamSource::File(new_filename).open().unwrap();
        assert_eq!(orig.get_extra_data().unwrap(), new.get_extra_data().unwrap());
        let mut final_durations = None;
        loop {
            let orig_pkt = match orig.get_next() {
                Ok(p) => Some(p),
                Err(ffmpeg::Error::Eof) => None,
                Err(e) => { panic!("unexpected input error: {}", e); },
            };
            let new_pkt = match new.get_next() {
                Ok(p) => Some(p),
                Err(ffmpeg::Error::Eof) => { break; },
                Err(e) => { panic!("unexpected input error: {}", e); },
            };
            let (orig_pkt, new_pkt) = match (orig_pkt, new_pkt) {
                (Some(o), Some(n)) => (o, n),
                (None, None) => break,
                (o, n) => panic!("orig: {} new: {}", o.is_some(), n.is_some()),
            };
            assert_eq!(orig_pkt.pts().unwrap(), new_pkt.pts().unwrap() + pts_offset);
            assert_eq!(orig_pkt.dts(), new_pkt.dts() + pts_offset);
            assert_eq!(orig_pkt.data(), new_pkt.data());
            assert_eq!(orig_pkt.is_key(), new_pkt.is_key());
            final_durations = Some((orig_pkt.duration(), new_pkt.duration()));
        }

        if let Some((orig_dur, new_dur)) = final_durations {
            // One would normally expect the duration to be exactly the same, but when using an
            // edit list, ffmpeg appears to extend the last packet's duration by the amount skipped
            // at the beginning. I think this is a bug on their side.
            assert!(orig_dur - shorten + pts_offset == new_dur,
                    "orig_dur={} new_dur={} shorten={} pts_offset={}",
                    orig_dur, new_dur, shorten, pts_offset);
        }
    }

    #[test]
    fn test_round_trip() {
        testutil::init();
        let db = setup_db();
        copy_mp4_to_db(&db);
        let mp4 = create_mp4_from_db(db.db.clone(), db.dir.clone(), 0, 0, false);
        let new_filename = write_mp4(&mp4, db.tmpdir.path());
        compare_mp4s(&new_filename, 0, 0);

        // Test the metadata. This is brittle, which is the point. Any time the digest comparison
        // here fails, it can be updated, but the etag must change as well! Otherwise clients may
        // combine ranges from the new format with ranges from the old format.
        let sha1 = digest(&mp4);
        assert_eq!("1e5331e8371bd97ac3158b3a86494abc87cdc70e", super::hex(&sha1[..]));
        const EXPECTED_ETAG: &'static str = "191730a3c41adc9c5394b4516638ee6fda05649c";
        assert_eq!(Some(&header::EntityTag::strong(EXPECTED_ETAG.to_owned())), mp4.etag());
        drop(db.syncer_channel);
        db.syncer_join.join().unwrap();
    }

    #[test]
    fn test_round_trip_with_subtitles() {
        testutil::init();
        let db = setup_db();
        copy_mp4_to_db(&db);
        let mp4 = create_mp4_from_db(db.db.clone(), db.dir.clone(), 0, 0, true);
        let new_filename = write_mp4(&mp4, db.tmpdir.path());
        compare_mp4s(&new_filename, 0, 0);

        // Test the metadata. This is brittle, which is the point. Any time the digest comparison
        // here fails, it can be updated, but the etag must change as well! Otherwise clients may
        // combine ranges from the new format with ranges from the old format.
        let sha1 = digest(&mp4);
        assert_eq!("0081a442ba73092027fc580eeac2ebf25cb1ef50", super::hex(&sha1[..]));
        const EXPECTED_ETAG: &'static str = "d7aedccfae063219974086c43efdc6fda5a7e889";
        assert_eq!(Some(&header::EntityTag::strong(EXPECTED_ETAG.to_owned())), mp4.etag());
        drop(db.syncer_channel);
        db.syncer_join.join().unwrap();
    }

    #[test]
    fn test_round_trip_with_edit_list() {
        testutil::init();
        let db = setup_db();
        copy_mp4_to_db(&db);
        let mp4 = create_mp4_from_db(db.db.clone(), db.dir.clone(), 1, 0, false);
        let new_filename = write_mp4(&mp4, db.tmpdir.path());
        compare_mp4s(&new_filename, 1, 0);

        // Test the metadata. This is brittle, which is the point. Any time the digest comparison
        // here fails, it can be updated, but the etag must change as well! Otherwise clients may
        // combine ranges from the new format with ranges from the old format.
        let sha1 = digest(&mp4);
        assert_eq!("685e026af44204bc9cc52115c5e17058e9fb7c70", super::hex(&sha1[..]));
        const EXPECTED_ETAG: &'static str = "4cd904b6746330e63590a60d3bd254df3987caee";
        assert_eq!(Some(&header::EntityTag::strong(EXPECTED_ETAG.to_owned())), mp4.etag());
        drop(db.syncer_channel);
        db.syncer_join.join().unwrap();
    }

    #[test]
    fn test_round_trip_with_shorten() {
        testutil::init();
        let db = setup_db();
        copy_mp4_to_db(&db);
        let mp4 = create_mp4_from_db(db.db.clone(), db.dir.clone(), 0, 1, false);
        let new_filename = write_mp4(&mp4, db.tmpdir.path());
        compare_mp4s(&new_filename, 0, 1);

        // Test the metadata. This is brittle, which is the point. Any time the digest comparison
        // here fails, it can be updated, but the etag must change as well! Otherwise clients may
        // combine ranges from the new format with ranges from the old format.
        let sha1 = digest(&mp4);
        assert_eq!("e0d28ddf08e24575a82657b1ce0b2da73f32fd88", super::hex(&sha1[..]));
        const EXPECTED_ETAG: &'static str = "d2e75438be6c3a747bf0d9aa86332604340d2b82";
        assert_eq!(Some(&header::EntityTag::strong(EXPECTED_ETAG.to_owned())), mp4.etag());
        drop(db.syncer_channel);
        db.syncer_join.join().unwrap();
    }

    #[test]
    fn mp4_file_slice_size() {
        assert_eq!(8, mem::size_of::<super::Mp4FileSlice>());
    }

    /// An HTTP server for benchmarking.
    /// It's used as a singleton via `lazy_static!` for two reasons:
    ///
    ///    * to avoid running out of file descriptors. `#[bench]` functions apparently get called
    ///      many times as the number of iterations is tuned, and hyper servers
    ///      [can't be shut down](https://github.com/hyperium/hyper/issues/338), so
    ///      otherwise the default Ubuntu 16.04.1 ulimit of 1024 files is quickly exhausted.
    ///    * so that when getting a CPU profile of the benchmark, more of the profile focuses
    ///      on the HTTP serving rather than the setup.
    ///
    /// Currently this only serves a single `.mp4` file but we could set up variations to benchmark
    /// different scenarios: with/without subtitles and edit lists, different lengths, serving
    /// different fractions of the file, etc.
    struct BenchServer {
        url: hyper::Url,
        generated_len: u64,
    }

    impl BenchServer {
        fn new() -> BenchServer {
            let mut listener = hyper::net::HttpListener::new("127.0.0.1:0").unwrap();
            use hyper::net::NetworkListener;
            let addr = listener.local_addr().unwrap();
            let server = hyper::Server::new(listener);
            let url = hyper::Url::parse(
                format!("http://{}:{}/", addr.ip(), addr.port()).as_str()).unwrap();
            let db = setup_db();
            add_dummy_recordings_to_db(&db.db);
            let mp4 = create_mp4_from_db(db.db.clone(), db.dir.clone(), 0, 0, false);
            let p = mp4.initial_sample_byte_pos;
            use std::thread::spawn;
            spawn(move || {
                use hyper::server::{Request, Response, Fresh};
                let (db, dir) = (db.db.clone(), db.dir.clone());
                let _ = server.handle(move |req: Request, res: Response<Fresh>| {
                    let mp4 = create_mp4_from_db(db.clone(), dir.clone(), 0, 0, false);
                    resource::serve(&mp4, &req, res).unwrap();
                });
            });
            BenchServer{
                url: url,
                generated_len: p,
            }
        }
    }

    lazy_static! {
        static ref SERVER: BenchServer = { BenchServer::new() };
    }

    /// Benchmarks serving the generated part of a `.mp4` file (up to the first byte from disk).
    #[bench]
    fn serve_generated_bytes_fresh_client(b: &mut Bencher) {
        testutil::init();
        let server = &*SERVER;
        let p = server.generated_len;
        let mut buf = Vec::with_capacity(p as usize);
        b.bytes = p;
        b.iter(|| {
            let client = hyper::Client::new();
            let mut resp =
                client.get(server.url.clone())
                      .header(header::Range::Bytes(vec![header::ByteRangeSpec::FromTo(0, p - 1)]))
                      .send()
                      .unwrap();
            buf.clear();
            use std::io::Read;
            let size = resp.read_to_end(&mut buf).unwrap();
            assert_eq!(p, size as u64);
        });
    }

    /// Another benchmark of serving generated bytes, but reusing the `hyper::Client`.
    /// This should be faster than the `fresh` version, but see
    /// [this hyper issue](https://github.com/hyperium/hyper/issues/944) relating to Nagle's
    /// algorithm.
    #[bench]
    fn serve_generated_bytes_reuse_client(b: &mut Bencher) {
        testutil::init();
        let server = &*SERVER;
        let p = server.generated_len;
        let mut buf = Vec::with_capacity(p as usize);
        b.bytes = p;
        let client = hyper::Client::new();
        b.iter(|| {
            let mut resp =
                client.get(server.url.clone())
                      .header(header::Range::Bytes(vec![header::ByteRangeSpec::FromTo(0, p - 1)]))
                      .send()
                      .unwrap();
            buf.clear();
            use std::io::Read;
            let size = resp.read_to_end(&mut buf).unwrap();
            assert_eq!(p, size as u64);
        });
    }

    #[bench]
    fn mp4_construction(b: &mut Bencher) {
        testutil::init();
        let db = setup_db();
        add_dummy_recordings_to_db(&db.db);
        b.iter(|| {
            create_mp4_from_db(db.db.clone(), db.dir.clone(), 0, 0, false);
        });
    }
}
