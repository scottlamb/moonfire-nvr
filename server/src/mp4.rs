// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

//! `.mp4` virtual file serving.
//!
//! The `mp4` module builds virtual files representing ISO/IEC 14496-12 (ISO base media format /
//! MPEG-4 / `.mp4`) video. See [the wiki page on standards and specifications](https://github.com/scottlamb/moonfire-nvr/wiki/Standards-and-specifications)
//! to find a copy of the relevant standards. This code won't make much sense without them!
//!
//! The virtual files can be constructed from one or more recordings and are suitable
//! for HTTP range serving or download. The generated `.mp4` file has the `moov` box before the
//! `mdat` box for fast start. More specifically, boxes are arranged in the order suggested by
//! ISO/IEC 14496-12 section 6.2.3 (Table 1):
//!
//! ```text
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

use crate::body::{wrap_error, BoxedError, Chunk};
use crate::slices::{self, Slices};
use base::{bail, err, Error, ErrorKind, ResultExt};
use byteorder::{BigEndian, ByteOrder, WriteBytesExt};
use bytes::BytesMut;
use db::dir;
use db::recording::{self, rescale, TIME_UNITS_PER_SEC};
use futures::Stream;
use http::header::HeaderValue;
use hyper::body::Buf;
use pin_project::pin_project;
use reffers::ARefss;
use smallvec::SmallVec;
use std::cmp;
use std::convert::TryFrom;
use std::fmt;
use std::io;
use std::mem;
use std::ops::Range;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;
use tracing::{debug, error, trace, warn};

/// This value should be incremented any time a change is made to this file that causes different
/// bytes or headers to be output for a particular set of `FileBuilder` options. Incrementing this
/// value will cause the etag to change as well.
const FORMAT_VERSION: [u8; 1] = [0x0a];

/// An `ftyp` (ISO/IEC 14496-12 section 4.3 `FileType`) box.
const NORMAL_FTYP_BOX: &[u8] = &[
    0x00, 0x00, 0x00, 0x20, // length = 32, sizeof(NORMAL_FTYP_BOX)
    b'f', b't', b'y', b'p', // type
    b'i', b's', b'o', b'm', // major_brand
    0x00, 0x00, 0x00, 0x00, // minor_version
    b'i', b's', b'o', b'm', // compatible_brands[0]
    b'i', b's', b'o', b'2', // compatible_brands[1]
    b'a', b'v', b'c', b'1', // compatible_brands[2]
    b'm', b'p', b'4', b'1', // compatible_brands[3]
];

/// An `ftyp` (ISO/IEC 14496-12 section 4.3 `FileType`) box for an initialization segment.
/// More restrictive brands because of the default-base-is-moof flag.
/// Eg ISO/IEC 14496-12 section A.2 says "NOTE The default‐base‐is‐moof flag
/// (8.8.7.1) cannot be set where a file is marked with [the avc1 brand]."
/// Note that Safari insists there be a compatible brand set in this list. The
/// major brand is not enough.
const INIT_SEGMENT_FTYP_BOX: &[u8] = &[
    0x00, 0x00, 0x00, 0x14, // length = 20, sizeof(INIT_SEGMENT_FTYP_BOX)
    b'f', b't', b'y', b'p', // type
    b'i', b's', b'o', b'5', // major_brand
    0x00, 0x00, 0x00, 0x00, // minor_version
    b'i', b's', b'o', b'5', // compatible_brands[0]
];

/// An `hdlr` (ISO/IEC 14496-12 section 8.4.3 `HandlerBox`) box suitable for video.
const VIDEO_HDLR_BOX: &[u8] = &[
    0x00, 0x00, 0x00, 0x21, // length == sizeof(kHdlrBox)
    b'h', b'd', b'l', b'r', // type == hdlr, ISO/IEC 14496-12 section 8.4.3.
    0x00, 0x00, 0x00, 0x00, // version + flags
    0x00, 0x00, 0x00, 0x00, // pre_defined
    b'v', b'i', b'd', b'e', // handler = vide
    0x00, 0x00, 0x00, 0x00, // reserved[0]
    0x00, 0x00, 0x00, 0x00, // reserved[1]
    0x00, 0x00, 0x00, 0x00, // reserved[2]
    0x00, // name, zero-terminated (empty)
];

/// An `hdlr` (ISO/IEC 14496-12 section 8.4.3 `HandlerBox`) box suitable for subtitles.
const SUBTITLE_HDLR_BOX: &[u8] = &[
    0x00, 0x00, 0x00, 0x21, // length == sizeof(kHdlrBox)
    b'h', b'd', b'l', b'r', // type == hdlr, ISO/IEC 14496-12 section 8.4.3.
    0x00, 0x00, 0x00, 0x00, // version + flags
    0x00, 0x00, 0x00, 0x00, // pre_defined
    b's', b'b', b't', b'l', // handler = sbtl
    0x00, 0x00, 0x00, 0x00, // reserved[0]
    0x00, 0x00, 0x00, 0x00, // reserved[1]
    0x00, 0x00, 0x00, 0x00, // reserved[2]
    0x00, // name, zero-terminated (empty)
];

/// Part of an `mvhd` (`MovieHeaderBox` version 0, ISO/IEC 14496-12 section 8.2.2), used from
/// `append_mvhd`.
const MVHD_JUNK: &[u8] = &[
    0x00, 0x01, 0x00, 0x00, // rate
    0x01, 0x00, // volume
    0x00, 0x00, // reserved
    0x00, 0x00, 0x00, 0x00, // reserved
    0x00, 0x00, 0x00, 0x00, // reserved
    0x00, 0x01, 0x00, 0x00, // matrix[0]
    0x00, 0x00, 0x00, 0x00, // matrix[1]
    0x00, 0x00, 0x00, 0x00, // matrix[2]
    0x00, 0x00, 0x00, 0x00, // matrix[3]
    0x00, 0x01, 0x00, 0x00, // matrix[4]
    0x00, 0x00, 0x00, 0x00, // matrix[5]
    0x00, 0x00, 0x00, 0x00, // matrix[6]
    0x00, 0x00, 0x00, 0x00, // matrix[7]
    0x40, 0x00, 0x00, 0x00, // matrix[8]
    0x00, 0x00, 0x00, 0x00, // pre_defined[0]
    0x00, 0x00, 0x00, 0x00, // pre_defined[1]
    0x00, 0x00, 0x00, 0x00, // pre_defined[2]
    0x00, 0x00, 0x00, 0x00, // pre_defined[3]
    0x00, 0x00, 0x00, 0x00, // pre_defined[4]
    0x00, 0x00, 0x00, 0x00, // pre_defined[5]
];

/// Part of a `tkhd` (`TrackHeaderBox` version 0, ISO/IEC 14496-12 section 8.3.2), used from
/// `append_video_tkhd` and `append_subtitle_tkhd`.
const TKHD_JUNK: &[u8] = &[
    0x00, 0x00, 0x00, 0x00, // reserved
    0x00, 0x00, 0x00, 0x00, // reserved
    0x00, 0x00, 0x00, 0x00, // layer + alternate_group
    0x00, 0x00, 0x00, 0x00, // volume + reserved
    0x00, 0x01, 0x00, 0x00, // matrix[0]
    0x00, 0x00, 0x00, 0x00, // matrix[1]
    0x00, 0x00, 0x00, 0x00, // matrix[2]
    0x00, 0x00, 0x00, 0x00, // matrix[3]
    0x00, 0x01, 0x00, 0x00, // matrix[4]
    0x00, 0x00, 0x00, 0x00, // matrix[5]
    0x00, 0x00, 0x00, 0x00, // matrix[6]
    0x00, 0x00, 0x00, 0x00, // matrix[7]
    0x40, 0x00, 0x00, 0x00, // matrix[8]
];

/// Part of a `minf` (`MediaInformationBox`, ISO/IEC 14496-12 section 8.4.4), used from
/// `append_video_minf`.
const VIDEO_MINF_JUNK: &[u8] = &[
    b'm', b'i', b'n', b'f', // type = minf, ISO/IEC 14496-12 section 8.4.4.
    // A vmhd box; the "graphicsmode" and "opcolor" values don't have any
    // meaningful use.
    0x00, 0x00, 0x00, 0x14, // length == sizeof(kVmhdBox)
    b'v', b'm', b'h', b'd', // type = vmhd, ISO/IEC 14496-12 section 12.1.2.
    0x00, 0x00, 0x00, 0x01, // version + flags(1)
    0x00, 0x00, 0x00, 0x00, // graphicsmode (copy), opcolor[0]
    0x00, 0x00, 0x00, 0x00, // opcolor[1], opcolor[2]
    // A dinf box suitable for a "self-contained" .mp4 file (no URL/URN
    // references to external data).
    0x00, 0x00, 0x00, 0x24, // length == sizeof(kDinfBox)
    b'd', b'i', b'n', b'f', // type = dinf, ISO/IEC 14496-12 section 8.7.1.
    0x00, 0x00, 0x00, 0x1c, // length
    b'd', b'r', b'e', b'f', // type = dref, ISO/IEC 14496-12 section 8.7.2.
    0x00, 0x00, 0x00, 0x00, // version and flags
    0x00, 0x00, 0x00, 0x01, // entry_count
    0x00, 0x00, 0x00, 0x0c, // length
    b'u', b'r', b'l', b' ', // type = url, ISO/IEC 14496-12 section 8.7.2.
    0x00, 0x00, 0x00, 0x01, // version=0, flags=self-contained
];

/// Part of a `minf` (`MediaInformationBox`, ISO/IEC 14496-12 section 8.4.4), used from
/// `append_subtitle_minf`.
const SUBTITLE_MINF_JUNK: &[u8] = &[
    b'm', b'i', b'n', b'f', // type = minf, ISO/IEC 14496-12 section 8.4.4.
    // A nmhd box.
    0x00, 0x00, 0x00, 0x0c, // length == sizeof(kNmhdBox)
    b'n', b'm', b'h', b'd', // type = nmhd, ISO/IEC 14496-12 section 12.1.2.
    0x00, 0x00, 0x00, 0x01, // version + flags(1)
    // A dinf box suitable for a "self-contained" .mp4 file (no URL/URN
    // references to external data).
    0x00, 0x00, 0x00, 0x24, // length == sizeof(kDinfBox)
    b'd', b'i', b'n', b'f', // type = dinf, ISO/IEC 14496-12 section 8.7.1.
    0x00, 0x00, 0x00, 0x1c, // length
    b'd', b'r', b'e', b'f', // type = dref, ISO/IEC 14496-12 section 8.7.2.
    0x00, 0x00, 0x00, 0x00, // version and flags
    0x00, 0x00, 0x00, 0x01, // entry_count
    0x00, 0x00, 0x00, 0x0c, // length
    b'u', b'r', b'l', b' ', // type = url, ISO/IEC 14496-12 section 8.7.2.
    0x00, 0x00, 0x00, 0x01, // version=0, flags=self-contained
];

/// Part of a `stbl` (`SampleTableBox`, ISO/IEC 14496 section 8.5.1) used from
/// `append_subtitle_stbl`.
#[rustfmt::skip]
const SUBTITLE_STBL_JUNK: &[u8] = &[
    b's', b't', b'b', b'l', // type = stbl, ISO/IEC 14496-12 section 8.5.1.
    // A stsd box.
    0x00, 0x00, 0x00, 0x54, // length
    b's', b't', b's', b'd', // type == stsd, ISO/IEC 14496-12 section 8.5.2.
    0x00, 0x00, 0x00, 0x00, // version + flags
    0x00, 0x00, 0x00, 0x01, // entry_count == 1
    // SampleEntry, ISO/IEC 14496-12 section 8.5.2.2.
    0x00, 0x00, 0x00, 0x44, // length
    b't', b'x', b'3', b'g', // type == tx3g, 3GPP TS 26.245 section 5.16.
    0x00, 0x00, 0x00, 0x00, // reserved
    0x00, 0x00, 0x00, 0x01, // reserved, data_reference_index == 1
    // TextSampleEntry
    0x00, 0x00, 0x00, 0x00, // displayFlags == none
    0x00, // horizontal-justification == left
    0x00, // vertical-justification == top
    0x00, 0x00, 0x00, 0x00, // background-color-rgba == transparent
    // TextSampleEntry.BoxRecord
    0x00, 0x00, // top
    0x00, 0x00, // left
    0x00, 0x00, // bottom
    0x00, 0x00, // right
    // TextSampleEntry.StyleRecord
    0x00, 0x00, // startChar
    0x00, 0x00, // endChar
    0x00, 0x01, // font-ID
    0x00,       // face-style-flags
    0x12,       // font-size == 18 px
    0xff, 0xff, 0xff, 0xff, // text-color-rgba == opaque white
    // TextSampleEntry.FontTableBox
    0x00, 0x00, 0x00, 0x16, // length
    b'f', b't', b'a', b'b', // type == ftab, section 5.16
    0x00, 0x01,             // entry-count == 1
    0x00, 0x01,             // font-ID == 1
    0x09,                   // font-name-length == 9
    b'M', b'o', b'n', b'o', b's', b'p', b'a', b'c', b'e',
];

/// Pointers to each static bytestrings.
/// The order here must match the `StaticBytestring` enum.
const STATIC_BYTESTRINGS: [&[u8]; 9] = [
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
const SUBTITLE_TEMPLATE: &str = "%Y-%m-%d %H:%M:%S %z";

/// The length of the output of `SUBTITLE_TEMPLATE`.
const SUBTITLE_LENGTH: usize = 25; // "2015-07-02 17:10:00 -0700".len();

/// The lengths of the indexes associated with a `Segment`; for use within `Segment` only.
struct SegmentLengths {
    stts: usize,
    stsz: usize,
    stss: usize,
}

/// A wrapper around `recording::Segment` that keeps some additional `.mp4`-specific state.
struct Segment {
    /// The underlying segment (a portion of a recording).
    s: recording::Segment,

    /// The absolute timestamp of the recording's start time.
    recording_start: recording::Time,

    recording_wall_duration_90k: i32,
    recording_media_duration_90k: i32,

    /// The _desired_, _relative_, _media_ time range covered by this recording.
    /// *   _desired_: as noted in `recording::Segment`, the _actual_ time range may be somewhat
    ///     more if there's no key frame at the desired start.
    /// *   _relative_: relative to `recording_start` rather than absolute timestamps.
    /// *   _media_ time: as described in design/glossary.md and design/time.md.
    rel_media_range_90k: Range<i32>,

    /// If generated, the `.mp4`-format sample indexes, accessed only through `index`:
    ///    1. stts: `slice[.. stsz_start]`
    ///    2. stsz: `slice[stsz_start .. stss_start]`
    ///    3. stss: `slice[stss_start ..]`
    index: OnceLock<Result<Box<[u8]>, ()>>,

    /// The 1-indexed frame number in the `File` of the first frame in this segment.
    first_frame_num: u32,
    num_subtitle_samples: u16,
}

// Manually implement `Debug` skipping the obnoxiously-long `index` field.
impl fmt::Debug for Segment {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("mp4::Segment")
            .field("s", &self.s)
            .field("recording_start", &self.recording_start)
            .field(
                "recording_wall_duration_90k",
                &self.recording_wall_duration_90k,
            )
            .field(
                "recording_media_duration_90k",
                &self.recording_media_duration_90k,
            )
            .field("rel_media_range_90k", &self.rel_media_range_90k)
            .field("first_frame_num", &self.first_frame_num)
            .field("num_subtitle_samples", &self.num_subtitle_samples)
            .finish()
    }
}

impl Segment {
    fn new(
        db: &db::LockedDatabase,
        row: &db::ListRecordingsRow,
        rel_media_range_90k: Range<i32>,
        first_frame_num: u32,
        start_at_key: bool,
    ) -> Result<Self, Error> {
        Ok(Segment {
            s: recording::Segment::new(db, row, rel_media_range_90k.clone(), start_at_key)
                .err_kind(ErrorKind::Unknown)?,
            recording_start: row.start,
            recording_wall_duration_90k: row.wall_duration_90k,
            recording_media_duration_90k: row.media_duration_90k,
            rel_media_range_90k,
            index: OnceLock::new(),
            first_frame_num,
            num_subtitle_samples: 0,
        })
    }

    fn wall(&self, rel_media_90k: i32) -> i32 {
        rescale(
            rel_media_90k,
            self.recording_media_duration_90k,
            self.recording_wall_duration_90k,
        )
    }

    fn media(&self, rel_wall_90k: i32) -> i32 {
        rescale(
            rel_wall_90k,
            self.recording_wall_duration_90k,
            self.recording_media_duration_90k,
        )
    }

    fn index<'a>(&'a self, db: &db::Database) -> Result<&'a [u8], Error> {
        self.index
            .get_or_init(|| {
                db
                .lock()
                .with_recording_playback(self.s.id, &mut |playback| self.build_index(playback))
                .map_err(|err| {
                    error!(err = %err.chain(), recording_id = %self.s.id, "unable to build index for segment");
                })
            })
            .as_deref()
            .map_err(|()| err!(Unknown, msg("unable to build index; see logs")))
    }

    fn lens(&self) -> SegmentLengths {
        SegmentLengths {
            stts: mem::size_of::<u32>() * 2 * (self.s.frames as usize),
            stsz: mem::size_of::<u32>() * self.s.frames as usize,
            stss: mem::size_of::<u32>() * self.s.key_frames as usize,
        }
    }

    fn stts(buf: &[u8], lens: SegmentLengths) -> &[u8] {
        &buf[..lens.stts]
    }
    fn stsz(buf: &[u8], lens: SegmentLengths) -> &[u8] {
        &buf[lens.stts..lens.stts + lens.stsz]
    }
    fn stss(buf: &[u8], lens: SegmentLengths) -> &[u8] {
        &buf[lens.stts + lens.stsz..]
    }

    fn build_index(&self, playback: &db::RecordingPlayback) -> Result<Box<[u8]>, Error> {
        let s = &self.s;
        let lens = self.lens();
        let len = lens.stts + lens.stsz + lens.stss;

        // This was a few percent faster when we didn't pre-initialize the
        // slice (as in the commented-out code below), but it was unsound. See
        // <https://github.com/scottlamb/moonfire-nvr/issues/185>. It might be
        // nice to use `MaybeUninit::write_slice` here when it stabilizes,
        // more ergonomic than dealing with raw pointers. In the meantime, a few
        // percent difference in speed here probably isn't the biggest unsolved
        // problem with Moonfire...
        let mut buf = vec![0; len].into_boxed_slice();
        // let mut buf = {
        //     let mut v = Vec::with_capacity(len);
        //     unsafe { v.set_len(len) };
        //     v.into_boxed_slice()
        // };

        {
            let (stts, rest) = buf.split_at_mut(lens.stts);
            let (stsz, stss) = rest.split_at_mut(lens.stsz);
            let mut frame = 0;
            let mut key_frame = 0;
            let mut last_start_and_dur = None;
            s.foreach(playback, |it| {
                last_start_and_dur = Some((it.start_90k, it.duration_90k));
                BigEndian::write_u32(&mut stts[8 * frame..8 * frame + 4], 1);
                BigEndian::write_u32(
                    &mut stts[8 * frame + 4..8 * frame + 8],
                    it.duration_90k as u32,
                );
                BigEndian::write_u32(&mut stsz[4 * frame..4 * frame + 4], it.bytes as u32);
                if it.is_key() {
                    BigEndian::write_u32(
                        &mut stss[4 * key_frame..4 * key_frame + 4],
                        self.first_frame_num + (frame as u32),
                    );
                    key_frame += 1;
                }
                frame += 1;
                Ok(())
            })?;

            // Fix up the final frame's duration.
            // Doing this after the fact is more efficient than having a condition on every
            // iteration.
            if let Some((last_start, dur)) = last_start_and_dur {
                let min = cmp::min(self.rel_media_range_90k.end - last_start, dur);
                BigEndian::write_u32(&mut stts[8 * frame - 4..], u32::try_from(min).unwrap());
            }
        }

        Ok(buf)
    }

    fn truns_len(&self) -> usize {
        self.s.key_frames as usize * (mem::size_of::<u32>() * 6)
            + self.s.frames as usize * (mem::size_of::<u32>() * 2)
            + if self.s.starts_with_nonkey() {
                mem::size_of::<u32>() * 5
            } else {
                0
            }
    }

    // TrackRunBox / trun (8.8.8).
    fn truns(
        &self,
        playback: &db::RecordingPlayback,
        initial_pos: u64,
        len: usize,
    ) -> Result<Vec<u8>, Error> {
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
        self.s
            .foreach(playback, |it| {
                let is_key = it.is_key();
                if is_key {
                    if let Some(r) = run_info.take() {
                        // Finish a non-terminal run.
                        let p = v.len();
                        BigEndian::write_u32(
                            &mut v[r.box_len_pos..r.box_len_pos + 4],
                            (p - r.box_len_pos) as u32,
                        );
                        BigEndian::write_u32(
                            &mut v[r.sample_count_pos..r.sample_count_pos + 4],
                            r.count,
                        );
                    }
                }
                let mut r = match run_info.take() {
                    None => {
                        let box_len_pos = v.len();
                        v.extend_from_slice(&[
                            0x00,
                            0x00,
                            0x00,
                            0x00, // placeholder for size
                            b't',
                            b'r',
                            b'u',
                            b'n',
                            // version 0, tr_flags:
                            // 0x000001 data-offset-present
                            // 0x000004 first-sample-flags-present
                            // 0x000100 sample-duration-present
                            // 0x000200 sample-size-present
                            0x00,
                            0x00,
                            0x03,
                            0x01 | if is_key { 0x04 } else { 0 },
                        ]);
                        let sample_count_pos = v.len();
                        v.write_u32::<BigEndian>(0)?; // placeholder for sample count
                        v.write_u32::<BigEndian>(data_pos as u32)?;

                        if is_key {
                            // first_sample_flags. See trex (8.8.3.1).
                            #[allow(clippy::identity_op)]
                            v.write_u32::<BigEndian>(
                                // As defined by the Independent and Disposable Samples Box
                                // (sdp, 8.6.4).
                                (2 << 26) | // is_leading: this sample is not a leading sample
                            (2 << 24) | // sample_depends_on: this sample does not depend on others
                            (1 << 22) | // sample_is_depend_on: others may depend on this one
                            (2 << 20) | // sample_has_redundancy: no redundant coding
                            // As defined by the sample padding bits (padb, 8.7.6).
                            (0 << 17) | // no padding
                            (0 << 16), // sample_is_non_sync_sample=0
                            )?; // TODO: sample_degradation_priority
                        }
                        RunInfo {
                            box_len_pos,
                            sample_count_pos,
                            count: 0,
                            last_start: 0,
                            last_dur: 0,
                        }
                    }
                    Some(r) => r,
                };
                r.count += 1;
                r.last_start = it.start_90k;
                r.last_dur = it.duration_90k;
                v.write_u32::<BigEndian>(it.duration_90k as u32)?;
                v.write_u32::<BigEndian>(it.bytes as u32)?;
                data_pos += it.bytes as u64;
                run_info = Some(r);
                Ok(())
            })
            .err_kind(ErrorKind::Internal)?;
        if let Some(r) = run_info.take() {
            // Finish the run as in the non-terminal case above.
            let p = v.len();
            BigEndian::write_u32(
                &mut v[r.box_len_pos..r.box_len_pos + 4],
                (p - r.box_len_pos) as u32,
            );
            BigEndian::write_u32(&mut v[r.sample_count_pos..r.sample_count_pos + 4], r.count);

            // One more thing to do in the terminal case: fix up the final frame's duration.
            // Doing this after the fact is more efficient than having a condition on every
            // iteration.
            BigEndian::write_u32(
                &mut v[p - 8..p - 4],
                u32::try_from(cmp::min(
                    self.rel_media_range_90k.end - r.last_start,
                    r.last_dur,
                ))
                .unwrap(),
            );
        }
        if len != v.len() {
            bail!(
                Internal,
                msg(
                    "truns on {:?} expected len {} got len {}",
                    self,
                    len,
                    v.len(),
                ),
            );
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

    /// The total media time, after applying edit lists (if applicable) to skip unwanted portions.
    media_duration_90k: u64,
    num_subtitle_samples: u32,
    subtitle_co64_pos: Option<usize>,
    body: BodyState,
    type_: Type,
    prev_media_duration_and_cur_runs: Option<(recording::Duration, i32)>,
    include_timestamp_subtitle_track: bool,
    content_disposition: Option<HeaderValue>,
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
    Static = 0,             // param is index into STATIC_BYTESTRINGS
    Buf = 1,                // param is index into m.buf
    VideoSampleEntry = 2,   // param is index into m.video_sample_entries
    Stts = 3,               // param is index into m.segments
    Stsz = 4,               // param is index into m.segments
    Stss = 5,               // param is index into m.segments
    Co64 = 6,               // param is unused
    VideoSampleData = 7,    // param is index into m.segments
    SubtitleSampleData = 8, // param is index into m.segments
    Truns = 9,              // param is index into m.segments

                            // There must be no value > 15, as this is packed into 4 bits in Slice.
}

impl Slice {
    fn new(end: u64, t: SliceType, p: usize) -> Result<Self, Error> {
        if end >= (1 << 40) || p >= (1 << 20) {
            bail!(
                OutOfRange,
                msg("end={} p={} too large for {:?} Slice", end, p, t,),
            );
        }

        Ok(Slice(end | ((t as u64) << 40) | ((p as u64) << 44)))
    }

    fn t(&self) -> SliceType {
        // This value is guaranteed to be a valid SliceType because it was copied from a SliceType
        // in Slice::new.
        unsafe { ::std::mem::transmute(((self.0 >> 40) & 0xF) as u8) }
    }
    fn p(&self) -> usize {
        (self.0 >> 44) as usize
    }

    fn wrap_index<F>(&self, mp4: &File, r: Range<u64>, len: u64, f: &F) -> Result<Chunk, Error>
    where
        F: Fn(&[u8], SegmentLengths) -> &[u8],
    {
        let mp4 = ARefss::new(mp4.0.clone());
        let r = r.start as usize..r.end as usize;
        let p = self.p();
        Ok(mp4
            .try_map(|mp4| {
                let segment = &mp4.segments[p];
                let i = segment.index(&mp4.db)?;
                let i = f(i, segment.lens());
                if u64::try_from(i.len()).unwrap() != len {
                    bail!(Internal, msg("expected len {} got {}", len, i.len()));
                }
                Ok::<_, Error>(&i[r])
            })?
            .into())
    }

    fn wrap_truns(&self, mp4: &File, r: Range<u64>, len: usize) -> Result<Chunk, Error> {
        let s = &mp4.0.segments[self.p()];
        let mut pos = mp4.0.initial_sample_byte_pos;
        for ps in &mp4.0.segments[0..self.p()] {
            let r = ps.s.sample_file_range();
            pos += r.end - r.start;
        }
        let truns = mp4
            .0
            .db
            .lock()
            .with_recording_playback(s.s.id, &mut |playback| s.truns(playback, pos, len))
            .err_kind(ErrorKind::Unknown)?;
        let truns = ARefss::new(truns);
        Ok(truns.map(|t| &t[r.start as usize..r.end as usize]).into())
    }

    fn wrap_video_sample_entry(&self, f: &File, r: Range<u64>, len: u64) -> Result<Chunk, Error> {
        let mp4 = ARefss::new(f.0.clone());
        Ok(mp4
            .try_map(|mp4| {
                let data = &mp4.video_sample_entries[self.p()].data;
                if u64::try_from(data.len()).unwrap() != len {
                    bail!(Internal, msg("expected len {} got len {}", len, data.len()));
                }
                Ok::<_, Error>(&data[r.start as usize..r.end as usize])
            })?
            .into())
    }
}

#[pin_project(project = SliceStreamProj)]
enum SliceStream {
    Once(Option<Result<Chunk, Error>>),
    File(#[pin] db::dir::reader::FileStream),
}

impl futures::stream::Stream for SliceStream {
    type Item = Result<Chunk, BoxedError>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.project() {
            SliceStreamProj::Once(o) => {
                std::task::Poll::Ready(o.take().map(|r| r.map_err(wrap_error)))
            }
            SliceStreamProj::File(f) => f.poll_next(cx).map_ok(Chunk::from).map_err(wrap_error),
        }
    }
}

impl slices::Slice for Slice {
    type Ctx = File;
    type Chunk = Chunk;
    type Stream = SliceStream;

    fn end(&self) -> u64 {
        self.0 & 0xFF_FF_FF_FF_FF
    }
    fn get_range(&self, f: &File, range: Range<u64>, len: u64) -> SliceStream {
        trace!("getting mp4 slice {:?}'s range {:?} / {}", self, range, len);
        let p = self.p();
        let res = match self.t() {
            SliceType::Static => {
                let s = STATIC_BYTESTRINGS[p];
                if u64::try_from(s.len()).unwrap() != len {
                    Err(err!(
                        Internal,
                        msg("expected len {} got len {}", len, s.len())
                    ))
                } else {
                    let part = &s[range.start as usize..range.end as usize];
                    Ok(part.into())
                }
            }
            SliceType::Buf => {
                let r = ARefss::new(f.0.clone());
                Ok(
                    r.map(|f| &f.buf[p + range.start as usize..p + range.end as usize])
                        .into(),
                )
            }
            SliceType::VideoSampleEntry => self.wrap_video_sample_entry(f, range.clone(), len),
            SliceType::Stts => self.wrap_index(f, range.clone(), len, &Segment::stts),
            SliceType::Stsz => self.wrap_index(f, range.clone(), len, &Segment::stsz),
            SliceType::Stss => self.wrap_index(f, range.clone(), len, &Segment::stss),
            SliceType::Co64 => f.0.get_co64(range.clone(), len),
            SliceType::VideoSampleData => return f.0.get_video_sample_data(p, range),
            SliceType::SubtitleSampleData => f.0.get_subtitle_sample_data(p, range.clone(), len),
            SliceType::Truns => self.wrap_truns(f, range.clone(), len as usize),
        };
        SliceStream::Once(Some(res.and_then(move |c| {
            if c.remaining() != (range.end - range.start) as usize {
                bail!(
                    Internal,
                    msg(
                        "{:?} range {:?} produced incorrect len {}",
                        self,
                        range,
                        c.remaining()
                    )
                );
            }
            Ok(c)
        })))
    }

    fn get_slices(ctx: &File) -> &Slices<Self> {
        &ctx.0.slices
    }
}

impl fmt::Debug for Slice {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        // Write an unpacked representation. Omit end(); Slices writes that part.
        write!(f, "{:?} {}", self.t(), self.p())
    }
}

/// Converts from seconds since Unix epoch (1970-01-01 00:00:00 UTC) to seconds since
/// ISO-14496 epoch (1904-01-01 00:00:00 UTC).
fn to_iso14496_timestamp(unix_secs: i64) -> u32 {
    unix_secs as u32 + 24107 * 86400
}

/// Writes a box length for everything appended in the supplied scope.
/// Used only within FileBuilder::build (and methods it calls internally).
macro_rules! write_length {
    ($_self:ident, $b:block) => {{
        let len_pos = $_self.body.buf.len();
        let len_start = $_self.body.slices.len() + $_self.body.buf.len() as u64
            - $_self.body.unflushed_buf_pos as u64;
        $_self.body.append_u32(0); // placeholder.
        {
            $b;
        }
        let len_end = $_self.body.slices.len() + $_self.body.buf.len() as u64
            - $_self.body.unflushed_buf_pos as u64;
        BigEndian::write_u32(
            &mut $_self.body.buf[len_pos..len_pos + 4],
            (len_end - len_start) as u32,
        );
        Ok::<_, Error>(())
    }};
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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
            media_duration_90k: 0,
            num_subtitle_samples: 0,
            subtitle_co64_pos: None,
            body: BodyState {
                slices: Slices::new(),
                buf: Vec::new(),
                unflushed_buf_pos: 0,
            },
            type_,
            include_timestamp_subtitle_track: false,
            content_disposition: None,
            prev_media_duration_and_cur_runs: None,
        }
    }

    /// Sets if the generated `.mp4` should include a subtitle track with second-level timestamps.
    /// Default is false.
    pub fn include_timestamp_subtitle_track(&mut self, b: bool) -> Result<(), Error> {
        if b && self.type_ == Type::MediaSegment {
            // There's no support today for timestamp truns or for timestamps without edit lists.
            // The latter would invalidate the code's assumption that desired timespan == actual
            // timespan in the timestamp track.
            bail!(
                InvalidArgument,
                msg("timestamp subtitles aren't supported on media segments")
            );
        }
        self.include_timestamp_subtitle_track = b;
        Ok(())
    }

    /// Reserves space for the given number of additional segments.
    pub fn reserve(&mut self, additional: usize) {
        self.segments.reserve(additional);
    }

    pub fn append_video_sample_entry(&mut self, ent: Arc<db::VideoSampleEntry>) {
        self.video_sample_entries.push(ent);
    }

    /// Appends a segment for (a subset of) the given recording.
    /// `rel_media_range_90k` is the media time range within the recording.
    /// Eg `0 .. row.media_duration_90k` means the full recording.
    pub fn append(
        &mut self,
        db: &db::LockedDatabase,
        row: &db::ListRecordingsRow,
        rel_media_range_90k: Range<i32>,
        start_at_key: bool,
    ) -> Result<(), Error> {
        if let Some(prev) = self.segments.last() {
            if prev.s.have_trailing_zero() {
                bail!(
                    InvalidArgument,
                    msg(
                        "unable to append recording {} after recording {} with trailing zero",
                        row.id,
                        prev.s.id,
                    ),
                );
            }
        } else {
            // Include the current run in this count here, as we're not propagating the
            // run_offset_id further.
            self.prev_media_duration_and_cur_runs = row
                .prev_media_duration_and_runs
                .map(|(d, r)| (d, r + if row.open_id == 0 { 1 } else { 0 }));
        }
        let s = Segment::new(
            db,
            row,
            rel_media_range_90k,
            self.next_frame_num,
            start_at_key,
        )?;

        self.next_frame_num += s.s.frames as u32;
        self.segments.push(s);
        if !self
            .video_sample_entries
            .iter()
            .any(|e| e.id == row.video_sample_entry_id)
        {
            let vse = db
                .video_sample_entries_by_id()
                .get(&row.video_sample_entry_id)
                .unwrap();
            self.video_sample_entries.push(vse.clone());
        }
        Ok(())
    }

    pub fn set_filename(&mut self, filename: &str) -> Result<(), Error> {
        self.content_disposition = Some(
            HeaderValue::try_from(format!("attachment; filename=\"{filename}\""))
                .err_kind(ErrorKind::InvalidArgument)?,
        );
        Ok(())
    }

    /// Builds the `File`, consuming the builder.
    pub fn build(
        mut self,
        db: Arc<db::Database>,
        dirs_by_stream_id: Arc<::base::FastHashMap<i32, Arc<dir::SampleFileDir>>>,
    ) -> Result<File, Error> {
        let mut max_end = None;
        let mut etag = blake3::Hasher::new();
        etag.update(&FORMAT_VERSION[..]);
        if self.include_timestamp_subtitle_track {
            etag.update(b":ts:");
        }
        if let Some(cd) = self.content_disposition.as_ref() {
            etag.update(b":cd:");
            etag.update(cd.as_bytes());
        }
        match self.type_ {
            Type::Normal => {}
            Type::InitSegment => {
                etag.update(b":init:");
            }
            Type::MediaSegment => {
                etag.update(b":media:");
            }
        };
        for s in &mut self.segments {
            let md = &s.rel_media_range_90k;

            // Add the media time for this segment. If edit lists are supported (not media
            // segments), this shouldn't include the portion they skip.
            let start = match self.type_ {
                Type::MediaSegment => s.s.actual_start_90k(),
                _ => md.start,
            };
            self.media_duration_90k += u64::try_from(md.end - start).unwrap();
            let wall = s.recording_start + recording::Duration(i64::from(s.wall(md.start)))
                ..s.recording_start + recording::Duration(i64::from(s.wall(md.end)));
            max_end = match max_end {
                None => Some(wall.end),
                Some(v) => Some(cmp::max(v, wall.end)),
            };

            if self.include_timestamp_subtitle_track {
                // Calculate the number of subtitle samples: starting to ending time (rounding up).
                let start_sec = wall.start.unix_seconds();
                let end_sec =
                    (wall.end + recording::Duration(TIME_UNITS_PER_SEC - 1)).unix_seconds();
                s.num_subtitle_samples = (end_sec - start_sec + 1) as u16;
                self.num_subtitle_samples += s.num_subtitle_samples as u32;
            }

            // Update the etag to reflect this segment.
            let mut data = [0_u8; 28];
            let mut cursor = io::Cursor::new(&mut data[..]);
            cursor
                .write_i64::<BigEndian>(s.s.id.0)
                .err_kind(ErrorKind::Internal)?;
            cursor
                .write_i64::<BigEndian>(s.recording_start.0)
                .err_kind(ErrorKind::Internal)?;
            cursor
                .write_u32::<BigEndian>(s.s.open_id)
                .err_kind(ErrorKind::Internal)?;
            cursor
                .write_i32::<BigEndian>(md.start)
                .err_kind(ErrorKind::Internal)?;
            cursor
                .write_i32::<BigEndian>(md.end)
                .err_kind(ErrorKind::Internal)?;
            etag.update(cursor.into_inner());
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
                let p = self.append_media_mdat()?;

                // If the segment is > 4 GiB, the 32-bit trun data offsets are untrustworthy.
                // We'd need multiple moof+mdat sequences to support large media segments properly.
                if self.body.slices.len() > u64::from(u32::MAX) {
                    bail!(
                        OutOfRange,
                        msg(
                            "media segment has length {}, greater than allowed 4 GiB",
                            self.body.slices.len(),
                        ),
                    );
                }

                p
            }
            Type::InitSegment => {
                self.body
                    .append_static(StaticBytestring::InitSegmentFtypBox)?;
                self.append_moov(creation_ts)?;
                self.body.flush_buf()?;
                0
            }
            Type::Normal => {
                self.body.append_static(StaticBytestring::NormalFtypBox)?;
                self.append_moov(creation_ts)?;
                self.append_normal_mdat()?
            }
        };

        if est_slices < self.body.slices.num() {
            warn!(
                "Estimated {} slices; actually were {} slices",
                est_slices,
                self.body.slices.num()
            );
        } else {
            debug!(
                "Estimated {} slices; actually were {} slices",
                est_slices,
                self.body.slices.num()
            );
        }
        if EST_BUF_LEN < self.body.buf.len() {
            warn!(
                "Estimated {} buf bytes; actually were {}",
                EST_BUF_LEN,
                self.body.buf.len()
            );
        } else {
            debug!(
                "Estimated {} buf bytes; actually were {}",
                EST_BUF_LEN,
                self.body.buf.len()
            );
        }
        trace!("segments: {:#?}", self.segments);
        trace!("slices: {:?}", self.body.slices);
        let last_modified =
            ::std::time::UNIX_EPOCH + ::std::time::Duration::from_secs(max_end as u64);
        let etag = etag.finalize();
        Ok(File(Arc::new(FileInner {
            db,
            dirs_by_stream_id,
            segments: self.segments,
            slices: self.body.slices,
            buf: self.body.buf,
            video_sample_entries: self.video_sample_entries,
            initial_sample_byte_pos,
            last_modified,
            etag: HeaderValue::try_from(format!("\"{}\"", etag.to_hex().as_str()))
                .expect("hex string should be valid UTF-8"),
            content_disposition: self.content_disposition,
            prev_media_duration_and_cur_runs: self.prev_media_duration_and_cur_runs,
            type_: self.type_,
        })))
    }

    fn append_mdat_contents(&mut self) -> Result<(), Error> {
        for (i, s) in self.segments.iter().enumerate() {
            let r = s.s.sample_file_range();
            self.body
                .append_slice(r.end - r.start, SliceType::VideoSampleData, i)?;
        }
        if let Some(p) = self.subtitle_co64_pos {
            BigEndian::write_u64(&mut self.body.buf[p..p + 8], self.body.slices.len());
            for (i, s) in self.segments.iter().enumerate() {
                self.body.append_slice(
                    s.num_subtitle_samples as u64
                        * (mem::size_of::<u16>() + SUBTITLE_LENGTH) as u64,
                    SliceType::SubtitleSampleData,
                    i,
                )?;
            }
        }
        Ok(())
    }

    /// Appends an mdat suitable for a normal `.mp4`, returning initial sample
    /// file byte position.
    fn append_normal_mdat(&mut self) -> Result<u64, Error> {
        // Use the large format to support >= 4 GiB of media data.
        // It'd be nice to use the until-EOF form, but QuickTime Player doesn't support it.
        self.body
            .buf
            .extend_from_slice(b"\x00\x00\x00\x01mdat\x00\x00\x00\x00\x00\x00\x00\x00");
        let mdat_len_pos = self.body.buf.len() - 8;
        self.body.flush_buf()?;
        let initial_sample_byte_pos = self.body.slices.len();
        self.append_mdat_contents()?;
        // 16 is the length of the large mdat header.
        BigEndian::write_u64(
            &mut self.body.buf[mdat_len_pos..mdat_len_pos + 8],
            16 + self.body.slices.len() - initial_sample_byte_pos,
        );
        Ok(initial_sample_byte_pos)
    }

    /// Appends an mdat suitable for a media `.mp4`, returning initial sample
    /// file byte position. Caller should verify that the file doesn't exceed
    /// 32 bits.
    fn append_media_mdat(&mut self) -> Result<u64, Error> {
        // Write the mdat header with zeroes for the length as a placeholder;
        // fill it in after it's known.
        // Safari 14.0.3 (14610.4.3.1.7) doesn't support the large mdat
        // format in media segments. Media segments are unlikely to exceed 4
        // GiB anyway, and the trun offsets would also be problematic.
        let mdat_len_pos = self.body.buf.len();
        self.body.buf.extend_from_slice(b"\x00\x00\x00\x00mdat");
        self.body.flush_buf()?;
        let initial_sample_byte_pos = self.body.slices.len();
        self.append_mdat_contents()?;
        // Fill in the length left as a placeholder above.
        // 8 is the length of the small mdat header.
        // Don't bother checking for overflow; the caller does that.
        BigEndian::write_u32(
            &mut self.body.buf[mdat_len_pos..mdat_len_pos + 4],
            (8 + self.body.slices.len() - initial_sample_byte_pos) as u32,
        );
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

            // Appends a `TrackExtendsBox`, `trex` (ISO/IEC 14496-12 section 8.8.3) for the video
            // track.
            write_length!(self, {
                #[rustfmt::skip]
                self.body.buf.extend_from_slice(&[
                    b't', b'r', b'e', b'x', 0x00, 0x00, 0x00, 0x00, // version + flags
                    0x00, 0x00, 0x00, 0x01, // track_id
                    0x00, 0x00, 0x00, 0x01, // default_sample_description_index
                    0x00, 0x00, 0x00, 0x00, // default_sample_duration
                    0x00, 0x00, 0x00, 0x00, // default_sample_size
                    0x09, 0x21, 0x00, 0x00, // default_sample_flags (non sync):
                                            // is_leading: not a leading sample
                                            // sample_depends_on: does depend on others
                                            // sample_is_depend_on: unknown
                                            // sample_has_redundancy: no padding
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
                self.body.append_u32(1); // sequence_number
            })?;

            // TrackFragmentBox (ISO/IEC 14496-12 section 8.8.6).
            write_length!(self, {
                self.body.buf.extend_from_slice(b"traf");

                // TrackFragmentHeaderBox, tfhd (ISO/IEC 14496-12 section 8.8.7).
                write_length!(self, {
                    #[rustfmt::skip]
                    self.body.buf.extend_from_slice(&[
                        b't', b'f', b'h', b'd',
                        0x00, 0x02, 0x00, 0x00, // version + flags (default-base-is-moof)
                        0x00, 0x00, 0x00, 0x01, // track_id = 1
                    ]);
                })?;
                // `TrackFragmentBaseMediaDecodeTimeBox`, tfdt (ISO/IEC 14496-12 section 8.8.12).
                // This is mandated by the Media Source Extensions ISO BMFF byte format spec.
                // ISO/IEC 14496-12:2015 section 8.8.12.1 says "The Track
                // Fragment Base Media Decode Time Box, if present, shall be
                // positioned after the Track Fragment Header Box and before the
                // first Track Fragment Run box." Safari cares deeply that this rule is followed.
                write_length!(self, {
                    self.body.buf.extend_from_slice(&[
                        b't', b'f', b'd', b't', 0x00, 0x00, 0x00, 0x00, // version + flags
                        0x00, 0x00, 0x00, 0x00, // TODO: baseMediaDecodeTime
                    ]);
                })?;
                self.append_truns()?;
            })?;
        })
    }

    fn append_truns(&mut self) -> Result<(), Error> {
        self.body.flush_buf()?;
        for (i, s) in self.segments.iter().enumerate() {
            self.body
                .append_slice(s.truns_len() as u64, SliceType::Truns, i)?;
        }
        Ok(())
    }

    /// Appends a `MovieHeaderBox` version 1 (ISO/IEC 14496-12 section 8.2.2).
    fn append_mvhd(&mut self, creation_ts: u32) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"mvhd\x01\x00\x00\x00");
            self.body.append_u64(creation_ts as u64);
            self.body.append_u64(creation_ts as u64);
            self.body.append_u32(TIME_UNITS_PER_SEC as u32);
            let d = self.media_duration_90k;
            self.body.append_u64(d);
            self.body.append_static(StaticBytestring::MvhdJunk)?;
            let next_track_id = if self.include_timestamp_subtitle_track {
                3
            } else {
                2
            };
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
            self.body.append_u32(1); // track_id
            self.body.append_u32(0); // reserved
            self.body.append_u32(self.media_duration_90k as u32);
            self.body.append_static(StaticBytestring::TkhdJunk)?;

            let (width, height) = self
                .video_sample_entries
                .iter()
                .fold(None, |m, e| match m {
                    None => Some((e.width, e.height)),
                    Some((w, h)) => Some((cmp::max(w, e.width), cmp::max(h, e.height))),
                })
                .ok_or_else(|| err!(InvalidArgument, msg("no video_sample_entries")))?;
            self.body.append_u32((width as u32) << 16);
            self.body.append_u32((height as u32) << 16);
        })
    }

    /// Appends a `TrackHeaderBox` (ISO/IEC 14496-12 section 8.3.2) suitable for subtitles.
    fn append_subtitle_tkhd(&mut self, creation_ts: u32) -> Result<(), Error> {
        write_length!(self, {
            // flags 7: track_enabled | track_in_movie | track_in_preview
            self.body.buf.extend_from_slice(b"tkhd\x01\x00\x00\x07");
            self.body.append_u64(creation_ts as u64);
            self.body.append_u64(creation_ts as u64);
            self.body.append_u32(2); // track_id
            self.body.append_u32(0); // reserved
            self.body.append_u64(self.media_duration_90k);
            self.body.append_static(StaticBytestring::TkhdJunk)?;
            self.body.append_u32(0); // width, unused.
            self.body.append_u32(0); // height, unused.
        })
    }

    /// Appends an `EditBox` (ISO/IEC 14496-12 section 8.6.5) suitable for video, if necessary.
    fn maybe_append_video_edts(&mut self) -> Result<(), Error> {
        #[derive(Debug, Default)]
        struct Entry {
            segment_duration: u64,
            media_time: u64,
        }
        let mut flushed: Vec<Entry> = Vec::new();
        let mut unflushed: Entry = Default::default();
        let mut cur_media_time: u64 = 0;
        for s in &self.segments {
            // The actual range may start before the desired range because it can only start on a
            // key frame. This relationship should hold true:
            // actual start <= desired start <= desired end
            let actual_start_90k = s.s.actual_start_90k();
            let md = &s.rel_media_range_90k;
            let skip = md.start - actual_start_90k;
            let keep = md.end - md.start;
            if skip < 0 || keep < 0 {
                bail!(
                    Internal,
                    msg("skip={} keep={} on segment {:#?}", skip, keep, s)
                );
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
            return Ok(()); // use implicit one-to-one mapping.
        }

        flushed.push(unflushed);

        trace!("Using edit list: {:?}", flushed);
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

    /// Appends a `MediaHeaderBox` (ISO/IEC 14496-12 section 8.4.2) suitable for either the video
    /// or subtitle track.
    fn append_mdhd(&mut self, creation_ts: u32) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"mdhd\x01\x00\x00\x00");
            self.body.append_u64(u64::from(creation_ts));
            self.body.append_u64(u64::from(creation_ts));
            self.body.append_u32(TIME_UNITS_PER_SEC as u32);
            self.body.append_u64(self.media_duration_90k);
            self.body.append_u32(0x55c40000); // language=und + pre_defined
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
            self.body
                .append_static(StaticBytestring::SubtitleMinfJunk)?;
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
            if self.type_ != Type::InitSegment {
                self.append_video_stss()?;
            }
        })
    }

    /// Appends a `SampleTableBox` (ISO/IEC 14496-12 section 8.5.1) suitable for subtitles.
    fn append_subtitle_stbl(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body
                .append_static(StaticBytestring::SubtitleStblJunk)?;
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
                self.body
                    .append_slice(e.data.len() as u64, SliceType::VideoSampleEntry, i)?;
            }
        })
    }

    /// Appends an `stts` / `TimeToSampleBox` (ISO/IEC 14496-12 section 8.6.1) for video.
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
                        SliceType::Stts,
                        i,
                    )?;
                }
            }
        })
    }

    /// Appends an `stts` / `TimeToSampleBox` (ISO/IEC 14496-12 section 8.6.1) for subtitles.
    fn append_subtitle_stts(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stts\x00\x00\x00\x00");

            let entry_count_pos = self.body.buf.len();
            self.body.append_u32(0); // placeholder for entry_count

            let mut entry_count = 0;
            for s in &self.segments {
                // Note desired media range = actual media range for the subtitle track.
                // We still need to consider media time vs wall time.
                let mr = &s.rel_media_range_90k;
                let start = s.recording_start + recording::Duration(i64::from(s.wall(mr.start)));
                let end = s.recording_start + recording::Duration(i64::from(s.wall(mr.end)));
                let start_next_sec =
                    recording::Time(start.0 + TIME_UNITS_PER_SEC - (start.0 % TIME_UNITS_PER_SEC));

                if end <= start_next_sec {
                    // Segment doesn't last past the next second. Just write one entry.
                    entry_count += 1;
                    self.body.append_u32(1);
                    self.body
                        .append_u32(u32::try_from(mr.end - mr.start).unwrap());
                } else {
                    // The first subtitle lasts until the next second.
                    // media_off is relative to the start of the desired range.
                    let mut media_off = s.media(i32::try_from((start_next_sec - start).0).unwrap());
                    entry_count += 1;
                    self.body.append_u32(1);
                    self.body.append_u32(u32::try_from(media_off).unwrap());

                    // Then there are zero or more "interior" subtitles, one second each. That's
                    // one second converted from wall to media duration. rescale rounds down,
                    // and these errors accumulate, so the final subtitle can be too early by as
                    // much as (MAX_RECORDING_WALL_DURATION/TIME_UNITS_PER_SEC) time units, or
                    // roughly 3 ms. We could avoid that by writing a separate entry for each
                    // second but it's not worth bloating the moov over 3 ms.
                    let end_prev_sec = recording::Time(end.0 - (end.0 % TIME_UNITS_PER_SEC));
                    if start_next_sec < end_prev_sec {
                        let onesec_media_dur = s.media(i32::try_from(TIME_UNITS_PER_SEC).unwrap());
                        let interior = (end_prev_sec - start_next_sec).0 / TIME_UNITS_PER_SEC;
                        entry_count += 1;
                        self.body.append_u32(interior as u32); // count
                        self.body
                            .append_u32(u32::try_from(onesec_media_dur).unwrap());
                        media_off += onesec_media_dur * i32::try_from(interior).unwrap();
                    }

                    // Then there's a final subtitle for the remaining fraction of a second.
                    entry_count += 1;
                    self.body.append_u32(1);
                    self.body
                        .append_u32(u32::try_from(mr.end - mr.start - media_off).unwrap());
                }
            }
            BigEndian::write_u32(
                &mut self.body.buf[entry_count_pos..entry_count_pos + 4],
                entry_count,
            );
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
                let i = self
                    .video_sample_entries
                    .iter()
                    .position(|e| e.id == s.s.video_sample_entry_id())
                    .unwrap();
                self.body.append_u32((i + 1) as u32);
            }
        })
    }

    /// Appends a `SampleToChunkBox` (ISO/IEC 14496-12 section 8.7.4) suitable for subtitles.
    fn append_subtitle_stsc(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body
                .buf
                .extend_from_slice(b"stsc\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x01");
            self.body.append_u32(self.num_subtitle_samples);
            self.body.append_u32(1);
        })
    }

    /// Appends a `SampleSizeBox` (ISO/IEC 14496-12 section 8.7.3) suitable for video.
    fn append_video_stsz(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body
                .buf
                .extend_from_slice(b"stsz\x00\x00\x00\x00\x00\x00\x00\x00");
            let mut entry_count = 0;
            for s in &self.segments {
                entry_count += s.s.frames as u32;
            }
            self.body.append_u32(entry_count);
            if !self.segments.is_empty() {
                self.body.flush_buf()?;
                for (i, s) in self.segments.iter().enumerate() {
                    self.body.append_slice(
                        (mem::size_of::<u32>()) as u64 * (s.s.frames as u64),
                        SliceType::Stsz,
                        i,
                    )?;
                }
            }
        })
    }

    /// Appends a `SampleSizeBox` (ISO/IEC 14496-12 section 8.7.3) suitable for subtitles.
    fn append_subtitle_stsz(&mut self) -> Result<(), Error> {
        write_length!(self, {
            self.body.buf.extend_from_slice(b"stsz\x00\x00\x00\x00");
            self.body
                .append_u32((mem::size_of::<u16>() + SUBTITLE_LENGTH) as u32);
            self.body.append_u32(self.num_subtitle_samples);
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
                    SliceType::Co64,
                    0,
                )?;
            }
        })
    }

    /// Appends a `ChunkLargeOffsetBox` (ISO/IEC 14496-12 section 8.7.5) suitable for subtitles.
    fn append_subtitle_co64(&mut self) -> Result<(), Error> {
        write_length!(self, {
            // Write a placeholder; the actual value will be filled in later.
            self.body.buf.extend_from_slice(
                b"co64\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00",
            );
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
                        SliceType::Stss,
                        i,
                    )?;
                }
            }
        })
    }
}

impl BodyState {
    fn append_u32(&mut self, v: u32) {
        self.buf
            .write_u32::<BigEndian>(v)
            .expect("Vec write shouldn't fail");
    }

    fn append_u64(&mut self, v: u64) {
        self.buf
            .write_u64::<BigEndian>(v)
            .expect("Vec write shouldn't fail");
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
        self.slices
            .append(Slice::new(l + len, t, p)?)
            .err_kind(ErrorKind::Internal)
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
    dirs_by_stream_id: Arc<::base::FastHashMap<i32, Arc<dir::SampleFileDir>>>,
    segments: Vec<Segment>,
    slices: Slices<Slice>,
    buf: Vec<u8>,
    video_sample_entries: SmallVec<[Arc<db::VideoSampleEntry>; 1]>,
    initial_sample_byte_pos: u64,
    last_modified: SystemTime,
    etag: HeaderValue,
    content_disposition: Option<HeaderValue>,
    prev_media_duration_and_cur_runs: Option<(recording::Duration, i32)>,
    type_: Type,
}

impl FileInner {
    fn get_co64(&self, r: Range<u64>, l: u64) -> Result<Chunk, Error> {
        let mut v = Vec::with_capacity(l as usize);
        let mut pos = self.initial_sample_byte_pos;
        for s in &self.segments {
            v.write_u64::<BigEndian>(pos)
                .err_kind(ErrorKind::Internal)?;
            let r = s.s.sample_file_range();
            pos += r.end - r.start;
        }
        Ok(ARefss::new(v)
            .map(|v| &v[r.start as usize..r.end as usize])
            .into())
    }

    /// Gets a stream representing a range of segment `i`'s sample data from disk.
    fn get_video_sample_data(&self, i: usize, r: Range<u64>) -> SliceStream {
        let s = &self.segments[i];
        let sr = s.s.sample_file_range();
        let f = match self.dirs_by_stream_id.get(&s.s.id.stream()) {
            None => {
                return SliceStream::Once(Some(Err(err!(
                    NotFound,
                    msg("{}: stream not found", s.s.id)
                ))))
            }
            Some(d) => d.open_file(s.s.id, (r.start + sr.start)..(r.end + sr.start)),
        };
        SliceStream::File(f)
    }

    fn get_subtitle_sample_data(&self, i: usize, r: Range<u64>, len: u64) -> Result<Chunk, Error> {
        let s = &self.segments[i];
        let md = &s.rel_media_range_90k;
        let wd = s.wall(md.start)..s.wall(md.end);
        let start_sec =
            (s.recording_start + recording::Duration(i64::from(wd.start))).unix_seconds();
        let end_inclusive_sec = (s.recording_start
            + recording::Duration(i64::from(wd.end) + TIME_UNITS_PER_SEC - 1))
        .unix_seconds();
        let len = usize::try_from(len).unwrap();
        let mut v = Vec::with_capacity(len);
        let mut tm = jiff::Zoned::new(
            jiff::Timestamp::from_second(start_sec).expect("timestamp is valid"),
            base::time::global_zone(),
        );
        let mut cur_sec = start_sec;
        loop {
            v.write_u16::<BigEndian>(SUBTITLE_LENGTH as u16)
                .expect("Vec write shouldn't fail");
            use std::io::Write as _;
            write!(v, "{}", tm.strftime(SUBTITLE_TEMPLATE)).expect("Vec write shouldn't fail");
            if cur_sec == end_inclusive_sec {
                break;
            }
            tm += std::time::Duration::from_secs(1);
            cur_sec += 1;
        }
        assert_eq!(len, v.len());
        Ok(ARefss::new(v)
            .map(|v| &v[r.start as usize..r.end as usize])
            .into())
    }
}

#[derive(Clone)]
pub struct File(Arc<FileInner>);

impl File {
    pub async fn append_into_vec(self, v: &mut Vec<u8>) -> Result<(), Error> {
        use http_serve::Entity;
        v.reserve(usize::try_from(self.len()).map_err(|_| {
            err!(
                InvalidArgument,
                msg(
                    "{}-byte mp4 is too big to send over WebSockets!",
                    self.len()
                ),
            )
        })?);
        let mut b = self.get_range(0..self.len());
        loop {
            use futures::stream::StreamExt;
            match b.next().await {
                Some(r) => {
                    let mut chunk = r.map_err(|e| err!(Unknown, source(e)))?;
                    while chunk.has_remaining() {
                        let c = chunk.chunk();
                        v.extend_from_slice(c);
                        let len = c.len();
                        chunk.advance(len);
                    }
                }
                None => return Ok(()),
            }
        }
    }
}

impl http_serve::Entity for File {
    type Data = Chunk;
    type Error = BoxedError;

    fn add_headers(&self, hdrs: &mut http::header::HeaderMap) {
        let mut mime = BytesMut::with_capacity(64);
        mime.extend_from_slice(b"video/mp4; codecs=\"");
        let mut first = true;
        for e in &self.0.video_sample_entries {
            if first {
                first = false
            } else {
                mime.extend_from_slice(b", ");
            }
            mime.extend_from_slice(e.rfc6381_codec.as_bytes());
        }
        mime.extend_from_slice(b"\"");
        hdrs.insert(
            http::header::CONTENT_TYPE,
            http::header::HeaderValue::from_maybe_shared(mime.freeze()).unwrap(),
        );

        if let Some(cd) = self.0.content_disposition.as_ref() {
            hdrs.insert(http::header::CONTENT_DISPOSITION, cd.clone());
        }
        if self.0.type_ == Type::MediaSegment {
            if let Some((d, r)) = self.0.prev_media_duration_and_cur_runs {
                hdrs.insert(
                    "X-Prev-Media-Duration",
                    HeaderValue::try_from(d.0.to_string()).expect("ints are valid headers"),
                );
                hdrs.insert(
                    "X-Runs",
                    HeaderValue::try_from(r.to_string()).expect("ints are valid headers"),
                );
            }
            if let Some(s) = self.0.segments.first() {
                let skip = s.rel_media_range_90k.start - s.s.actual_start_90k();
                if skip > 0 {
                    hdrs.insert(
                        "X-Leading-Media-Duration",
                        HeaderValue::try_from(skip.to_string()).expect("ints are valid headers"),
                    );
                }
            }
        } else if self.0.type_ == Type::InitSegment {
            // FileBuilder::build() should have failed if there were no video_sample_entries.
            let ent = self
                .0
                .video_sample_entries
                .first()
                .expect("no video_sample_entries");
            let aspect = ent.aspect();
            hdrs.insert(
                "X-Aspect",
                HeaderValue::try_from(format!("{}:{}", aspect.numer(), aspect.denom()))
                    .expect("no invalid chars in X-Aspect format"),
            );
        }
    }
    fn last_modified(&self) -> Option<SystemTime> {
        Some(self.0.last_modified)
    }
    fn etag(&self) -> Option<HeaderValue> {
        Some(self.0.etag.clone())
    }
    fn len(&self) -> u64 {
        self.0.slices.len()
    }
    fn get_range(
        &self,
        range: Range<u64>,
    ) -> Pin<Box<dyn Stream<Item = Result<Self::Data, Self::Error>> + Send + Sync>> {
        self.0.slices.get_range(self, range)
    }
}

impl fmt::Debug for File {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("mp4::File")
            .field("last_modified", &self.0.last_modified)
            .field("etag", &self.0.etag)
            .field("slices", &self.0.slices)
            .field("segments", &self.0.segments)
            .finish()
    }
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
    use super::*;
    use crate::stream;
    use base::clock::RealClocks;
    use byteorder::{BigEndian, ByteOrder};
    use db::recording::{self, TIME_UNITS_PER_SEC};
    use db::testutil::{self, TestDb, TEST_STREAM_ID};
    use db::writer;
    use futures::stream::TryStreamExt;
    use http_serve::{self, Entity};
    use hyper::body::Buf;
    use std::fs;
    use std::ops::Range;
    use std::path::Path;
    use std::pin::Pin;
    use std::str;
    use tracing::info;

    async fn fill_slice<E: http_serve::Entity>(slice: &mut [u8], e: &E, start: u64)
    where
        E::Error: ::std::fmt::Debug,
    {
        let mut p = 0;
        Pin::from(e.get_range(start..start + slice.len() as u64))
            .try_for_each(|mut chunk| {
                let len = chunk.remaining();
                chunk.copy_to_slice(&mut slice[p..p + len]);
                p += len;
                futures::future::ok::<_, E::Error>(())
            })
            .await
            .unwrap();
    }

    /// Returns the Blake3 digest of the given `Entity`'s headers and body.
    async fn digest<E: http_serve::Entity>(e: &E) -> blake3::Hash
    where
        E::Error: ::std::fmt::Debug,
    {
        let mut hasher = blake3::Hasher::new();
        let mut headers = http::header::HeaderMap::new();
        e.add_headers(&mut headers);
        for (name, value) in &headers {
            hasher.update(name.as_str().as_bytes());
            hasher.update(&b": "[..]);
            hasher.update(value.as_bytes());
            hasher.update(&b"\r\n"[..]);
        }
        hasher.update(&b"\r\n"[..]);
        Pin::from(e.get_range(0..e.len()))
            .try_fold(hasher, |mut hasher, mut chunk| {
                while chunk.has_remaining() {
                    let c = chunk.chunk();
                    hasher.update(c);
                    let len = c.len();
                    chunk.advance(len);
                }
                futures::future::ok::<_, E::Error>(hasher)
            })
            .await
            .unwrap()
            .finalize()
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
            BoxCursor {
                mp4,
                stack: Vec::new(),
            }
        }

        /// Pushes the box at the given position onto the stack (returning true), or returns
        /// false if pos == max.
        async fn internal_push(&mut self, pos: u64, max: u64) -> bool {
            if pos == max {
                return false;
            }
            let mut hdr = [0u8; 16];
            fill_slice(&mut hdr[..8], &self.mp4, pos).await;
            let (len, hdr_len, boxtype_slice) = match BigEndian::read_u32(&hdr[..4]) {
                0 => (self.mp4.len() - pos, 8, &hdr[4..8]),
                1 => {
                    fill_slice(&mut hdr[8..], &self.mp4, pos + 8).await;
                    (BigEndian::read_u64(&hdr[8..16]), 16, &hdr[4..8])
                }
                l => (l as u64, 8, &hdr[4..8]),
            };
            let mut boxtype = [0u8; 4];
            assert!(pos + (hdr_len as u64) <= max);
            assert!(
                pos + len <= max,
                "path={} pos={} len={} max={}",
                self.path(),
                pos,
                len,
                max
            );
            boxtype[..].copy_from_slice(boxtype_slice);
            self.stack.push(Mp4Box {
                interior: pos + hdr_len as u64..pos + len,
                boxtype,
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

        pub fn depth(&self) -> usize {
            self.stack.len()
        }

        /// Gets the specified byte range within the current box (excluding length and type).
        /// Must not be at EOF.
        pub async fn get(&self, start: u64, buf: &mut [u8]) {
            let interior = &self.stack.last().expect("at root").interior;
            assert!(
                start + (buf.len() as u64) <= interior.end - interior.start,
                "path={} start={} buf.len={} interior={:?}",
                self.path(),
                start,
                buf.len(),
                interior
            );
            fill_slice(buf, &self.mp4, start + interior.start).await;
        }

        pub async fn get_all(&self) -> Vec<u8> {
            let interior = self.stack.last().expect("at root").interior.clone();
            let len = (interior.end - interior.start) as usize;
            trace!("get_all: start={}, len={}", interior.start, len);
            let mut out = Vec::with_capacity(len);
            unsafe { out.set_len(len) };
            fill_slice(&mut out[..], &self.mp4, interior.start).await;
            out
        }

        /// Gets the specified u32 within the current box (excluding length and type).
        /// Must not be at EOF.
        pub async fn get_u32(&self, p: u64) -> u32 {
            let mut buf = [0u8; 4];
            self.get(p, &mut buf).await;
            BigEndian::read_u32(&buf[..])
        }

        pub async fn get_u64(&self, p: u64) -> u64 {
            let mut buf = [0u8; 8];
            self.get(p, &mut buf).await;
            BigEndian::read_u64(&buf[..])
        }

        /// Navigates to the next box after the current one, or up if the current one is last.
        pub async fn next(&mut self) -> bool {
            let old = self
                .stack
                .pop()
                .expect("positioned at root; there is no next");
            let max = self
                .stack
                .last()
                .map(|b| b.interior.end)
                .unwrap_or_else(|| self.mp4.len());
            self.internal_push(old.interior.end, max).await
        }

        /// Finds the next box of the given type after the current one, or navigates up if absent.
        pub async fn find(&mut self, boxtype: &[u8]) -> bool {
            trace!("looking for {}", str::from_utf8(boxtype).unwrap());
            loop {
                if &self.stack.last().unwrap().boxtype[..] == boxtype {
                    return true;
                }
                if !self.next().await {
                    return false;
                }
            }
        }

        /// Moves up the stack. Must not be at root.
        pub fn up(&mut self) {
            self.stack.pop();
        }

        /// Moves down the stack. Must be positioned on a box with children.
        pub async fn down(&mut self) {
            let range = self
                .stack
                .last()
                .map(|b| b.interior.clone())
                .unwrap_or_else(|| 0..self.mp4.len());
            assert!(
                self.internal_push(range.start, range.end).await,
                "no children in {}",
                self.path()
            );
        }
    }

    /// Information returned by `find_track`.
    struct Track {
        edts_cursor: Option<BoxCursor>,
        stbl_cursor: BoxCursor,
    }

    /// Finds the `moov/trak` that has a `tkhd` associated with the given `track_id`, which must
    /// exist.
    async fn find_track(mp4: File, track_id: u32) -> Track {
        let mut cursor = BoxCursor::new(mp4);
        cursor.down().await;
        assert!(cursor.find(b"moov").await);
        cursor.down().await;
        loop {
            assert!(cursor.find(b"trak").await);
            cursor.down().await;
            assert!(cursor.find(b"tkhd").await);
            let mut version = [0u8; 1];
            cursor.get(0, &mut version).await;

            // Let id_pos be the offset after the FullBox section of the track_id.
            let id_pos = match version[0] {
                0 => 8,  // track_id follows 32-bit creation_time and modification_time
                1 => 16, // ...64-bit times...
                v => panic!("unexpected tkhd version {}", v),
            };
            let cur_track_id = cursor.get_u32(4 + id_pos).await;
            trace!(
                "found moov/trak/tkhd with id {}; want {}",
                cur_track_id,
                track_id
            );
            if cur_track_id == track_id {
                break;
            }
            cursor.up();
            assert!(cursor.next().await);
        }
        let edts_cursor;
        if cursor.find(b"edts").await {
            edts_cursor = Some(cursor.clone());
            cursor.up();
        } else {
            edts_cursor = None;
        };
        cursor.down().await;
        assert!(cursor.find(b"mdia").await);
        cursor.down().await;
        assert!(cursor.find(b"minf").await);
        cursor.down().await;
        assert!(cursor.find(b"stbl").await);
        Track {
            edts_cursor,
            stbl_cursor: cursor,
        }
    }

    /// Traverses the box structure in `mp4` depth-first, validating the box positions.
    async fn traverse(mp4: File) {
        let mut cursor = BoxCursor::new(mp4);
        cursor.down().await;
        while cursor.depth() > 0 {
            cursor.next().await;
        }
    }

    fn copy_mp4_to_db(db: &mut TestDb<RealClocks>) {
        let input = stream::testutil::Mp4Stream::open("src/testdata/clip.mp4").unwrap();
        let mut input: Box<dyn stream::Stream> = Box::new(input);

        // 2015-04-26 00:00:00 UTC.
        const START_TIME: recording::Time = recording::Time(1430006400i64 * TIME_UNITS_PER_SEC);
        let video_sample_entry_id = db
            .db
            .lock()
            .insert_video_sample_entry(input.video_sample_entry().clone())
            .unwrap();
        let dir = db.dirs_by_stream_id.get(&TEST_STREAM_ID).unwrap();
        let mut output = writer::Writer::new(dir, &db.db, &db.syncer_channel, TEST_STREAM_ID);

        // end_pts is the pts of the end of the most recent frame (start + duration).
        // It's needed because dir::Writer calculates a packet's duration from its pts and the
        // next packet's pts. That's more accurate for RTSP than ffmpeg's estimate of duration.
        // To write the final packet of this sample .mp4 with a full duration, we need to fake a
        // next packet's pts from the ffmpeg-supplied duration.
        let mut end_pts = None;

        let mut frame_time = START_TIME;

        loop {
            let pkt = match input.next() {
                Ok(p) => p,
                Err(e) if e.kind() == ErrorKind::OutOfRange => {
                    break;
                }
                Err(e) => {
                    panic!("unexpected input error: {}", e);
                }
            };
            frame_time += recording::Duration(i64::from(pkt.duration));
            output
                .write(
                    &mut db.shutdown_rx,
                    &pkt.data[..],
                    frame_time,
                    pkt.pts,
                    pkt.is_key,
                    video_sample_entry_id,
                )
                .unwrap();
            end_pts = Some(pkt.pts + i64::from(pkt.duration));
        }
        output.close(end_pts, None).unwrap();
        db.syncer_channel.flush();
    }

    pub fn create_mp4_from_db(
        tdb: &TestDb<RealClocks>,
        skip_90k: i32,
        shorten_90k: i32,
        include_subtitles: bool,
    ) -> File {
        let mut builder = FileBuilder::new(Type::Normal);
        builder
            .include_timestamp_subtitle_track(include_subtitles)
            .unwrap();
        let all_time = recording::Time(i64::min_value())..recording::Time(i64::max_value());
        {
            let db = tdb.db.lock();
            db.list_recordings_by_time(TEST_STREAM_ID, all_time, &mut |r| {
                let d = r.media_duration_90k;
                assert!(
                    skip_90k + shorten_90k < d,
                    "{}",
                    "skip_90k={skip_90k} shorten_90k={shorten_90k} r={r:?}"
                );
                builder
                    .append(&db, &r, skip_90k..d - shorten_90k, true)
                    .unwrap();
                Ok(())
            })
            .unwrap();
        }
        builder
            .build(tdb.db.clone(), tdb.dirs_by_stream_id.clone())
            .unwrap()
    }

    async fn write_mp4(mp4: &File, dir: &Path) -> String {
        let mut filename = dir.to_path_buf();
        filename.push("clip.new.mp4");
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&filename)
            .unwrap();
        use ::std::io::Write;
        Pin::from(mp4.get_range(0..mp4.len()))
            .try_for_each(|mut chunk| {
                while chunk.has_remaining() {
                    let c = chunk.chunk();
                    let len = match out.write(c) {
                        Err(e) => return futures::future::err(BoxedError::from(e)),
                        Ok(l) => l,
                    };
                    chunk.advance(len);
                }
                futures::future::ok(())
            })
            .await
            .unwrap();
        info!("wrote {:?}", filename);
        filename.to_str().unwrap().to_string()
    }

    fn compare_mp4s(new_filename: &str, pts_offset: i64, shorten: i64) {
        let orig = stream::testutil::Mp4Stream::open("src/testdata/clip.mp4").unwrap();
        let new = stream::testutil::Mp4Stream::open(new_filename).unwrap();

        if pts_offset > 0 {
            // The mp4 crate doesn't interpret the edit list. Manually inspect it.
            let elst = new.elst().unwrap();
            assert_eq!(
                &elst.entries,
                &[mp4::mp4box::elst::ElstEntry {
                    segment_duration: new.duration(),
                    media_time: pts_offset as u64,
                    media_rate: mp4::FixedPointU16::new(1),
                }]
            );
        }

        let mut orig: Box<dyn stream::Stream> = Box::new(orig);
        let mut new: Box<dyn stream::Stream> = Box::new(new);
        assert_eq!(orig.video_sample_entry(), new.video_sample_entry());
        let mut final_durations = None;
        for i in 0.. {
            let orig_pkt = match orig.next() {
                Ok(p) => Some(p),
                Err(e) if e.msg().unwrap() == "end of file" => None,
                Err(e) => {
                    panic!("unexpected input error: {}", e);
                }
            };
            let new_pkt = match new.next() {
                Ok(p) => Some(p),
                Err(e) if e.msg().unwrap() == "end of file" => {
                    break;
                }
                Err(e) => {
                    panic!("unexpected input error: {}", e);
                }
            };
            let (orig_pkt, new_pkt) = match (orig_pkt, new_pkt) {
                (Some(o), Some(n)) => (o, n),
                (None, None) => break,
                (o, n) => panic!("orig: {} new: {}", o.is_some(), n.is_some()),
            };
            assert_eq!(
                orig_pkt.pts, new_pkt.pts, /*+ pts_offset*/
                "pkt {i} pts"
            );
            assert_eq!(orig_pkt.data, new_pkt.data, "pkt {i} data");
            assert_eq!(orig_pkt.is_key, new_pkt.is_key, "pkt {i} key");
            final_durations = Some((i64::from(orig_pkt.duration), i64::from(new_pkt.duration)));
        }

        if let Some((orig_dur, new_dur)) = final_durations {
            // One would normally expect the duration to be exactly the same, but when using an
            // edit list, ffmpeg 3.x appears to extend the last packet's duration by the amount
            // skipped at the beginning. ffmpeg 4.x behaves properly. Allow either behavior.
            // See <https://github.com/scottlamb/moonfire-nvr/issues/10>.
            assert!(
                orig_dur - shorten + pts_offset == new_dur || orig_dur - shorten == new_dur,
                "{}",
                "orig_dur={orig_dur} new_dur={new_dur} shorten={shorten} pts_offset={pts_offset}"
            );
        }
    }

    /// Makes a `.mp4` file which is only good for exercising the `Slice` logic for producing
    /// sample tables that match the supplied encoder.
    fn make_mp4_from_encoders(
        type_: Type,
        db: &TestDb<RealClocks>,
        mut recordings: Vec<db::RecordingToInsert>,
        desired_range_90k: Range<i32>,
        start_at_key: bool,
    ) -> Result<File, Error> {
        let mut builder = FileBuilder::new(type_);
        let mut duration_so_far = 0;
        for r in recordings.drain(..) {
            let row = db.insert_recording_from_encoder(r);
            let d_start = if desired_range_90k.start < duration_so_far {
                0
            } else {
                desired_range_90k.start - duration_so_far
            };
            let d_end = if desired_range_90k.end > duration_so_far + row.media_duration_90k {
                row.media_duration_90k
            } else {
                desired_range_90k.end - duration_so_far
            };
            duration_so_far += row.media_duration_90k;
            builder
                .append(&db.db.lock(), &row, d_start..d_end, start_at_key)
                .unwrap();
        }
        builder.build(db.db.clone(), db.dirs_by_stream_id.clone())
    }

    /// Tests sample table for a simple video index of all sync frames.
    #[tokio::test]
    async fn test_all_sync_frames() {
        testutil::init();
        let db = TestDb::new(RealClocks {});
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::default();
        for i in 1..6 {
            let duration_90k = 2 * i;
            let bytes = 3 * i;
            encoder.add_sample(duration_90k, bytes, true, &mut r);
        }

        // Time range [2, 2+4+6+8) means the 2nd, 3rd, and 4th samples should be included.
        let mp4 =
            make_mp4_from_encoders(Type::Normal, &db, vec![r], 2..2 + 4 + 6 + 8, true).unwrap();
        traverse(mp4.clone()).await;
        let track = find_track(mp4, 1).await;
        assert!(track.edts_cursor.is_none());
        let mut cursor = track.stbl_cursor;
        cursor.down().await;
        cursor.find(b"stts").await;
        assert_eq!(
            cursor.get_all().await,
            &[
                0x00, 0x00, 0x00, 0x00, // version + flags
                0x00, 0x00, 0x00, 0x03, // entry_count
                // entries
                0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x04, // run length / timestamps.
                0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
                0x00, 0x08,
            ]
        );

        cursor.find(b"stsz").await;
        assert_eq!(
            cursor.get_all().await,
            &[
                0x00, 0x00, 0x00, 0x00, // version + flags
                0x00, 0x00, 0x00, 0x00, // sample_size
                0x00, 0x00, 0x00, 0x03, // sample_count
                // entries
                0x00, 0x00, 0x00, 0x06, // size
                0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x0c,
            ]
        );

        cursor.find(b"stss").await;
        assert_eq!(
            cursor.get_all().await,
            &[
                0x00, 0x00, 0x00, 0x00, // version + flags
                0x00, 0x00, 0x00, 0x03, // entry_count
                // entries
                0x00, 0x00, 0x00, 0x01, // sample_number
                0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x03,
            ]
        );
    }

    /// Tests sample table and edit list for a video index with half sync frames.
    #[tokio::test]
    async fn test_half_sync_frames() {
        testutil::init();
        let db = TestDb::new(RealClocks {});
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::default();
        for i in 1..6 {
            let duration_90k = 2 * i;
            let bytes = 3 * i;
            encoder.add_sample(duration_90k, bytes, (i % 2) == 1, &mut r);
        }

        // Time range [2+4+6, 2+4+6+8) means the 4th sample should be included.
        // The 3rd gets pulled in also because it's a sync frame and the 4th isn't.
        let mp4 =
            make_mp4_from_encoders(Type::Normal, &db, vec![r], 2 + 4 + 6..2 + 4 + 6 + 8, true)
                .unwrap();
        traverse(mp4.clone()).await;
        let track = find_track(mp4, 1).await;

        // Examine edts. It should skip the 3rd frame.
        let mut cursor = track.edts_cursor.unwrap();
        cursor.down().await;
        cursor.find(b"elst").await;
        assert_eq!(
            cursor.get_all().await,
            &[
                0x01, 0x00, 0x00, 0x00, // version + flags
                0x00, 0x00, 0x00, 0x01, // length
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, // segment_duration
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, // media_time
                0x00, 0x01, 0x00, 0x00, // media_rate_{integer,fraction}
            ]
        );

        // Examine stbl.
        let mut cursor = track.stbl_cursor;
        cursor.down().await;
        cursor.find(b"stts").await;
        assert_eq!(
            cursor.get_all().await,
            &[
                0x00, 0x00, 0x00, 0x00, // version + flags
                0x00, 0x00, 0x00, 0x02, // entry_count
                // entries
                0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06, // run length / timestamps.
                0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x08,
            ]
        );

        cursor.find(b"stsz").await;
        assert_eq!(
            cursor.get_all().await,
            &[
                0x00, 0x00, 0x00, 0x00, // version + flags
                0x00, 0x00, 0x00, 0x00, // sample_size
                0x00, 0x00, 0x00, 0x02, // sample_count
                // entries
                0x00, 0x00, 0x00, 0x09, // size
                0x00, 0x00, 0x00, 0x0c,
            ]
        );

        cursor.find(b"stss").await;
        assert_eq!(
            cursor.get_all().await,
            &[
                0x00, 0x00, 0x00, 0x00, // version + flags
                0x00, 0x00, 0x00, 0x01, // entry_count
                // entries
                0x00, 0x00, 0x00, 0x01, // sample_number
            ]
        );
    }

    #[tokio::test]
    async fn test_no_segments() {
        testutil::init();
        let db = TestDb::new(RealClocks {});
        let e = make_mp4_from_encoders(Type::Normal, &db, vec![], 0..0, true)
            .err()
            .unwrap();
        assert_eq!(e.kind(), ErrorKind::InvalidArgument);
        assert_eq!(e.msg().unwrap(), "no video_sample_entries");
    }

    #[tokio::test]
    async fn test_multi_segment() {
        testutil::init();
        let db = TestDb::new(RealClocks {});
        let mut encoders = Vec::new();
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::default();
        encoder.add_sample(1, 1, true, &mut r);
        encoder.add_sample(2, 2, false, &mut r);
        encoder.add_sample(3, 3, true, &mut r);
        encoders.push(r);
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::default();
        encoder.add_sample(4, 4, true, &mut r);
        encoder.add_sample(5, 5, false, &mut r);
        encoders.push(r);

        // This should include samples 3 and 4 only, both sync frames.
        let mp4 = make_mp4_from_encoders(Type::Normal, &db, encoders, 1 + 2..1 + 2 + 3 + 4, true)
            .unwrap();
        traverse(mp4.clone()).await;
        let mut cursor = BoxCursor::new(mp4);
        cursor.down().await;
        assert!(cursor.find(b"moov").await);
        cursor.down().await;
        assert!(cursor.find(b"trak").await);
        cursor.down().await;
        assert!(cursor.find(b"mdia").await);
        cursor.down().await;
        assert!(cursor.find(b"minf").await);
        cursor.down().await;
        assert!(cursor.find(b"stbl").await);
        cursor.down().await;
        assert!(cursor.find(b"stss").await);
        assert_eq!(cursor.get_u32(4).await, 2); // entry_count
        assert_eq!(cursor.get_u32(8).await, 1);
        assert_eq!(cursor.get_u32(12).await, 2);
    }

    #[tokio::test]
    async fn test_zero_duration_recording() {
        testutil::init();
        let db = TestDb::new(RealClocks {});
        let mut encoders = Vec::new();
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::default();
        encoder.add_sample(2, 1, true, &mut r);
        encoder.add_sample(3, 2, false, &mut r);
        encoders.push(r);
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::default();
        encoder.add_sample(0, 3, true, &mut r);
        encoders.push(r);

        // Multi-segment recording with an edit list, encoding with a zero-duration recording.
        let mp4 = make_mp4_from_encoders(Type::Normal, &db, encoders, 1..2 + 3, true).unwrap();
        traverse(mp4.clone()).await;
        let track = find_track(mp4, 1).await;
        let mut cursor = track.edts_cursor.unwrap();
        cursor.down().await;
        cursor.find(b"elst").await;
        assert_eq!(cursor.get_u32(4).await, 1); // entry_count
        assert_eq!(cursor.get_u64(8).await, 4); // segment_duration
        assert_eq!(cursor.get_u64(16).await, 1); // media_time
    }

    #[tokio::test]
    async fn test_init_segment() {
        testutil::init();
        let db = TestDb::new(RealClocks {});
        let ent = {
            let mut l = db.db.lock();
            let id = l
                .insert_video_sample_entry(db::VideoSampleEntryToInsert {
                    width: 1920,
                    height: 1080,
                    pasp_h_spacing: 1,
                    pasp_v_spacing: 1,
                    data: [0u8; 100].to_vec(),
                    rfc6381_codec: "avc1.000000".to_owned(),
                })
                .unwrap();
            l.video_sample_entries_by_id().get(&id).unwrap().clone()
        };
        let mut builder = FileBuilder::new(Type::InitSegment);
        builder.append_video_sample_entry(ent);
        let mp4 = builder
            .build(db.db.clone(), db.dirs_by_stream_id.clone())
            .unwrap();
        let mut hdrs = http::header::HeaderMap::new();
        mp4.add_headers(&mut hdrs);
        assert_eq!(hdrs.get("X-Aspect").unwrap(), "16:9");
        traverse(mp4.clone()).await;
    }

    #[tokio::test]
    async fn test_media_segment() {
        testutil::init();
        let db = TestDb::new(RealClocks {});
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::default();
        for i in 1..6 {
            let duration_90k = 2 * i;
            let bytes = 3 * i;
            encoder.add_sample(duration_90k, bytes, (i % 2) == 1, &mut r);
        }

        // Time range [2+4+6, 2+4+6+8+1) means the 4th sample and part of the 5th are included.
        // The 3rd gets pulled in also because it's a sync frame and the 4th isn't.
        let mp4 = make_mp4_from_encoders(
            Type::MediaSegment,
            &db,
            vec![r],
            2 + 4 + 6..2 + 4 + 6 + 8 + 1,
            true,
        )
        .unwrap();
        traverse(mp4.clone()).await;
        let mut cursor = BoxCursor::new(mp4);
        cursor.down().await;

        let mut mdat = cursor.clone();
        assert!(mdat.find(b"mdat").await);

        assert!(cursor.find(b"moof").await);
        cursor.down().await;
        assert!(cursor.find(b"traf").await);
        cursor.down().await;
        assert!(cursor.find(b"trun").await);
        assert_eq!(cursor.get_u32(4).await, 2);
        assert_eq!(cursor.get_u32(8).await as u64, mdat.interior().start);
        assert_eq!(cursor.get_u32(12).await, 174063616); // first_sample_flags
        assert_eq!(cursor.get_u32(16).await, 6); // sample duration
        assert_eq!(cursor.get_u32(20).await, 9); // sample size
        assert_eq!(cursor.get_u32(24).await, 8); // sample duration
        assert_eq!(cursor.get_u32(28).await, 12); // sample size
        assert!(cursor.next().await);
        assert_eq!(cursor.name(), "trun");
        assert_eq!(cursor.get_u32(4).await, 1);
        assert_eq!(
            cursor.get_u32(8).await as u64,
            mdat.interior().start + 9 + 12
        );
        assert_eq!(cursor.get_u32(12).await, 174063616); // first_sample_flags
        assert_eq!(cursor.get_u32(16).await, 1); // sample duration
        assert_eq!(cursor.get_u32(20).await, 15); // sample size
    }

    /// Tests `.mp4` files which represent a single frame, as in the live view WebSocket stream.
    #[tokio::test]
    async fn test_single_frame_media_segment() {
        testutil::init();
        let db = TestDb::new(RealClocks {});
        let mut r = db::RecordingToInsert::default();
        let mut encoder = recording::SampleIndexEncoder::default();
        for i in 1..6 {
            let duration_90k = 2 * i;
            let bytes = 3 * i;
            encoder.add_sample(duration_90k, bytes, (i % 2) == 1, &mut r);
        }

        let mut pos = 0;
        for i in 1..6 {
            let duration_90k = 2 * i;
            let mp4 = make_mp4_from_encoders(
                Type::MediaSegment,
                &db,
                vec![r.clone()],
                pos..pos + duration_90k,
                false,
            )
            .unwrap();
            traverse(mp4.clone()).await;
            let mut cursor = BoxCursor::new(mp4);
            cursor.down().await;
            assert!(cursor.find(b"moof").await);
            cursor.down().await;
            assert!(cursor.find(b"traf").await);
            cursor.down().await;
            assert!(cursor.find(b"trun").await);
            pos += duration_90k;
        }
    }

    #[tokio::test]
    async fn test_round_trip() {
        testutil::init();
        let mut db = TestDb::new(RealClocks {});
        copy_mp4_to_db(&mut db);
        let mp4 = create_mp4_from_db(&db, 0, 0, false);
        traverse(mp4.clone()).await;
        let new_filename = write_mp4(&mp4, db.tmpdir.path()).await;
        compare_mp4s(&new_filename, 0, 0);

        // Test the metadata. This is brittle, which is the point. Any time the digest comparison
        // here fails, it can be updated, but the etag must change as well! Otherwise clients may
        // combine ranges from the new format with ranges from the old format.
        let hash = digest(&mp4).await;
        assert_eq!(
            "123e2cf075125c81e80820bffa412d38729aff05c252c7ea2ab3384905903bb7",
            hash.to_hex().as_str()
        );
        const EXPECTED_ETAG: &str =
            "\"37c89bda9f0513acdc2ab95f48f03b3f797dfa3fb30bbefa6549fdc7296afed2\"";
        assert_eq!(
            Some(HeaderValue::from_str(EXPECTED_ETAG).unwrap()),
            mp4.etag()
        );
        drop(db.syncer_channel);
        db.db.lock().clear_on_flush();
        db.syncer_join.join().unwrap();
    }

    #[tokio::test]
    async fn test_round_trip_with_subtitles() {
        testutil::init();
        let mut db = TestDb::new(RealClocks {});
        copy_mp4_to_db(&mut db);
        let mp4 = create_mp4_from_db(&db, 0, 0, true);
        traverse(mp4.clone()).await;
        let new_filename = write_mp4(&mp4, db.tmpdir.path()).await;
        compare_mp4s(&new_filename, 0, 0);

        // Test the metadata. This is brittle, which is the point. Any time the digest comparison
        // here fails, it can be updated, but the etag must change as well! Otherwise clients may
        // combine ranges from the new format with ranges from the old format.
        let hash = digest(&mp4).await;
        assert_eq!(
            "8f17df9b43dc55654a1e4e00126e7477f43234693d4f1fae72185798a09479d7",
            hash.to_hex().as_str()
        );
        const EXPECTED_ETAG: &str =
            "\"d4af0554a50f6dfff2f7f95a14e16f720bd5fe36a9570cd4fd32f6664f1487c4\"";
        assert_eq!(
            Some(HeaderValue::from_str(EXPECTED_ETAG).unwrap()),
            mp4.etag()
        );
        drop(db.syncer_channel);
        db.db.lock().clear_on_flush();
        db.syncer_join.join().unwrap();
    }

    #[tokio::test]
    async fn test_round_trip_with_edit_list() {
        testutil::init();
        let mut db = TestDb::new(RealClocks {});
        copy_mp4_to_db(&mut db);
        let mp4 = create_mp4_from_db(&db, 1, 0, false);
        traverse(mp4.clone()).await;
        let new_filename = write_mp4(&mp4, db.tmpdir.path()).await;
        compare_mp4s(&new_filename, 1, 0);

        // Test the metadata. This is brittle, which is the point. Any time the digest comparison
        // here fails, it can be updated, but the etag must change as well! Otherwise clients may
        // combine ranges from the new format with ranges from the old format.
        let hash = digest(&mp4).await;
        assert_eq!(
            "1debe76fc6277546209454919550ff4c3a379560f481fa0ce78378cbf3c646f8",
            hash.to_hex().as_str()
        );
        const EXPECTED_ETAG: &str =
            "\"7165c1a866451b7e714a8ad47f4a0022a3749212e945321b35b2f8aaee8aea5c\"";
        assert_eq!(
            Some(HeaderValue::from_str(EXPECTED_ETAG).unwrap()),
            mp4.etag()
        );
        drop(db.syncer_channel);
        db.db.lock().clear_on_flush();
        db.syncer_join.join().unwrap();
    }

    #[tokio::test]
    async fn test_round_trip_with_edit_list_and_subtitles() {
        testutil::init();
        let mut db = TestDb::new(RealClocks {});
        copy_mp4_to_db(&mut db);
        let off = 2 * TIME_UNITS_PER_SEC;
        let mp4 = create_mp4_from_db(&db, i32::try_from(off).unwrap(), 0, true);
        traverse(mp4.clone()).await;
        let new_filename = write_mp4(&mp4, db.tmpdir.path()).await;
        compare_mp4s(&new_filename, off, 0);

        // Test the metadata. This is brittle, which is the point. Any time the digest comparison
        // here fails, it can be updated, but the etag must change as well! Otherwise clients may
        // combine ranges from the new format with ranges from the old format.
        let hash = digest(&mp4).await;
        assert_eq!(
            "caf8b23f3b6ee959981687ff0bcbf8d6b01db9daef35695b2600ffb9f8b54fe1",
            hash.to_hex().as_str()
        );
        const EXPECTED_ETAG: &str =
            "\"167ad6b44502cb09eb15d08fdd2c360e4e54e521251eceeebddf74c4041b0b38\"";
        assert_eq!(
            Some(HeaderValue::from_str(EXPECTED_ETAG).unwrap()),
            mp4.etag()
        );
        drop(db.syncer_channel);
        db.db.lock().clear_on_flush();
        db.syncer_join.join().unwrap();
    }

    #[tokio::test]
    async fn test_round_trip_with_shorten() {
        testutil::init();
        let mut db = TestDb::new(RealClocks {});
        copy_mp4_to_db(&mut db);
        let mp4 = create_mp4_from_db(&db, 0, 1, false);
        traverse(mp4.clone()).await;
        let new_filename = write_mp4(&mp4, db.tmpdir.path()).await;
        compare_mp4s(&new_filename, 0, 1);

        // Test the metadata. This is brittle, which is the point. Any time the digest comparison
        // here fails, it can be updated, but the etag must change as well! Otherwise clients may
        // combine ranges from the new format with ranges from the old format.
        let hash = digest(&mp4).await;
        assert_eq!(
            "e06b5627788828b73b98726dfb6466d32305df64af0acbe6164fc8ab296de473",
            hash.to_hex().as_str()
        );
        const EXPECTED_ETAG: &str =
            "\"2c591788cf06f09b55450cd98cb07c670d580413359260f2d18b9595bd0b430d\"";
        assert_eq!(
            Some(HeaderValue::from_str(EXPECTED_ETAG).unwrap()),
            mp4.etag()
        );
        drop(db.syncer_channel);
        db.db.lock().clear_on_flush();
        db.syncer_join.join().unwrap();
    }
}

#[cfg(all(test, feature = "nightly"))]
mod bench {
    extern crate test;

    use std::convert::Infallible;
    use std::net::SocketAddr;

    use super::tests::create_mp4_from_db;
    use base::clock::RealClocks;
    use db::recording;
    use db::testutil::{self, TestDb};
    use http_serve;
    use hyper;
    use hyper::service::service_fn;
    use url::Url;

    /// An HTTP server for benchmarking.
    /// It's used as a singleton so that when getting a CPU profile of the
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
            let db = TestDb::new(RealClocks {});
            testutil::add_dummy_recordings_to_db(&db.db, 60);
            let mp4 = create_mp4_from_db(&db, 0, 0, false);
            let p = mp4.0.initial_sample_byte_pos;
            let addr: SocketAddr = ([127, 0, 0, 1], 0).into();
            let listener = std::net::TcpListener::bind(addr).unwrap();
            listener.set_nonblocking(true).unwrap();
            let addr = listener.local_addr().unwrap(); // resolve port 0 to a real ephemeral port number.
            let srv = async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                loop {
                    let (conn, _remote_addr) = listener.accept().await.unwrap();
                    conn.set_nodelay(true).unwrap();
                    let io = hyper_util::rt::TokioIo::new(conn);
                    let mp4 = mp4.clone();
                    let svc_fn = service_fn(move |req| {
                        futures::future::ok::<_, Infallible>(http_serve::serve(mp4.clone(), &req))
                    });
                    tokio::spawn(
                        hyper::server::conn::http1::Builder::new().serve_connection(io, svc_fn),
                    );
                }
            };
            std::thread::Builder::new()
                .name("bench-server".to_owned())
                .spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    rt.block_on(srv)
                })
                .unwrap();
            BenchServer {
                url: Url::parse(&format!("http://{}:{}/", addr.ip(), addr.port())).unwrap(),
                generated_len: p,
            }
        }
    }

    static SERVER: std::sync::OnceLock<BenchServer> = std::sync::OnceLock::new();

    #[bench]
    fn build_index(b: &mut test::Bencher) {
        testutil::init();
        let db = TestDb::new(RealClocks {});
        testutil::add_dummy_recordings_to_db(&db.db, 1);

        let db = db.db.lock();
        let segment = {
            let all_time = recording::Time(i64::min_value())..recording::Time(i64::max_value());
            let mut row = None;
            db.list_recordings_by_time(testutil::TEST_STREAM_ID, all_time, &mut |r| {
                row = Some(r);
                Ok(())
            })
            .unwrap();
            let row = row.unwrap();
            let rel_range_90k = 0..row.media_duration_90k;
            super::Segment::new(&db, &row, rel_range_90k, 1, true).unwrap()
        };
        db.with_recording_playback(segment.s.id, &mut |playback| {
            let v = segment.build_index(playback).unwrap(); // warm.
            b.bytes = v.len() as u64; // define the benchmark performance in terms of output bytes.
            b.iter(|| {
                for _i in 0..100 {
                    segment.build_index(playback).unwrap();
                }
            });
            Ok(())
        })
        .unwrap();
    }

    /// Benchmarks serving the generated part of a `.mp4` file (up to the first byte from disk).
    #[bench]
    fn serve_generated_bytes(b: &mut test::Bencher) {
        testutil::init();
        let server = SERVER.get_or_init(BenchServer::new);
        let p = server.generated_len;
        b.bytes = p;
        let client = reqwest::Client::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let run = || {
            rt.block_on(async {
                for _i in 0..100 {
                    let mut resp = client
                        .get(server.url.clone())
                        .header(reqwest::header::RANGE, format!("bytes=0-{}", p - 1))
                        .send()
                        .await
                        .unwrap();
                    let mut size = 0u64;
                    while let Some(b) = resp.chunk().await.unwrap() {
                        size += u64::try_from(b.len()).unwrap();
                    }
                    assert_eq!(p, size);
                }
            });
        };
        run(); // warm.
        b.iter(run);
    }

    #[bench]
    fn mp4_construction(b: &mut test::Bencher) {
        testutil::init();
        let db = TestDb::new(RealClocks {});
        testutil::add_dummy_recordings_to_db(&db.db, 60);
        b.iter(|| {
            for _i in 0..100 {
                create_mp4_from_db(&db, 0, 0, false);
            }
        });
    }
}
