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
//! for HTTP range serving or download. The generated `.mp4` file has the `moov` box before the
//! `mdat` box for fast start. More specifically, boxes are arranged in the order suggested by
//! ISO/IEC 14496-12 section 6.2.3 (Table 1):
//!
//! * ftyp (file type and compatibility)
//! * moov (container for all the metadata)
//! ** mvhd (movie header, overall declarations)
//!
//! ** trak (video: container for an individual track or stream)
//! *** tkhd (track header, overall information about the track)
//! *** (optional) edts (edit list container)
//! **** elst (an edit list)
//! *** mdia (container for the media information in a track)
//! **** mdhd (media header, overall information about the media)
//! *** minf (media information container)
//! **** vmhd (video media header, overall information (video track only))
//! **** dinf (data information box, container)
//! ***** dref (data reference box, declares source(s) of media data in track)
//! **** stbl (sample table box, container for the time/space map)
//! ***** stsd (sample descriptions (codec types, initilization etc.)
//! ***** stts ((decoding) time-to-sample)
//! ***** stsc (sample-to-chunk, partial data-offset information)
//! ***** stsz (samples sizes (framing))
//! ***** co64 (64-bit chunk offset)
//! ***** stss (sync sample table)
//!
//! ** (optional) trak (subtitle: container for an individual track or stream)
//! *** tkhd (track header, overall information about the track)
//! *** mdia (container for the media information in a track)
//! **** mdhd (media header, overall information about the media)
//! *** minf (media information container)
//! **** nmhd (null media header, overall information)
//! **** dinf (data information box, container)
//! ***** dref (data reference box, declares source(s) of media data in track)
//! **** stbl (sample table box, container for the time/space map)
//! ***** stsd (sample descriptions (codec types, initilization etc.)
//! ***** stts ((decoding) time-to-sample)
//! ***** stsc (sample-to-chunk, partial data-offset information)
//! ***** stsz (samples sizes (framing))
//! ***** co64 (64-bit chunk offset)
//!
//! * mdat (media data container)
//! ```

extern crate time;

use byteorder::{BigEndian, ByteOrder, WriteBytesExt};
use db::recording::{self, TIME_UNITS_PER_SEC};
use db::{self, dir};
use failure::Error;
use futures::stream;
use http_serve;
use hyper::header;
use memmap;
use openssl::hash;
use parking_lot::{Once, ONCE_INIT};
use reffers::ARefs;
use slices::{self, Body, Chunk, Slices};
use smallvec::SmallVec;
use std::cell::UnsafeCell;
use std::cmp;
use std::fmt;
use std::io;
use std::ops::Range;
use std::mem;
use std::sync::Arc;
use strutil;

/// This value should be incremented any time a change is made to this file that causes different
/// bytes to be output for a particular set of `Mp4Builder` options. Incrementing this value will
/// cause the etag to change as well.
const FORMAT_VERSION: [u8; 1] = [0x05];

/// An `ftyp` (ISO/IEC 14496-12 section 4.3 `FileType`) box.
const NORMAL_FTYP_BOX: &'static [u8] = &[
    0x00,  0x00,  0x00,  0x20,  // length = 32, sizeof(NORMAL_FTYP_BOX)
    b'f',  b't',  b'y',  b'p',  // type
    b'i',  b's',  b'o',  b'm',  // major_brand
    0x00,  0x00,  0x02,  0x00,  // minor_version
    b'i',  b's',  b'o',  b'm',  // compatible_brands[0]
    b'i',  b's',  b'o',  b'2',  // compatible_brands[1]
    b'a',  b'v',  b'c',  b'1',  // compatible_brands[2]
    b'm',  b'p',  b'4',  b'1',  // compatible_brands[3]
];

/// An `ftyp` (ISO/IEC 14496-12 section 4.3 `FileType`) box for an initialization segment.
/// More restrictive brands because of the default-base-is-moof flag.
const INIT_SEGMENT_FTYP_BOX: &'static [u8] = &[
    0x00,  0x00,  0x00,  0x10,  // length = 16, sizeof(INIT_SEGMENT_FTYP_BOX)
    b'f',  b't',  b'y',  b'p',  // type
    b'i',  b's',  b'o',  b'5',  // major_brand
    0x00,  0x00,  0x02,  0x00,  // minor_version
];

/// An `hdlr` (ISO/IEC 14496-12 section 8.4.3 `HandlerBox`) box suitable for video.
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

/// An `hdlr` (ISO/IEC 14496-12 section 8.4.3 `HandlerBox`) box suitable for subtitles.
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
const STATIC_BYTESTRINGS: [&'static [u8]; 9] = [
    NORMAL_FTYP_BOX,
    INIT_SEGMENT_FTYP_BOX,
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
/// fits into `Slice`'s 20-bit `p`.
#[derive(Copy, Clone, Debug)]
enum StaticBytestring {
    NormalFtypBox,
    InitSegmentFtypBox,
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

/// The lengths of the indexes associated with a `Segment`; for use within `Segment` only.
struct SegmentLengths {
    stts: usize,
    stsz: usize,
    stss: usize,
}

/// A wrapper around `recording::Segment` that keeps some additional `.mp4`-specific state.
struct Segment {
    s: recording::Segment,

    /// If generated, the `.mp4`-format sample indexes, accessed only through `get_index`:
    ///    1. stts: `slice[.. stsz_start]`
    ///    2. stsz: `slice[stsz_start .. stss_start]`
    ///    3. stss: `slice[stss_start ..]`
    index: UnsafeCell<Result<Box<[u8]>, ()>>,

    /// The 1-indexed frame number in the `File` of the first frame in this segment.
    first_frame_num: u32,
    num_subtitle_samples: u16,

    index_once: Once,
}

// Manually implement Debug because `index` and `index_once` are not Debug.
impl fmt::Debug for Segment {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("mp4::Segment")
           .field("s", &self.s)
           .field("first_frame_num", &self.first_frame_num)
           .field("num_subtitle_samples", &self.num_subtitle_samples)
           .finish()
    }
}

unsafe impl Sync for Segment {}

impl Segment {
    fn new(db: &db::LockedDatabase, row: &db::ListRecordingsRow, rel_range_90k: Range<i32>,
           first_frame_num: u32) -> Result<Self, Error> {
        Ok(Segment{
            s: recording::Segment::new(db, row, rel_range_90k)?,
            index: UnsafeCell::new(Err(())),
            index_once: ONCE_INIT,
            first_frame_num,
            num_subtitle_samples: 0,
        })
    }

    fn get_index<'a, F>(&'a self, db: &db::Database, f: F) -> Result<&'a [u8], Error>
    where F: FnOnce(&[u8], SegmentLengths) -> &[u8] {
        self.index_once.call_once(|| {
            let index = unsafe { &mut *self.index.get() };
            *index = db.lock()
                       .with_recording_playback(self.s.id, |playback| self.build_index(playback))
                       .map_err(|e| { error!("Unable to build index for segment: {:?}", e); });
        });
        let index: &'a _ = unsafe { &*self.index.get() };
        match *index {
            Ok(ref b) => return Ok(f(&b[..], self.lens())),
            Err(()) => bail!("Unable to build index; see previous error."),
        }
    }

    fn lens(&self) -> SegmentLengths {
        SegmentLengths {
            stts: mem::size_of::<u32>() * 2 * (self.s.frames as usize),
            stsz: mem::size_of::<u32>() * self.s.frames as usize,
            stss: mem::size_of::<u32>() * self.s.key_frames as usize,
        }
    }

    fn stts(buf: &[u8], lens: SegmentLengths) -> &[u8] { &buf[.. lens.stts] }
    fn stsz(buf: &[u8], lens: SegmentLengths) -> &[u8] { &buf[lens.stts .. lens.stts + lens.stsz] }
    fn stss(buf: &[u8], lens: SegmentLengths) -> &[u8] { &buf[lens.stts + lens.stsz ..] }

    fn build_index(&self, playback: &db::RecordingPlayback) -> Result<Box<[u8]>, Error> {
        let s = &self.s;
        let lens = self.lens();
        let len = lens.stts + lens.stsz + lens.stss;
        let mut buf = {
            let mut v = Vec::with_capacity(len);
            unsafe { v.set_len(len) };
            v.into_boxed_slice()
        };

        {
            let (stts, rest) = buf.split_at_mut(lens.stts);
            let (stsz, stss) = rest.split_at_mut(lens.stsz);
            let mut frame = 0;
            let mut key_frame = 0;
            let mut last_start_and_dur = None;
            s.foreach(playback, |it| {
                last_start_and_dur = Some((it.start_90k, it.duration_90k));
                BigEndian::write_u32(&mut stts[8*frame .. 8*frame+4], 1);
                BigEndian::write_u32(&mut stts[8*frame+4 .. 8*frame+8], it.duration_90k as u32);
                BigEndian::write_u32(&mut stsz[4*frame .. 4*frame+4], it.bytes as u32);
                if it.is_key() {
                    BigEndian::write_u32(&mut stss[4*key_frame .. 4*key_frame+4],
                                         self.first_frame_num + (frame as u32));
                    key_frame += 1;
                }
                frame += 1;
                Ok(())
            })?;

            // Fix up the final frame's duration.
            // Doing this after the fact is more efficient than having a condition on every
            // iteration.
            if let Some((last_start, dur)) = last_start_and_dur {
                BigEndian::write_u32(&mut stts[8*frame-4 ..],
                                     cmp::min(s.desired_range_90k.end - last_start, dur) as u32);
            }
        }

        Ok(buf)
    }

    fn truns_len(&self) -> usize {
        (self.s.key_frames as usize) * (mem::size_of::<u32>() * 6) +
        (    self.s.frames as usize) * (mem::size_of::<u32>() * 2)
    }

    // TrackRunBox / trun (8.8.8).
    fn truns(&self, playback: &db::RecordingPlayback, initial_pos: u64, len: usize)
             -> Result<Vec<u8>, Error> {
        let mut v = Vec::with_capacity(len);

        struct RunInfo {
            box_len_pos: usize,
            sample_count_pos: usize,
            count: u32,
            last_start: i32,
            last_dur: i32,
        }
        let mut run_info: Option<RunInfo> = None;
        let mut data_pos = initial_pos;
        self.s.foreach(playback, |it| {
            if it.is_key() {
                if let Some(r) = run_info.take() {
                    // Finish a non-terminal run.
                    let p = v.len();
                    BigEndian::write_u32(&mut v[r.box_len_pos .. r.box_len_pos + 4],
                                         (p - r.box_len_pos) as u32);
                    BigEndian::write_u32(&mut v[r.sample_count_pos .. r.sample_count_pos + 4],
                                         r.count);
                }
                let box_len_pos = v.len();
                v.extend_from_slice(&[
                    0x00, 0x00, 0x00, 0x00,  // placeholder for size
                    b't', b'r', b'u', b'n',

                    // version 0, tr_flags:
                    // 0x000001 data-offset-present
                    // 0x000004 first-sample-flags-present
                    // 0x000100 sample-duration-present
                    // 0x000200 sample-size-present
                    0x00, 0x00, 0x03, 0x05,
                    ]);
                run_info = Some(RunInfo {
                    box_len_pos,
                    sample_count_pos: v.len(),
                    count: 1,
                    last_start: it.start_90k,
                    last_dur: it.duration_90k,
                });
                v.write_u32::<BigEndian>(0)?;  // placeholder for sample count
                v.write_u32::<BigEndian>(data_pos as u32)?;

                // first_sample_flags. See trex (8.8.3.1).
                v.write_u32::<BigEndian>(
                    // As defined by the Independent and Disposable Samples Box (sdp, 8.6.4).
                    (2 << 26) |  // is_leading: this sample is not a leading sample
                    (2 << 24) |  // sample_depends_on: this sample does not depend on others
                    (1 << 22) |  // sample_is_depend_on: others may depend on this one
                    (2 << 20) |  // sample_has_redundancy: no redundant coding
                    // As defined by the sample padding bits (padb, 8.7.6).
                    (0 << 17) |  // no padding
                    (0 << 16) |  // sample_is_non_sync_sample=0
                    0)?;         // TODO: sample_degradation_priority
            } else {
                let r = run_info.as_mut().expect("non-key sample must be preceded by key sample");
                r.count += 1;
                r.last_start = it.start_90k;
                r.last_dur = it.duration_90k;
            }
            v.write_u32::<BigEndian>(it.duration_90k as u32)?;
            v.write_u32::<BigEndian>(it.bytes as u32)?;
            data_pos += it.bytes as u64;
            Ok(())
        })?;
        if let Some(r) = run_info.take() {
            // Finish the run as in the non-terminal case above.
            let p = v.len();
            BigEndian::write_u32(&mut v[r.box_len_pos .. r.box_len_pos + 4],
                                 (p - r.box_len_pos) as u32);
            BigEndian::write_u32(&mut v[r.sample_count_pos .. r.sample_count_pos + 4], r.count);

            // One more thing to do in the terminal case: fix up the final frame's duration.
            // Doing this after the fact is more efficient than having a condition on every
            // iteration.
            BigEndian::write_u32(&mut v[p-8 .. p-4],
                                 cmp::min(self.s.desired_range_90k.end - r.last_start,
                                          r.last_dur) as u32);

        }
        Ok(v)
    }
}

pub struct FileBuilder {
    /// Segments of video: one per "recording" table entry as they should
    /// appear in the video.
    segments: Vec<Segment>,
    video_sample_entries: SmallVec<[Arc<db::VideoSampleEntry>; 1]>,
    next_frame_num: u32,
    duration_90k: u32,
    num_subtitle_samples: u32,
    subtitle_co64_pos: Option<usize>,
    body: BodyState,
    type_: Type,
    include_timestamp_subtitle_track: bool,
}

/// The portion of `FileBuilder` which is mutated while building the body of the file.
/// This is separated out from the rest so that it can be borrowed in a loop over
/// `FileBuilder::segments`; otherwise this would cause a double-self-borrow.
struct BodyState {
    slices: Slices<Slice>,

    /// `self.buf[unflushed_buf_pos .. self.buf.len()]` holds bytes that should be
    /// appended to `slices` before any other slice. See `flush_buf()`.
    unflushed_buf_pos: usize,
    buf: Vec<u8>,
}

/// A single slice of a `File`, for use with a `Slices` object. Each slice is responsible for
/// some portion of the generated `.mp4` file. The box headers and such are generally in `Static`
/// or `Buf` slices; the others generally represent a single segment's contribution to the
/// like-named box.
///
/// This is stored in a packed representation to be more cache-efficient:
///
///    * low 40 bits: end() (maximum 1 TiB).
///    * next 4 bits: t(), the SliceType.
///    * top 20 bits: p(), a parameter specified by the SliceType (maximum 1 Mi).
struct Slice(u64);

/// The type of a `Slice`.
#[derive(Copy, Clone, Debug)]
#[repr(u8)]
enum SliceType {
    Static = 0,              // param is index into STATIC_BYTESTRINGS
    Buf = 1,                 // param is index into m.buf
    VideoSampleEntry = 2,    // param is index into m.video_sample_entries
    Stts = 3,                // param is index into m.segments
    Stsz = 4,                // param is index into m.segments
    Stss = 5,                // param is index into m.segments
    Co64 = 6,                // param is unused
    VideoSampleData = 7,     // param is index into m.segments
    SubtitleSampleData = 8,  // param is index into m.segments
    Truns = 9,               // param is index into m.segments

    // There must be no value > 15, as this is packed into 4 bits in Slice.
}

impl Slice {
    fn new(end: u64, t: SliceType, p: usize) -> Result<Self, Error> {
        if end >= (1<<40) || p >= (1<<20) {
            bail!("end={} p={} too large for Slice", end, p);
        }

        Ok(Slice(end | ((t as u64) << 40) | ((p as u64) << 44)))
    }

    fn t(&self) -> SliceType {
        // This value is guaranteed to be a valid SliceType because it was copied from a SliceType
        // in Slice::new.
        unsafe { ::std::mem::transmute(((self.0 >> 40) & 0xF) as u8) }
    }
    fn p(&self) -> usize { (self.0 >> 44) as usize }

    fn wrap_index<F>(&self, mp4: &File, r: Range<u64>, f: &F) -> Result<Chunk, Error>
    where F: Fn(&[u8], SegmentLengths) -> &[u8] {
        let mp4 = ARefs::new(mp4.0.clone());
        let r = r.start as usize .. r.end as usize;
        let p = self.p();
        mp4.try_map(|mp4| Ok(&mp4.segments[p].get_index(&mp4.db, f)?[r]))
    }

    fn wrap_truns(&self, mp4: &File, r: Range<u64>, len: usize) -> Result<Chunk, Error> {
        let s = &mp4.0.segments[self.p()];
        let mut pos = mp4.0.initial_sample_byte_pos;
        for ps in &mp4.0.segments[0 .. self.p()] {
            let r = ps.s.sample_file_range();
            pos += r.end - r.start;
        }
        let truns =
            mp4.0.db.lock()
               .with_recording_playback(s.s.id, |playback| s.truns(playback, pos, len))?;
        let truns = ARefs::new(truns);
        Ok(truns.map(|t| &t[r.start as usize .. r.end as usize]))
    }
}

impl slices::Slice for Slice {
    type Ctx = File;
    type Chunk = slices::Chunk;

    fn end(&self) -> u64 { return self.0 & 0xFF_FF_FF_FF_FF }
    fn get_range(&self, f: &File, range: Range<u64>, len: u64) -> Body {
        trace!("getting mp4 slice {:?}'s range {:?} / {}", self, range, len);
        let p = self.p();
        let res = match self.t() {
            SliceType::Static => {
                let s = STATIC_BYTESTRINGS[p];
                let part = &s[range.start as usize .. range.end as usize];
                Ok(part.into())
            },
            SliceType::Buf => {
                let r = ARefs::new(f.0.clone());
                Ok(r.map(|f| &f.buf[p+range.start as usize .. p+range.end as usize]))
            },
            SliceType::VideoSampleEntry => {
                let r = ARefs::new(f.0.clone());
                Ok(r.map(|f| &f.video_sample_entries[p]
                               .data[range.start as usize .. range.end as usize]))
            },
            SliceType::Stts => self.wrap_index(f, range.clone(), &Segment::stts),
            SliceType::Stsz => self.wrap_index(f, range.clone(), &Segment::stsz),
            SliceType::Stss => self.wrap_index(f, range.clone(), &Segment::stss),
            SliceType::Co64 => f.0.get_co64(range.clone(), len),
            SliceType::VideoSampleData => f.0.get_video_sample_data(p, range.clone()),
            SliceType::SubtitleSampleData => f.0.get_subtitle_sample_data(p, range.clone(), len),
            SliceType::Truns => self.wrap_truns(f, range.clone(), len as usize),
        };
        Box::new(stream::once(res
            .map_err(|e| {
                error!("Error producing {:?}: {:?}", self, e);
                ::hyper::Error::Incomplete
            })
            .and_then(move |c| {
                if c.len() != (range.end - range.start) as usize {
                    error!("Error producing {:?}: range {:?} produced incorrect len {}.",
                           self, range, c.len());
                    return Err(::hyper::Error::Incomplete);
                }
                Ok(c)
            })))
    }

    fn get_slices(ctx: &File) -> &Slices<Self> { &ctx.0.slices }
}

impl ::std::fmt::Debug for Slice {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> Result<(), ::std::fmt::Error> {
        // Write an unpacked representation. Omit end(); Slices writes that part.
        write!(f, "{:?} {}", self.t(), self.p())
    }
}

/// Converts from seconds since Unix epoch (1970-01-01 00:00:00 UTC) to seconds since
/// ISO-14496 epoch (1904-01-01 00:00:00 UTC).
fn to_iso14496_timestamp(unix_secs: i64) -> u32 { unix_secs as u32 + 24107 * 86400 }

/// Writes a box length for everything appended in the supplied scope.
/// Used only within FileBuilder::build (and methods it calls internally).
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
        Ok::<_, Error>(())
    }}
}

#[derive(PartialEq, Eq)]
pub enum Type {
    Normal,
    InitSegment,
    MediaSegment,
}

impl FileBuilder {
    pub fn new(type_: Type) -> Self {
        FileBuilder {
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
            type_: type_,
            include_timestamp_subtitle_track: false,
        }
    }

    /// Sets if the generated `.mp4` should include a subtitle track with second-level timestamps.
    /// Default is false.
    pub fn include_timestamp_subtitle_track(&mut self, b: bool) {
        self.include_timestamp_subtitle_track = b;
    }

    /// Reserves space for the given number of additional segments.
    pub fn reserve(&mut self, additional: usize) {
        self.segments.reserve(additional);
    }

    pub fn append_video_sample_entry(&mut self, ent: Arc<db::VideoSampleEntry>) {
        self.video_sample_entries.push(ent);
    }

    /// Appends a segment for (a subset of) the given recording.
    pub fn append(&mut self, db: &db::LockedDatabase, row: db::ListRecordingsRow,
                  rel_range_90k: Range<i32>) -> Result<(), Error> {
        if let Some(prev) = self.segments.last() {
            if prev.s.have_trailing_zero() {
                bail!("unable to append recording {} after recording {} with trailing zero",
                      row.id, prev.s.id);
            }
        }
        let s = Segment::new(db, &row, rel_range_90k, self.next_frame_num)?;

        self.next_frame_num += s.s.frames as u32;
        self.segments.push(s);
        if !self.video_sample_entries.iter().any(|e| e.id == row.video_sample_entry_id) {
            let vse = db.video_sample_entries_by_id().get(&row.video_sample_entry_id).unwrap();
            self.video_sample_entries.push(vse.clone());
        }
        Ok(())
    }

    /// Builds the `File`, consuming the builder.
    pub fn build(mut self, db: Arc<db::Database>,
                 dirs_by_stream_id: Arc<::fnv::FnvHashMap<i32, Arc<dir::SampleFileDir>>>)
                 -> Result<File, Error> {
        let mut max_end = None;
        let mut etag = hash::Hasher::new(hash::MessageDigest::sha1())?;
        etag.update(&FORMAT_VERSION[..])?;
        if self.include_timestamp_subtitle_track {
            etag.update(b":ts:")?;
        }
        match self.type_ {
            Type::Normal => {},
            Type::InitSegment => etag.update(b":init:")?,
            Type::MediaSegment => etag.update(b":media:")?,
        };
        for s in &mut self.segments {
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
                s.num_subtitle_samples = (end_sec - start_sec) as u16;
                self.num_subtitle_samples += s.num_subtitle_samples as u32;
            }

            // Update the etag to reflect this segment.
            let mut data = [0_u8; 28];
            let mut cursor = io::Cursor::new(&mut data[..]);
            cursor.write_i64::<BigEndian>(s.s.id.0)?;
            cursor.write_i64::<BigEndian>(s.s.start.0)?;
            cursor.write_u32::<BigEndian>(s.s.open_id)?;
            cursor.write_i32::<BigEndian>(d.start)?;
            cursor.write_i32::<BigEndian>(d.end)?;
            etag.update(cursor.into_inner())?;
        }
        let max_end = match max_end {
            None => 0,
            Some(v) => v.unix_seconds(),
        };
        let creation_ts = to_iso14496_timestamp(max_end);
        let mut est_slices = 16 + self.video_sample_entries.len() + 4 * self.segments.len();
        if self.include_timestamp_subtitle_track {
            est_slices += 16 + self.segments.len();
        }
        self.body.slices.reserve(est_slices);
        const EST_BUF_LEN: usize = 2048;
        self.body.buf.reserve(EST_BUF_LEN);
        let initial_sample_byte_pos = match self.type_ {
            Type::MediaSegment => {
                self.append_moof()?;
                let p = self.append_mdat()?;

                // If the segment is > 4 GiB, the 32-bit trun data offsets are untrustworthy.
                // We'd need multiple moof+mdat sequences to support large media segments properly.
                if self.body.slices.len() > u32::max_value() as u64 {
                    bail!("media segment has length {}, greater than allowed 4 GiB",
                          self.body.slices.len());
                }

                p
            },
            Type::InitSegment => {
                self.body.append_static(StaticBytestring::InitSegmentFtypBox)?;
                self.append_moov(creation_ts)?;
                self.body.flush_buf()?;
                0
            },
            Type::Normal => {
                self.body.append_static(StaticBytestring::NormalFtypBox)?;
                self.append_moov(creation_ts)?;
                self.append_mdat()?
            },
        };

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
        debug!("segments: {:#?}", self.segments);
        debug!("slices: {:?}", self.body.slices);
        let mtime = ::std::time::UNIX_EPOCH +
                    ::std::time::Duration::from_secs(max_end as u64);
        Ok(File(Arc::new(FileInner {
            db,
            dirs_by_stream_id,
            segments: self.segments,
            slices: self.body.slices,
            buf: self.body.buf,
            video_sample_entries: self.video_sample_entries,
            initial_sample_byte_pos,
            last_modified: mtime.into(),
            etag: header::EntityTag::strong(strutil::hex(&etag.finish()?)),
        })))
    }

    fn append_mdat(&mut self) -> Result<u64, Error> {
        // Write the mdat header. Use the large format to support files over 2^32-1 bytes long.
        // Write zeroes for the length as a placeholder; fill it in after it's known.
        // It'd be nice to use the until-EOF form, but QuickTime Player doesn't support it.
        self.body.buf.extend_from_slice(b"\x00\x00\x00\x01mdat\x00\x00\x00\x00\x00\x00\x00\x00");
        let mdat_len_pos = self.body.buf.len() - 8;
        self.body.flush_buf()?;
        let initial_sample_byte_pos = self.body.slices.len();
        for (i, s) in self.segments.iter().enumerate() {
            let r = s.s.sample_file_range();
            self.body.append_slice(r.end - r.start, SliceType::VideoSampleData, i)?;
        }
        if let Some(p) = self.subtitle_co64_pos {
            BigEndian::write_u64(&mut self.body.buf[p .. p + 8], self.body.slices.len());
            for (i, s) in self.segments.iter().enumerate() {
                self.body.append_slice(
                    s.num_subtitle_samples as u64 *
                    (mem::size_of::<u16>() + SUBTITLE_LENGTH) as u64,
                    SliceType::SubtitleSampleData, i)?;
            }
        }
        // Fill in the length left as a placeholder above. Note the 16 here is the length
        // of the mdat header.
        BigEndian::write_u64(&mut self.body.buf[mdat_len_pos .. mdat_len_pos + 8],
                             16 + self.body.slices.len() - initial_sample_byte_pos);
        Ok(initial_sample_byte_pos)
    }

    /// Appends a `MovieBox` (ISO/IEC 14496-12 section 8.2.1).
    fn append_moov(&mut self, creation_ts: u32) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"moov");
            self.append_mvhd(creation_ts)?;
            self.append_video_trak(creation_ts)?;
            if self.include_timestamp_subtitle_track {
                self.append_subtitle_trak(creation_ts)?;
            }
            if self.type_ == Type::InitSegment {
                self.append_mvex()?;
            }
        })
    }

    /// Appends a `MovieExtendsBox` (ISO/IEC 14496-12 section 8.8.1).
    fn append_mvex(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"mvex");

            // Appends a `TrackExtendsBox` (ISO/IEC 14496-12 section 8.8.3) for the video track.
            write_length!(self, {
                self.body.buf.extend_from_slice(&[
                    b't', b'r', b'e', b'x',
                    0x00, 0x00, 0x00, 0x00,  // version + flags
                    0x00, 0x00, 0x00, 0x01,  // track_id
                    0x00, 0x00, 0x00, 0x01,  // default_sample_description_index
                    0x00, 0x00, 0x00, 0x00,  // default_sample_duration
                    0x00, 0x00, 0x00, 0x00,  // default_sample_size
                    0x09, 0x21, 0x00, 0x00,  // default_sample_flags (non sync):
                                             // is_leading: not a leading sample
                                             // sample_depends_on: does depend on others
                                             // sample_is_depend_on: unknown
                                             // sample_has_redundancy: no
                                             // no padding
                                             // sample_is_non_sync_sample: 1
                                             // sample_degradation_priority: 0
                ]);
            })?;
        })
    }

    /// Appends a `MovieFragmentBox` (ISO/IEC 14496-12 section 8.8.4).
    fn append_moof(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"moof");

            // MovieFragmentHeaderBox (ISO/IEC 14496-12 section 8.8.5).
            write_length!(self, {
                self.body.buf.extend_from_slice(b"mfhd\x00\x00\x00\x00");
                self.body.append_u32(1);  // sequence_number
            })?;

            // TrackFragmentBox (ISO/IEC 14496-12 section 8.8.6).
            write_length!(self, {
                self.body.buf.extend_from_slice(b"traf");

                // TrackFragmentHeaderBox (ISO/IEC 14496-12 section 8.8.7).
                write_length!(self, {
                    self.body.buf.extend_from_slice(&[
                        b't', b'f', b'h', b'd',
                        0x00, 0x02, 0x00, 0x00,  // version + flags (default-base-is-moof)
                        0x00, 0x00, 0x00, 0x01,  // track_id = 1
                    ]);
                })?;
                self.append_truns()?;

                // `TrackFragmentBaseMediaDecodeTimeBox` (ISO/IEC 14496-12 section 8.8.12).
                write_length!(self, {
                    self.body.buf.extend_from_slice(&[
                        b't', b'f', b'd', b't',
                        0x00, 0x00, 0x00, 0x00,  // version + flags
                        0x00, 0x00, 0x00, 0x00,  // TODO: baseMediaDecodeTime
                    ]);
                })?;
            })?;
        })
    }

    fn append_truns(&mut self) -> Result<(), Error> {
        self.body.flush_buf()?;
        for (i, s) in self.segments.iter().enumerate() {
            self.body.append_slice(s.truns_len() as u64, SliceType::Truns, i)?;
        }
        Ok(())
    }

    /// Appends a `MovieHeaderBox` version 0 (ISO/IEC 14496-12 section 8.2.2).
    fn append_mvhd(&mut self, creation_ts: u32) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"mvhd\x00\x00\x00\x00");
            self.body.append_u32(creation_ts);
            self.body.append_u32(creation_ts);
            self.body.append_u32(TIME_UNITS_PER_SEC as u32);
            let d = self.duration_90k;
            self.body.append_u32(d);
            self.body.append_static(StaticBytestring::MvhdJunk)?;
            let next_track_id = if self.include_timestamp_subtitle_track { 3 } else { 2 };
            self.body.append_u32(next_track_id);
        })
    }

    /// Appends a `TrackBox` (ISO/IEC 14496-12 section 8.3.1) suitable for video.
    fn append_video_trak(&mut self, creation_ts: u32) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"trak");
            self.append_video_tkhd(creation_ts)?;
            self.maybe_append_video_edts()?;
            self.append_video_mdia(creation_ts)?;
        })
    }

    /// Appends a `TrackBox` (ISO/IEC 14496-12 section 8.3.1) suitable for subtitles.
    fn append_subtitle_trak(&mut self, creation_ts: u32) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"trak");
            self.append_subtitle_tkhd(creation_ts)?;
            self.append_subtitle_mdia(creation_ts)?;
        })
    }

    /// Appends a `TrackHeaderBox` (ISO/IEC 14496-12 section 8.3.2) suitable for video.
    fn append_video_tkhd(&mut self, creation_ts: u32) -> Result<(), Error> {
        write_length!(self, {
            // flags 7: track_enabled | track_in_movie | track_in_preview
            self.body.buf.extend_from_slice(b"tkhd\x00\x00\x00\x07");
            self.body.append_u32(creation_ts);
            self.body.append_u32(creation_ts);
            self.body.append_u32(1);  // track_id
            self.body.append_u32(0);  // reserved
            self.body.append_u32(self.duration_90k);
            self.body.append_static(StaticBytestring::TkhdJunk)?;
            let width = self.video_sample_entries.iter().map(|e| e.width).max().unwrap();
            let height = self.video_sample_entries.iter().map(|e| e.height).max().unwrap();
            self.body.append_u32((width as u32) << 16);
            self.body.append_u32((height as u32) << 16);
        })
    }

    /// Appends a `TrackHeaderBox` (ISO/IEC 14496-12 section 8.3.2) suitable for subtitles.
    fn append_subtitle_tkhd(&mut self, creation_ts: u32) -> Result<(), Error> {
        write_length!(self, {
            // flags 7: track_enabled | track_in_movie | track_in_preview
            self.body.buf.extend_from_slice(b"tkhd\x00\x00\x00\x07");
            self.body.append_u32(creation_ts);
            self.body.append_u32(creation_ts);
            self.body.append_u32(2);  // track_id
            self.body.append_u32(0);  // reserved
            self.body.append_u32(self.duration_90k);
            self.body.append_static(StaticBytestring::TkhdJunk)?;
            self.body.append_u32(0);  // width, unused.
            self.body.append_u32(0);  // height, unused.
        })
    }

    /// Appends an `EditBox` (ISO/IEC 14496-12 section 8.6.5) suitable for video, if necessary.
    fn maybe_append_video_edts(&mut self) -> Result<(), Error> {
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
            // actual start <= desired start <= desired end
            let actual_start_90k = s.s.actual_start_90k();
            let skip = s.s.desired_range_90k.start - actual_start_90k;
            let keep = s.s.desired_range_90k.end - s.s.desired_range_90k.start;
            if skip < 0 || keep < 0 {
                bail!("skip={} keep={} on segment {:#?}", skip, keep, s);
            }
            cur_media_time += skip as u64;
            if unflushed.segment_duration + unflushed.media_time == cur_media_time {
                unflushed.segment_duration += keep as u64;
            } else {
                if unflushed.segment_duration > 0 {
                    flushed.push(unflushed);
                }
                unflushed = Entry {
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

                    // media_rate_integer + media_rate_fraction: fixed at 1.0
                    self.body.buf.extend_from_slice(b"\x00\x01\x00\x00");
                }
            })?;
        })
    }

    /// Appends a `MediaBox` (ISO/IEC 14496-12 section 8.4.1) suitable for video.
    fn append_video_mdia(&mut self, creation_ts: u32) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"mdia");
            self.append_mdhd(creation_ts)?;
            self.body.append_static(StaticBytestring::VideoHdlrBox)?;
            self.append_video_minf()?;
        })
    }

    /// Appends a `MediaBox` (ISO/IEC 14496-12 section 8.4.1) suitable for subtitles.
    fn append_subtitle_mdia(&mut self, creation_ts: u32) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"mdia");
            self.append_mdhd(creation_ts)?;
            self.body.append_static(StaticBytestring::SubtitleHdlrBox)?;
            self.append_subtitle_minf()?;
        })
    }

    /// Appends a `MediaHeaderBox` (ISO/IEC 14496-12 section 8.4.2.) suitable for either the video
    /// or subtitle track.
    fn append_mdhd(&mut self, creation_ts: u32) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"mdhd\x00\x00\x00\x00");
            self.body.append_u32(creation_ts);
            self.body.append_u32(creation_ts);
            self.body.append_u32(TIME_UNITS_PER_SEC as u32);
            self.body.append_u32(self.duration_90k);
            self.body.append_u32(0x55c40000);  // language=und + pre_defined
        })
    }

    /// Appends a `MediaInformationBox` (ISO/IEC 14496-12 section 8.4.4) suitable for video.
    fn append_video_minf(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.append_static(StaticBytestring::VideoMinfJunk)?;
            self.append_video_stbl()?;
        })
    }

    /// Appends a `MediaInformationBox` (ISO/IEC 14496-12 section 8.4.4) suitable for subtitles.
    fn append_subtitle_minf(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.append_static(StaticBytestring::SubtitleMinfJunk)?;
            self.append_subtitle_stbl()?;
        })
    }

    /// Appends a `SampleTableBox` (ISO/IEC 14496-12 section 8.5.1) suitable for video.
    fn append_video_stbl(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stbl");
            self.append_video_stsd()?;
            self.append_video_stts()?;
            self.append_video_stsc()?;
            self.append_video_stsz()?;
            self.append_video_co64()?;
            self.append_video_stss()?;
        })
    }

    /// Appends a `SampleTableBox` (ISO/IEC 14496-12 section 8.5.1) suitable for subtitles.
    fn append_subtitle_stbl(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.append_static(StaticBytestring::SubtitleStblJunk)?;
            self.append_subtitle_stts()?;
            self.append_subtitle_stsc()?;
            self.append_subtitle_stsz()?;
            self.append_subtitle_co64()?;
        })
    }

    /// Appends a `SampleDescriptionBox` (ISO/IEC 14496-12 section 8.5.2) suitable for video.
    fn append_video_stsd(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stsd\x00\x00\x00\x00");
            let n_entries = self.video_sample_entries.len() as u32;
            self.body.append_u32(n_entries);
            self.body.flush_buf()?;
            for (i, e) in self.video_sample_entries.iter().enumerate() {
                self.body.append_slice(e.data.len() as u64, SliceType::VideoSampleEntry, i)?;
            }
        })
    }

    /// Appends a `TimeToSampleBox` (ISO/IEC 14496-12 section 8.6.1) suitable for video.
    fn append_video_stts(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stts\x00\x00\x00\x00");
            let mut entry_count = 0;
            for s in &self.segments {
                entry_count += s.s.frames as u32;
            }
            self.body.append_u32(entry_count);
            if !self.segments.is_empty() {
                self.body.flush_buf()?;
                for (i, s) in self.segments.iter().enumerate() {
                    self.body.append_slice(
                        2 * (mem::size_of::<u32>() as u64) * (s.s.frames as u64),
                        SliceType::Stts, i)?;
                }
            }
        })
    }

    /// Appends a `TimeToSampleBox` (ISO/IEC 14496-12 section 8.6.1) suitable for subtitles.
    fn append_subtitle_stts(&mut self) -> Result<(), Error> {
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
        })
    }

    /// Appends a `SampleToChunkBox` (ISO/IEC 14496-12 section 8.7.4) suitable for video.
    fn append_video_stsc(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stsc\x00\x00\x00\x00");
            self.body.append_u32(self.segments.len() as u32);
            for (i, s) in self.segments.iter().enumerate() {
                self.body.append_u32((i + 1) as u32);
                self.body.append_u32(s.s.frames as u32);

                // Write sample_description_index.
                let i = self.video_sample_entries.iter().position(
                    |e| e.id == s.s.video_sample_entry_id()).unwrap();
                self.body.append_u32((i + 1) as u32);
            }
        })
    }

    /// Appends a `SampleToChunkBox` (ISO/IEC 14496-12 section 8.7.4) suitable for subtitles.
    fn append_subtitle_stsc(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(
                b"stsc\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x01");
            self.body.append_u32(self.num_subtitle_samples as u32);
            self.body.append_u32(1);
        })
    }

    /// Appends a `SampleSizeBox` (ISO/IEC 14496-12 section 8.7.3) suitable for video.
    fn append_video_stsz(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stsz\x00\x00\x00\x00\x00\x00\x00\x00");
            let mut entry_count = 0;
            for s in &self.segments {
                entry_count += s.s.frames as u32;
            }
            self.body.append_u32(entry_count);
            if !self.segments.is_empty() {
                self.body.flush_buf()?;
                for (i, s) in self.segments.iter().enumerate() {
                    self.body.append_slice(
                        (mem::size_of::<u32>()) as u64 * (s.s.frames as u64), SliceType::Stsz, i)?;
                }
            }
        })
    }

    /// Appends a `SampleSizeBox` (ISO/IEC 14496-12 section 8.7.3) suitable for subtitles.
    fn append_subtitle_stsz(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stsz\x00\x00\x00\x00");
            self.body.append_u32((mem::size_of::<u16>() + SUBTITLE_LENGTH) as u32);
            self.body.append_u32(self.num_subtitle_samples as u32);
        })
    }

    /// Appends a `ChunkLargeOffsetBox` (ISO/IEC 14496-12 section 8.7.5) suitable for video.
    fn append_video_co64(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"co64\x00\x00\x00\x00");
            self.body.append_u32(self.segments.len() as u32);
            if !self.segments.is_empty() {
                self.body.flush_buf()?;
                self.body.append_slice(
                    (mem::size_of::<u64>()) as u64 * (self.segments.len() as u64),
                    SliceType::Co64, 0)?;
            }
        })
    }

    /// Appends a `ChunkLargeOffsetBox` (ISO/IEC 14496-12 section 8.7.5) suitable for subtitles.
    fn append_subtitle_co64(&mut self) -> Result<(), Error> {
        write_length!(self, {
            // Write a placeholder; the actual value will be filled in later.
            self.body.buf.extend_from_slice(
                b"co64\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00");
            self.subtitle_co64_pos = Some(self.body.buf.len() - 8);
        })
    }

    /// Appends a `SyncSampleBox` (ISO/IEC 14496-12 section 8.6.2) suitable for video.
    fn append_video_stss(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stss\x00\x00\x00\x00");
            let mut entry_count = 0;
            for s in &self.segments {
                entry_count += s.s.key_frames as u32;
            }
            self.body.append_u32(entry_count);
            if !self.segments.is_empty() {
                self.body.flush_buf()?;
                for (i, s) in self.segments.iter().enumerate() {
                    self.body.append_slice(
                        (mem::size_of::<u32>() as u64) * (s.s.key_frames as u64),
                        SliceType::Stss, i)?;
                }
            }
        })
    }
}

impl BodyState {
    fn append_u32(&mut self, v: u32) {
        self.buf.write_u32::<BigEndian>(v).expect("Vec write shouldn't fail");
    }

    fn append_u64(&mut self, v: u64) {
        self.buf.write_u64::<BigEndian>(v).expect("Vec write shouldn't fail");
    }

    /// Flushes the buffer: appends a slice for everything written into the buffer so far,
    /// noting the position which has been flushed. Call this method prior to adding any non-buffer
    /// slice.
    fn flush_buf(&mut self) -> Result<(), Error> {
        let len = self.buf.len();
        if self.unflushed_buf_pos < len {
            let p = self.unflushed_buf_pos;
            self.append_slice((len - p) as u64, SliceType::Buf, p)?;
            self.unflushed_buf_pos = len;
        }
        Ok(())
    }

    fn append_slice(&mut self, len: u64, t: SliceType, p: usize) -> Result<(), Error> {
        let l = self.slices.len();
        self.slices.append(Slice::new(l + len, t, p)?)
    }

    /// Appends a static bytestring, flushing the buffer if necessary.
    fn append_static(&mut self, which: StaticBytestring) -> Result<(), Error> {
        self.flush_buf()?;
        let s = STATIC_BYTESTRINGS[which as usize];
        self.append_slice(s.len() as u64, SliceType::Static, which as usize)
    }
}

struct FileInner {
    db: Arc<db::Database>,
    dirs_by_stream_id: Arc<::fnv::FnvHashMap<i32, Arc<dir::SampleFileDir>>>,
    segments: Vec<Segment>,
    slices: Slices<Slice>,
    buf: Vec<u8>,
    video_sample_entries: SmallVec<[Arc<db::VideoSampleEntry>; 1]>,
    initial_sample_byte_pos: u64,
    last_modified: header::HttpDate,
    etag: header::EntityTag,
}

impl FileInner {
    fn get_co64(&self, r: Range<u64>, l: u64) -> Result<Chunk, Error> {
        let mut v = Vec::with_capacity(l as usize);
        let mut pos = self.initial_sample_byte_pos;
        for s in &self.segments {
            v.write_u64::<BigEndian>(pos)?;
            let r = s.s.sample_file_range();
            pos += r.end - r.start;
        }
        Ok(ARefs::new(v).map(|v| &v[r.start as usize .. r.end as usize]))
    }

    /// Gets a `Chunk` of video sample data from disk.
    /// This works by `mmap()`ing in the data. There are a couple caveats:
    ///
    ///    * The thread which reads the resulting slice is likely to experience major page faults.
    ///      Eventually this will likely be rewritten to `mmap()` the memory in another thread, and
    ///      `mlock()` and send chunks of it to be read and `munlock()`ed to avoid this problem.
    ///
    ///    * If the backing file is truncated, the program will crash with `SIGBUS`. This shouldn't
    ///      happen because nothing should be touching Moonfire NVR's files but itself.
    fn get_video_sample_data(&self, i: usize, r: Range<u64>) -> Result<Chunk, Error> {
        let s = &self.segments[i];
        let f = self.dirs_by_stream_id
                    .get(&s.s.id.stream())
                    .ok_or_else(|| format_err!("{}: stream not found", s.s.id))?
                    .open_sample_file(s.s.id)?;
        let start = s.s.sample_file_range().start + r.start;
        let mmap = Box::new(unsafe {
            memmap::MmapOptions::new()
                .offset(start as usize)
                .len((r.end - r.start) as usize)
                .map(&f)?
            });
        use core::ops::Deref;
        Ok(ARefs::new(mmap).map(|m| m.deref()))
    }

    fn get_subtitle_sample_data(&self, i: usize, r: Range<u64>, l: u64) -> Result<Chunk, Error> {
        let s = &self.segments[i];
        let d = &s.s.desired_range_90k;
        let start_sec = (s.s.start + recording::Duration(d.start as i64)).unix_seconds();
        let end_sec = (s.s.start + recording::Duration(d.end as i64 + TIME_UNITS_PER_SEC - 1))
                      .unix_seconds();
        let mut v = Vec::with_capacity(l as usize);
        for ts in start_sec .. end_sec {
            v.write_u16::<BigEndian>(SUBTITLE_LENGTH as u16)?;
            let tm = time::at(time::Timespec{sec: ts, nsec: 0});
            use std::io::Write;
            write!(v, "{}", tm.strftime(SUBTITLE_TEMPLATE)?)?;
        }
        Ok(ARefs::new(v).map(|v| &v[r.start as usize .. r.end as usize]))
    }
}

#[derive(Clone)]
pub struct File(Arc<FileInner>);

impl http_serve::Entity for File {
    type Chunk = slices::Chunk;
    type Body = slices::Body;

    fn add_headers(&self, hdrs: &mut header::Headers) {
        let mut mime = String::with_capacity(64);
        mime.push_str("video/mp4; codecs=\"");
        let mut first = true;
        for e in &self.0.video_sample_entries {
            if first {
                first = false
            } else {
                mime.push_str(", ");
            }
            mime.push_str(&e.rfc6381_codec);
        }
        mime.push('"');
        hdrs.set(header::ContentType(mime.parse().unwrap()));
    }
    fn last_modified(&self) -> Option<header::HttpDate> { Some(self.0.last_modified) }
    fn etag(&self) -> Option<header::EntityTag> { Some(self.0.etag.clone()) }
    fn len(&self) -> u64 { self.0.slices.len() }
    fn get_range(&self, range: Range<u64>) -> Body { self.0.slices.get_range(self, range) }
}

/// Tests. There are two general strategies used to validate the resulting files:
///
///    * basic tests that ffmpeg can read the generated mp4s. This ensures compatibility with
///      popular software, though it's hard to test specifics. ffmpeg provides an abstraction layer
///      over the encapsulation format, so mp4-specific details are hard to see. Also, ffmpeg might
///      paper over errors in the mp4 or have its own bugs.
///
///    * tests using the `BoxCursor` type to inspect the generated mp4s more closely. These don't
///      detect misunderstandings of the specification or incompatibilities, but they can be used
///      to verify the output is byte-for-byte as expected.
#[cfg(test)]
mod tests {
    use byteorder::{BigEndian, ByteOrder};
    use db::recording::{self, TIME_UNITS_PER_SEC};
    use db::testutil::{self, TestDb, TEST_STREAM_ID};
    use futures::Future;
    use futures::Stream as FuturesStream;
    use hyper::header;
    use openssl::hash;
    use http_serve::{self, Entity};
    use std::fs;
    use std::ops::Range;
    use std::path::Path;
    use std::str;
    use strutil;
    use super::*;
    use stream::{self, Opener, Stream};

    fn fill_slice<E: http_serve::Entity>(slice: &mut [u8], e: &E, start: u64) {
        let mut p = 0;
        e.get_range(start .. start + slice.len() as u64)
         .for_each(|chunk| {
             let c: &[u8] = chunk.as_ref();
             slice[p .. p + c.len()].copy_from_slice(c);
             p += c.len();
             Ok::<_, ::hyper::Error>(())
         })
        .wait()
        .unwrap();
    }

    /// Returns the SHA-1 digest of the given `Entity`.
    fn digest<E: http_serve::Entity>(e: &E) -> hash::DigestBytes {
        e.get_range(0 .. e.len())
         .fold(hash::Hasher::new(hash::MessageDigest::sha1()).unwrap(), |mut sha1, chunk| {
             let c: &[u8] = chunk.as_ref();
             sha1.update(c).unwrap();
             Ok::<_, ::hyper::Error>(sha1)
         })
         .wait()
         .unwrap()
         .finish()
         .unwrap()
    }

    /// Information used within `BoxCursor` to describe a box on the stack.
    #[derive(Clone)]
    struct Mp4Box {
        interior: Range<u64>,
        boxtype: [u8; 4],
    }

    /// A cursor over the boxes in a `.mp4` file. Supports moving forward and up/down the box
    /// stack, not backward. Panics on error.
    #[derive(Clone)]
    struct BoxCursor {
        mp4: File,
        stack: Vec<Mp4Box>,
    }

    impl BoxCursor {
        pub fn new(mp4: File) -> BoxCursor {
            BoxCursor{
                mp4: mp4,
                stack: Vec::new(),
            }
        }

        /// Pushes the box at the given position onto the stack (returning true), or returns
        /// false if pos == max.
        fn internal_push(&mut self, pos: u64, max: u64) -> bool {
            if pos == max { return false; }
            let mut hdr = [0u8; 16];
            fill_slice(&mut hdr[..8], &self.mp4, pos);
            let (len, hdr_len, boxtype_slice) = match BigEndian::read_u32(&hdr[..4]) {
                0 => (self.mp4.len() - pos, 8, &hdr[4..8]),
                1 => {
                    fill_slice(&mut hdr[8..], &self.mp4, pos + 8);
                    (BigEndian::read_u64(&hdr[8..16]), 16, &hdr[4..8])
                },
                l => (l as u64, 8, &hdr[4..8]),
            };
            let mut boxtype = [0u8; 4];
            assert!(pos + (hdr_len as u64) <= max);
            assert!(pos + len <= max, "path={} pos={} len={} max={}", self.path(), pos, len, max);
            boxtype[..].copy_from_slice(boxtype_slice);
            self.stack.push(Mp4Box{
                interior: pos + hdr_len as u64 .. pos + len,
                boxtype: boxtype,
            });
            trace!("positioned at {}", self.path());
            true
        }

        fn interior(&self) -> Range<u64> {
            self.stack.last().expect("at root").interior.clone()
        }

        fn path(&self) -> String {
            let mut s = String::with_capacity(5 * self.stack.len());
            for b in &self.stack {
                s.push('/');
                s.push_str(str::from_utf8(&b.boxtype[..]).unwrap());
            }
            s
        }

        fn name(&self) -> &str {
            str::from_utf8(&self.stack.last().expect("at root").boxtype[..]).unwrap()
        }

        /// Gets the specified byte range within the current box (excluding length and type).
        /// Must not be at EOF.
        pub fn get(&self, start: u64, buf: &mut [u8]) {
            let interior = &self.stack.last().expect("at root").interior;
            assert!(start + (buf.len() as u64) <= interior.end - interior.start,
                    "path={} start={} buf.len={} interior={:?}",
                    self.path(), start, buf.len(), interior);
            fill_slice(buf, &self.mp4, start+interior.start);
        }

        pub fn get_all(&self) -> Vec<u8> {
            let interior = self.stack.last().expect("at root").interior.clone();
            let len = (interior.end - interior.start) as usize;
            trace!("get_all: start={}, len={}", interior.start, len);
            let mut out = Vec::with_capacity(len);
            unsafe { out.set_len(len) };
            fill_slice(&mut out[..], &self.mp4, interior.start);
            out
        }

        /// Gets the specified u32 within the current box (excluding length and type).
        /// Must not be at EOF.
        pub fn get_u32(&self, p: u64) -> u32 {
            let mut buf = [0u8; 4];
            self.get(p, &mut buf);
            BigEndian::read_u32(&buf[..])
        }

        pub fn get_u64(&self, p: u64) -> u64 {
            let mut buf = [0u8; 8];
            self.get(p, &mut buf);
            BigEndian::read_u64(&buf[..])
        }

        /// Navigates to the next box after the current one, or up if the current one is last.
        pub fn next(&mut self) -> bool {
            let old = self.stack.pop().expect("positioned at root; there is no next");
            let max = self.stack.last().map(|b| b.interior.end).unwrap_or_else(|| self.mp4.len());
            self.internal_push(old.interior.end, max)
        }

        /// Finds the next box of the given type after the current one, or navigates up if absent.
        pub fn find(&mut self, boxtype: &[u8]) -> bool {
            trace!("looking for {}", str::from_utf8(boxtype).unwrap());
            loop {
                if &self.stack.last().unwrap().boxtype[..] == boxtype {
                    return true;
                }
                if !self.next() {
                    return false;
                }
            }
        }

        /// Moves up the stack. Must not be at root.
        pub fn up(&mut self) { self.stack.pop(); }

        /// Moves down the stack. Must be positioned on a box with children.
        pub fn down(&mut self) {
            let range = self.stack.last().map(|b| b.interior.clone())
                                         .unwrap_or_else(|| 0 .. self.mp4.len());
            assert!(self.internal_push(range.start, range.end), "no children in {}", self.path());
        }
    }

    /// Information returned by `find_track`.
    struct Track {
        edts_cursor: Option<BoxCursor>,
        stbl_cursor: BoxCursor,
    }

    /// Finds the `moov/trak` that has a `tkhd` associated with the given `track_id`, which must
    /// exist.
    fn find_track(mp4: File, track_id: u32) -> Track {
        let mut cursor = BoxCursor::new(mp4);
        cursor.down();
        assert!(cursor.find(b"moov"));
        cursor.down();
        loop {
            assert!(cursor.find(b"trak"));
            cursor.down();
            assert!(cursor.find(b"tkhd"));
            let mut version = [0u8; 1];
            cursor.get(0, &mut version);

            // Let id_pos be the offset after the FullBox section of the track_id.
            let id_pos = match version[0] {
                0 => 8,   // track_id follows 32-bit creation_time and modification_time
                1 => 16,  // ...64-bit times...
                v => panic!("unexpected tkhd version {}", v),
            };
            let cur_track_id = cursor.get_u32(4 + id_pos);
            trace!("found moov/trak/tkhd with id {}; want {}", cur_track_id, track_id);
            if cur_track_id == track_id {
                break;
            }
            cursor.up();
            assert!(cursor.next());
        }
        let edts_cursor;
        if cursor.find(b"edts") {
            edts_cursor = Some(cursor.clone());
            cursor.up();
        } else {
            edts_cursor = None;
        };
        cursor.down();
        assert!(cursor.find(b"mdia"));
        cursor.down();
        assert!(cursor.find(b"minf"));
        cursor.down();
        assert!(cursor.find(b"stbl"));
        Track{
            edts_cursor: edts_cursor,
            stbl_cursor: cursor,
        }
    }

    fn copy_mp4_to_db(db: &TestDb) {
        let mut input =
            stream::FFMPEG.open(stream::Source::File("src/testdata/clip.mp4")).unwrap();

        // 2015-04-26 00:00:00 UTC.
        const START_TIME: recording::Time = recording::Time(1430006400i64 * TIME_UNITS_PER_SEC);
        let extra_data = input.get_extra_data().unwrap();
        let video_sample_entry_id = db.db.lock().insert_video_sample_entry(
            extra_data.width, extra_data.height, extra_data.sample_entry,
            extra_data.rfc6381_codec).unwrap();
        let dir = db.dirs_by_stream_id.get(&TEST_STREAM_ID).unwrap();
        let mut output = dir::Writer::new(dir, &db.db, &db.syncer_channel, TEST_STREAM_ID,
                                          video_sample_entry_id);

        // end_pts is the pts of the end of the most recent frame (start + duration).
        // It's needed because dir::Writer calculates a packet's duration from its pts and the
        // next packet's pts. That's more accurate for RTSP than ffmpeg's estimate of duration.
        // To write the final packet of this sample .mp4 with a full duration, we need to fake a
        // next packet's pts from the ffmpeg-supplied duration.
        let mut end_pts = None;

        let mut frame_time = START_TIME;

        loop {
            let pkt = match input.get_next() {
                Ok(p) => p,
                Err(e) if e.is_eof() => { break; },
                Err(e) => { panic!("unexpected input error: {}", e); },
            };
            let pts = pkt.pts().unwrap();
            frame_time += recording::Duration(pkt.duration() as i64);
            output.write(pkt.data().expect("packet without data"), frame_time, pts,
                         pkt.is_key()).unwrap();
            end_pts = Some(pts + pkt.duration() as i64);
        }
        output.close(end_pts);
        db.syncer_channel.flush();
    }

    pub fn create_mp4_from_db(tdb: &TestDb,
                              skip_90k: i32, shorten_90k: i32, include_subtitles: bool) -> File {
        let mut builder = FileBuilder::new(Type::Normal);
        builder.include_timestamp_subtitle_track(include_subtitles);
        let all_time = recording::Time(i64::min_value()) .. recording::Time(i64::max_value());
        {
            let db = tdb.db.lock();
            db.list_recordings_by_time(TEST_STREAM_ID, all_time, &mut |r| {
                let d = r.duration_90k;
                assert!(skip_90k + shorten_90k < d);
                builder.append(&*db, r, skip_90k .. d - shorten_90k).unwrap();
                Ok(())
            }).unwrap();
        }
        builder.build(tdb.db.clone(), tdb.dirs_by_stream_id.clone()).unwrap()
    }

    fn write_mp4(mp4: &File, dir: &Path) -> String {
        let mut filename = dir.to_path_buf();
        filename.push("clip.new.mp4");
        let mut out = fs::OpenOptions::new().write(true).create_new(true).open(&filename).unwrap();
        use ::std::io::Write;
        mp4.get_range(0 .. mp4.len())
           .for_each(|chunk| {
               out.write_all(&chunk)?;
               Ok(())
           })
           .wait()
           .unwrap();
        info!("wrote {:?}", filename);
        filename.to_str().unwrap().to_string()
    }

    fn compare_mp4s(new_filename: &str, pts_offset: i64, shorten: i64) {
        let mut orig = stream::FFMPEG.open(stream::Source::File("src/testdata/clip.mp4")).unwrap();
        let mut new = stream::FFMPEG.open(stream::Source::File(new_filename)).unwrap();
        assert_eq!(orig.get_extra_data().unwrap(), new.get_extra_data().unwrap());
        let mut final_durations = None;
        loop {
            let orig_pkt = match orig.get_next() {
                Ok(p) => Some(p),
                Err(e) if e.is_eof() => None,
                Err(e) => { panic!("unexpected input error: {}", e); },
            };
            let new_pkt = match new.get_next() {
                Ok(p) => Some(p),
                Err(e) if e.is_eof() => { break; },
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
            final_durations = Some((orig_pkt.duration() as i64, new_pkt.duration() as i64));
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

    /// Makes a `.mp4` file which is only good for exercising the `Slice` logic for producing
    /// sample tables that match the supplied encoder.
    fn make_mp4_from_encoders(type_: Type, db: &TestDb,
                              mut recordings: Vec<db::RecordingToInsert>,
                              desired_range_90k: Range<i32>) -> File {
        let mut builder = FileBuilder::new(type_);
        let mut duration_so_far = 0;
        for r in recordings.drain(..) {
            let row = db.insert_recording_from_encoder(r);
            let d_start = if desired_range_90k.start < duration_so_far { 0 }
                          else { desired_range_90k.start - duration_so_far };
            let d_end = if desired_range_90k.end > duration_so_far + row.duration_90k
                        { row.duration_90k } else { desired_range_90k.end - duration_so_far };
            duration_so_far += row.duration_90k;
            builder.append(&db.db.lock(), row, d_start .. d_end).unwrap();
        }
        builder.build(db.db.clone(), db.dirs_by_stream_id.clone()).unwrap()
    }

    /// Tests sample table for a simple video index of all sync frames.
    #[test]
    fn test_all_sync_frames() {
        testutil::init();
        let db = TestDb::new();
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::new();
        for i in 1..6 {
            let duration_90k = 2 * i;
            let bytes = 3 * i;
            encoder.add_sample(duration_90k, bytes, true, &mut r);
        }

        // Time range [2, 2+4+6+8) means the 2nd, 3rd, and 4th samples should be included.
        let mp4 = make_mp4_from_encoders(Type::Normal, &db, vec![r], 2 .. 2+4+6+8);
        let track = find_track(mp4, 1);
        assert!(track.edts_cursor.is_none());
        let mut cursor = track.stbl_cursor;
        cursor.down();
        cursor.find(b"stts");
        assert_eq!(cursor.get_all(), &[
            0x00, 0x00, 0x00, 0x00,  // version + flags
            0x00, 0x00, 0x00, 0x03,  // entry_count

            // entries
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x04,  // run length / timestamps.
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x08,
        ]);

        cursor.find(b"stsz");
        assert_eq!(cursor.get_all(), &[
            0x00, 0x00, 0x00, 0x00,  // version + flags
            0x00, 0x00, 0x00, 0x00,  // sample_size
            0x00, 0x00, 0x00, 0x03,  // sample_count

            // entries
            0x00, 0x00, 0x00, 0x06,  // size
            0x00, 0x00, 0x00, 0x09,
            0x00, 0x00, 0x00, 0x0c,
        ]);

        cursor.find(b"stss");
        assert_eq!(cursor.get_all(), &[
            0x00, 0x00, 0x00, 0x00,  // version + flags
            0x00, 0x00, 0x00, 0x03,  // entry_count

            // entries
            0x00, 0x00, 0x00, 0x01,  // sample_number
            0x00, 0x00, 0x00, 0x02,
            0x00, 0x00, 0x00, 0x03,
        ]);
    }

    /// Tests sample table and edit list for a video index with half sync frames.
    #[test]
    fn test_half_sync_frames() {
        testutil::init();
        let db = TestDb::new();
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::new();
        for i in 1..6 {
            let duration_90k = 2 * i;
            let bytes = 3 * i;
            encoder.add_sample(duration_90k, bytes, (i % 2) == 1, &mut r);
        }

        // Time range [2+4+6, 2+4+6+8) means the 4th sample should be included.
        // The 3rd gets pulled in also because it's a sync frame and the 4th isn't.
        let mp4 = make_mp4_from_encoders(Type::Normal, &db, vec![r], 2+4+6 .. 2+4+6+8);
        let track = find_track(mp4, 1);

        // Examine edts. It should skip the 3rd frame.
        let mut cursor = track.edts_cursor.unwrap();
        cursor.down();
        cursor.find(b"elst");
        assert_eq!(cursor.get_all(), &[
            0x01, 0x00, 0x00, 0x00,                          // version + flags
            0x00, 0x00, 0x00, 0x01,                          // length
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08,  // segment_duration
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06,  // media_time
            0x00, 0x01, 0x00, 0x00,                          // media_rate_{integer,fraction}
        ]);

        // Examine stbl.
        let mut cursor = track.stbl_cursor;
        cursor.down();
        cursor.find(b"stts");
        assert_eq!(cursor.get_all(), &[
            0x00, 0x00, 0x00, 0x00,  // version + flags
            0x00, 0x00, 0x00, 0x02,  // entry_count

            // entries
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06,  // run length / timestamps.
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x08,
        ]);

        cursor.find(b"stsz");
        assert_eq!(cursor.get_all(), &[
            0x00, 0x00, 0x00, 0x00,  // version + flags
            0x00, 0x00, 0x00, 0x00,  // sample_size
            0x00, 0x00, 0x00, 0x02,  // sample_count

            // entries
            0x00, 0x00, 0x00, 0x09,  // size
            0x00, 0x00, 0x00, 0x0c,
        ]);

        cursor.find(b"stss");
        assert_eq!(cursor.get_all(), &[
            0x00, 0x00, 0x00, 0x00,  // version + flags
            0x00, 0x00, 0x00, 0x01,  // entry_count

            // entries
            0x00, 0x00, 0x00, 0x01,  // sample_number
        ]);
    }

    #[test]
    fn test_multi_segment() {
        testutil::init();
        let db = TestDb::new();
        let mut encoders = Vec::new();
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::new();
        encoder.add_sample(1, 1, true, &mut r);
        encoder.add_sample(2, 2, false, &mut r);
        encoder.add_sample(3, 3, true, &mut r);
        encoders.push(r);
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::new();
        encoder.add_sample(4, 4, true, &mut r);
        encoder.add_sample(5, 5, false, &mut r);
        encoders.push(r);

        // This should include samples 3 and 4 only, both sync frames.
        let mp4 = make_mp4_from_encoders(Type::Normal, &db, encoders, 1+2 .. 1+2+3+4);
        let mut cursor = BoxCursor::new(mp4);
        cursor.down();
        assert!(cursor.find(b"moov"));
        cursor.down();
        assert!(cursor.find(b"trak"));
        cursor.down();
        assert!(cursor.find(b"mdia"));
        cursor.down();
        assert!(cursor.find(b"minf"));
        cursor.down();
        assert!(cursor.find(b"stbl"));
        cursor.down();
        assert!(cursor.find(b"stss"));
        assert_eq!(cursor.get_u32(4), 2);  // entry_count
        assert_eq!(cursor.get_u32(8), 1);
        assert_eq!(cursor.get_u32(12), 2);
    }

    #[test]
    fn test_zero_duration_recording() {
        testutil::init();
        let db = TestDb::new();
        let mut encoders = Vec::new();
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::new();
        encoder.add_sample(2, 1, true, &mut r);
        encoder.add_sample(3, 2, false, &mut r);
        encoders.push(r);
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::new();
        encoder.add_sample(0, 3, true, &mut r);
        encoders.push(r);

        // Multi-segment recording with an edit list, encoding with a zero-duration recording.
        let mp4 = make_mp4_from_encoders(Type::Normal, &db, encoders, 1 .. 2+3);
        let track = find_track(mp4, 1);
        let mut cursor = track.edts_cursor.unwrap();
        cursor.down();
        cursor.find(b"elst");
        assert_eq!(cursor.get_u32(4), 1);   // entry_count
        assert_eq!(cursor.get_u64(8), 4);   // segment_duration
        assert_eq!(cursor.get_u64(16), 1);  // media_time
    }

    #[test]
    fn test_media_segment() {
        testutil::init();
        let db = TestDb::new();
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::new();
        for i in 1..6 {
            let duration_90k = 2 * i;
            let bytes = 3 * i;
            encoder.add_sample(duration_90k, bytes, (i % 2) == 1, &mut r);
        }

        // Time range [2+4+6, 2+4+6+8+1) means the 4th sample and part of the 5th are included.
        // The 3rd gets pulled in also because it's a sync frame and the 4th isn't.
        let mp4 = make_mp4_from_encoders(Type::MediaSegment, &db, vec![r],
                                         2+4+6 .. 2+4+6+8+1);
        let mut cursor = BoxCursor::new(mp4);
        cursor.down();

        let mut mdat = cursor.clone();
        assert!(mdat.find(b"mdat"));

        assert!(cursor.find(b"moof"));
        cursor.down();
        assert!(cursor.find(b"traf"));
        cursor.down();
        assert!(cursor.find(b"trun"));
        assert_eq!(cursor.get_u32(4), 2);
        assert_eq!(cursor.get_u32(8) as u64, mdat.interior().start);
        assert_eq!(cursor.get_u32(12), 174063616);  // first_sample_flags
        assert_eq!(cursor.get_u32(16), 6);   // sample duration
        assert_eq!(cursor.get_u32(20), 9);   // sample size
        assert_eq!(cursor.get_u32(24), 8);   // sample duration
        assert_eq!(cursor.get_u32(28), 12);  // sample size
        assert!(cursor.next());
        assert_eq!(cursor.name(), "trun");
        assert_eq!(cursor.get_u32(4), 1);
        assert_eq!(cursor.get_u32(8) as u64, mdat.interior().start + 9 + 12);
        assert_eq!(cursor.get_u32(12), 174063616);  // first_sample_flags
        assert_eq!(cursor.get_u32(16), 1);    // sample duration
        assert_eq!(cursor.get_u32(20), 15);   // sample size
    }

    #[test]
    fn test_round_trip() {
        testutil::init();
        let db = TestDb::new();
        copy_mp4_to_db(&db);
        let mp4 = create_mp4_from_db(&db, 0, 0, false);
        let new_filename = write_mp4(&mp4, db.tmpdir.path());
        compare_mp4s(&new_filename, 0, 0);

        // Test the metadata. This is brittle, which is the point. Any time the digest comparison
        // here fails, it can be updated, but the etag must change as well! Otherwise clients may
        // combine ranges from the new format with ranges from the old format.
        let sha1 = digest(&mp4);
        assert_eq!("1e5331e8371bd97ac3158b3a86494abc87cdc70e", strutil::hex(&sha1[..]));
        const EXPECTED_ETAG: &'static str = "04298efb2df0cc45a6cea65dfdf2e817a3b42ca8";
        assert_eq!(Some(header::EntityTag::strong(EXPECTED_ETAG.to_owned())), mp4.etag());
        drop(db.syncer_channel);
        db.db.lock().clear_on_flush();
        db.syncer_join.join().unwrap();
    }

    #[test]
    fn test_round_trip_with_subtitles() {
        testutil::init();
        let db = TestDb::new();
        copy_mp4_to_db(&db);
        let mp4 = create_mp4_from_db(&db, 0, 0, true);
        let new_filename = write_mp4(&mp4, db.tmpdir.path());
        compare_mp4s(&new_filename, 0, 0);

        // Test the metadata. This is brittle, which is the point. Any time the digest comparison
        // here fails, it can be updated, but the etag must change as well! Otherwise clients may
        // combine ranges from the new format with ranges from the old format.
        let sha1 = digest(&mp4);
        assert_eq!("de382684a471f178e4e3a163762711b0653bfd83", strutil::hex(&sha1[..]));
        const EXPECTED_ETAG: &'static str = "16a4f6348560c3de0d149675dccba21ef7906be3";
        assert_eq!(Some(header::EntityTag::strong(EXPECTED_ETAG.to_owned())), mp4.etag());
        drop(db.syncer_channel);
        db.db.lock().clear_on_flush();
        db.syncer_join.join().unwrap();
    }

    #[test]
    fn test_round_trip_with_edit_list() {
        testutil::init();
        let db = TestDb::new();
        copy_mp4_to_db(&db);
        let mp4 = create_mp4_from_db(&db, 1, 0, false);
        let new_filename = write_mp4(&mp4, db.tmpdir.path());
        compare_mp4s(&new_filename, 1, 0);

        // Test the metadata. This is brittle, which is the point. Any time the digest comparison
        // here fails, it can be updated, but the etag must change as well! Otherwise clients may
        // combine ranges from the new format with ranges from the old format.
        let sha1 = digest(&mp4);
        assert_eq!("d655945f94e18e6ed88a2322d27522aff6f76403", strutil::hex(&sha1[..]));
        const EXPECTED_ETAG: &'static str = "80e418b029e81aa195f90aa6b806015a5030e5be";
        assert_eq!(Some(header::EntityTag::strong(EXPECTED_ETAG.to_owned())), mp4.etag());
        drop(db.syncer_channel);
        db.db.lock().clear_on_flush();
        db.syncer_join.join().unwrap();
    }

    #[test]
    fn test_round_trip_with_shorten() {
        testutil::init();
        let db = TestDb::new();
        copy_mp4_to_db(&db);
        let mp4 = create_mp4_from_db(&db, 0, 1, false);
        let new_filename = write_mp4(&mp4, db.tmpdir.path());
        compare_mp4s(&new_filename, 0, 1);

        // Test the metadata. This is brittle, which is the point. Any time the digest comparison
        // here fails, it can be updated, but the etag must change as well! Otherwise clients may
        // combine ranges from the new format with ranges from the old format.
        let sha1 = digest(&mp4);
        assert_eq!("e0d28ddf08e24575a82657b1ce0b2da73f32fd88", strutil::hex(&sha1[..]));
        const EXPECTED_ETAG: &'static str = "5bfea0f20108a7c5b77ef1e21d82ef2abc29540f";
        assert_eq!(Some(header::EntityTag::strong(EXPECTED_ETAG.to_owned())), mp4.etag());
        drop(db.syncer_channel);
        db.db.lock().clear_on_flush();
        db.syncer_join.join().unwrap();
    }
}

#[cfg(all(test, feature="nightly"))]
mod bench {
    extern crate reqwest;
    extern crate test;

    use db::recording;
    use db::testutil::{self, TestDb};
    use futures::Stream;
    use futures::future;
    use hyper;
    use http_serve;
    use reffers::ARefs;
    use self::test::Bencher;
    use super::tests::create_mp4_from_db;
    use url::Url;

    /// An HTTP server for benchmarking.
    /// It's used as a singleton via `lazy_static!` so that when getting a CPU profile of the
    /// benchmark, more of the profile focuses on the HTTP serving rather than the setup.
    ///
    /// Currently this only serves a single `.mp4` file but we could set up variations to benchmark
    /// different scenarios: with/without subtitles and edit lists, different lengths, serving
    /// different fractions of the file, etc.
    struct BenchServer {
        url: Url,
        generated_len: u64,
    }

    impl BenchServer {
        fn new() -> BenchServer {
            let db = TestDb::new();
            testutil::add_dummy_recordings_to_db(&db.db, 60);
            let mp4 = create_mp4_from_db(&db, 0, 0, false);
            let p = mp4.0.initial_sample_byte_pos;
            let (tx, rx) = ::std::sync::mpsc::channel();
            ::std::thread::spawn(move || {
                let addr = "127.0.0.1:0".parse().unwrap();
                let server = hyper::server::Http::new()
                    .bind(&addr, move || Ok(MyService(mp4.clone())))
                    .unwrap();
                tx.send(server.local_addr().unwrap()).unwrap();
                server.run().unwrap();
            });
            let addr = rx.recv().unwrap();
            BenchServer{
                url: Url::parse(&format!("http://{}:{}/", addr.ip(), addr.port())).unwrap(),
                generated_len: p,
            }
        }
    }

    struct MyService(super::File);

    impl hyper::server::Service for MyService {
        type Request = hyper::server::Request;
        type Response = hyper::server::Response<
            Box<Stream<Item = ARefs<'static, [u8]>, Error = hyper::Error> + Send>>;
        type Error = hyper::Error;
        type Future = future::FutureResult<Self::Response, Self::Error>;

        fn call(&self, req: hyper::server::Request) -> Self::Future {
            future::ok(http_serve::serve(self.0.clone(), &req))
        }
    }

    lazy_static! {
        static ref SERVER: BenchServer = { BenchServer::new() };
    }

    #[bench]
    fn build_index(b: &mut Bencher) {
        testutil::init();
        let db = TestDb::new();
        testutil::add_dummy_recordings_to_db(&db.db, 1);

        let db = db.db.lock();
        let segment = {
            let all_time = recording::Time(i64::min_value()) .. recording::Time(i64::max_value());
            let mut row = None;
            db.list_recordings_by_time(testutil::TEST_STREAM_ID, all_time, &mut |r| {
                row = Some(r);
                Ok(())
            }).unwrap();
            let row = row.unwrap();
            let rel_range_90k = 0 .. row.duration_90k;
            super::Segment::new(&db, &row, rel_range_90k, 1).unwrap()
        };
        db.with_recording_playback(segment.s.id, |playback| {
            let v = segment.build_index(playback).unwrap();  // warm.
            b.bytes = v.len() as u64;  // define the benchmark performance in terms of output bytes.
            b.iter(|| segment.build_index(playback).unwrap());
            Ok(())
        }).unwrap();
    }

    /// Benchmarks serving the generated part of a `.mp4` file (up to the first byte from disk).
    #[bench]
    fn serve_generated_bytes(b: &mut Bencher) {
        testutil::init();
        let server = &*SERVER;
        let p = server.generated_len;
        let mut buf = Vec::with_capacity(p as usize);
        b.bytes = p;
        let client = reqwest::Client::new();
        let mut run = || {
            use self::reqwest::header::{Range, ByteRangeSpec};
            let mut resp =
                client.get(server.url.clone())
                      .header(Range::Bytes(vec![ByteRangeSpec::FromTo(0, p - 1)]))
                      .send()
                      .unwrap();
            buf.clear();
            use std::io::Read;
            let size = resp.read_to_end(&mut buf).unwrap();
            assert_eq!(p, size as u64);
        };
        run();  // warm.
        b.iter(run);
    }

    #[bench]
    fn mp4_construction(b: &mut Bencher) {
        testutil::init();
        let db = TestDb::new();
        testutil::add_dummy_recordings_to_db(&db.db, 60);
        b.iter(|| {
            create_mp4_from_db(&db, 0, 0, false);
        });
    }
}
