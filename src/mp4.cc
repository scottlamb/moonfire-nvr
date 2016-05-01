// This file is part of Moonfire DVR, a security camera network video recorder.
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
//
// mp4.cc: implementation of mp4.h interface.
//
// This implementation will make the most sense when read side-by-side with
// ISO/IEC 14496-12:2015, available at the following URL:
// <http://standards.iso.org/ittf/PubliclyAvailableStandards/index.html>
//
// mp4.cc generates VirtualFiles via an array of FileSlices. Each FileSlice
// is responsible for some portion of the .mp4 file, generally some subset of
// a single .mp4 "box". Slices fall into these categories:
//
// 1. entirely static data from a const char kConstant[]. This is preferred in
//    the interest of simplicity and efficiency when there is only one useful
//    value for all the fields in the box, including its length.
//
//    These slices are represented using the StringPieceSlice class.
//
// 2. a box's fixed-length fields. In some cases a slice represents the entire
//    contents of a FullBox type; in others a slice represents only the
//    "length" and "type" fields of a container Box, while other contents
//    (such as child boxes) are appended to the box as a separate slice.
//
//    These slices are represented using a specific "struct ...Box" type for
//    type safety and simplicity. The structs match the actual wire format---in
//    particular, they are packed and store fields in network byte order.
//    sizeof(...Box) is meaningful and structure data can be simply written
//    with memcpy, as opposed to via manually-written or generated serialization
//    code. (This approach could be revisited if there's ever a need to run
//    on a compiler that doesn't support __attribute__((packed)) or a
//    processor that doesn't support unaligned access.) The structs are
//    wrapped with the Mp4Box<> template class which manages child slices and
//    fills in the box's length field automatically.
//
// 3. variable-length data generated using the Mp4SampleTablePieces class,
//    representing part of one box dealing with a single recording. These
//    are the largest portion of a typical .mp4's metadata.
//
//    These slices are generated using the FillerFileSlice class. They
//    determine their sizes eagerly (so that the size of the file is known and
//    so that later byte ranges can be served correctly) but only generate
//    their contents when the requested byte range overlaps with the slice
//    (for memory/CPU efficiency).
//
// 4. file-backed variable-length data, representing actual video samples.
//
//    These are represented using the FileSlice class and are mmap()ed via
//    libevent, letting the kernel decide how much to page in at once.
//
// The box hierarchy is constructed through append operations on the Mp4Box
// subclasses. Most of the static data is always in RAM when the VirtualFile
// is, but the file-backed and sample table portions are not. This should be
// a reasonable compromise between simplicity of implementation and memory
// efficiency.

#include "mp4.h"

#include "coding.h"

#define NET_UINT64_C(x) ::moonfire_nvr::ToNetworkU64(UINT64_C(x))
#define NET_INT64_C(x) ::moonfire_nvr::ToNetwork64(UINT64_C(x))
#define NET_UINT32_C(x) ::moonfire_nvr::ToNetworkU32(UINT32_C(x))
#define NET_INT32_C(x) ::moonfire_nvr::ToNetwork32(UINT32_C(x))
#define NET_UINT16_C(x) ::moonfire_nvr::ToNetworkU16(UINT16_C(x))
#define NET_INT16_C(x) ::moonfire_nvr::ToNetwork16(UINT16_C(x))

using ::moonfire_nvr::internal::Mp4FileSegment;

namespace moonfire_nvr {

namespace {

// strftime template for subtitles. Must be a constant length declared below.
const char kSubtitleTemplate[] = "%Y-%m-%d %H:%M:%S %z";
const size_t kSubtitleLength = strlen("2015-07-02 17:10:00 -0700");

// This value should be incremented any time a change is made to this file
// that causes the different bytes to be output for a particular set of
// Mp4Builder options. Incrementing this value will cause the etag to change
// as well.
const char kFormatVersion[] = {0x01};

// ISO/IEC 14496-12 section 4.3, ftyp.
const char kFtypBox[] = {
    0x00, 0x00, 0x00, 0x20,  // length = 32, sizeof(kFtypBox)
    'f',  't',  'y',  'p',   // type
    'i',  's',  'o',  'm',   // major_brand
    0x00, 0x00, 0x02, 0x00,  // minor_version
    'i',  's',  'o',  'm',   // compatible_brands[0]
    'i',  's',  'o',  '2',   // compatible_brands[1]
    'a',  'v',  'c',  '1',   // compatible_brands[2]
    'm',  'p',  '4',  '1',   // compatible_brands[3]
};

// vmhd and dinf boxes. These are both completely static and adjacent in the
// structure, so they're in a single constant.
const char kVmhdAndDinfBoxes[] = {
    // A vmhd box; the "graphicsmode" and "opcolor" values don't have any
    // meaningful use.
    0x00, 0x00, 0x00, 0x14,  // length == sizeof(kVmhdBox)
    'v', 'm', 'h', 'd',      // type = vmhd, ISO/IEC 14496-12 section 12.1.2.
    0x00, 0x00, 0x00, 0x01,  // version + flags(1)
    0x00, 0x00, 0x00, 0x00,  // graphicsmode (copy), opcolor[0]
    0x00, 0x00, 0x00, 0x00,  // opcolor[1], opcolor[2]

    // A dinf box suitable for a "self-contained" .mp4 file (no URL/URN
    // references to external data).
    0x00, 0x00, 0x00, 0x24,  // length == sizeof(kDinfBox)
    'd', 'i', 'n', 'f',      // type = dinf, ISO/IEC 14496-12 section 8.7.1.
    0x00, 0x00, 0x00, 0x1c,  // length
    'd', 'r', 'e', 'f',      // type = dref, ISO/IEC 14496-12 section 8.7.2.
    0x00, 0x00, 0x00, 0x00,  // version and flags
    0x00, 0x00, 0x00, 0x01,  // entry_count
    0x00, 0x00, 0x00, 0x0c,  // length
    'u', 'r', 'l', ' ',      // type = url, ISO/IEC 14496-12 section 8.7.2.
    0x00, 0x00, 0x00, 0x01,  // version=0, flags=self-contained
};

// Likewise, nmhd + dinf boxes, as used for subtitles.
const char kNmhdAndDinfBoxes[] = {
    // A nmhd box; the "graphicsmode" and "opcolor" values don't have any
    // meaningful use.
    0x00, 0x00, 0x00, 0x0c,  // length == sizeof(kNmhdBox)
    'n', 'm', 'h', 'd',      // type = vmhd, ISO/IEC 14496-12 section 12.1.2.
    0x00, 0x00, 0x00, 0x01,  // version + flags(1)

    // A dinf box suitable for a "self-contained" .mp4 file (no URL/URN
    // references to external data).
    0x00, 0x00, 0x00, 0x24,  // length == sizeof(kDinfBox)
    'd', 'i', 'n', 'f',      // type = dinf, ISO/IEC 14496-12 section 8.7.1.
    0x00, 0x00, 0x00, 0x1c,  // length
    'd', 'r', 'e', 'f',      // type = dref, ISO/IEC 14496-12 section 8.7.2.
    0x00, 0x00, 0x00, 0x00,  // version and flags
    0x00, 0x00, 0x00, 0x01,  // entry_count
    0x00, 0x00, 0x00, 0x0c,  // length
    'u', 'r', 'l', ' ',      // type = url, ISO/IEC 14496-12 section 8.7.2.
    0x00, 0x00, 0x00, 0x01,  // version=0, flags=self-contained
};

// A hdlr box suitable for a video track.
const char kVideoHdlrBox[] = {
    0x00, 0x00, 0x00, 0x21,  // length == sizeof(kHdlrBox)
    'h',  'd',  'l',  'r',   // type == hdlr, ISO/IEC 14496-12 section 8.4.3.
    0x00, 0x00, 0x00, 0x00,  // version + flags
    0x00, 0x00, 0x00, 0x00,  // pre_defined
    'v',  'i',  'd',  'e',   // handler = vide
    0x00, 0x00, 0x00, 0x00,  // reserved[0]
    0x00, 0x00, 0x00, 0x00,  // reserved[1]
    0x00, 0x00, 0x00, 0x00,  // reserved[2]
    0x00,                    // name, zero-terminated (empty)
};

// A hdlr box suitable for a subtitle track.
const char kSubtitleHdlrBox[] = {
    0x00, 0x00, 0x00, 0x21,  // length == sizeof(kHdlrBox)
    'h',  'd',  'l',  'r',   // type == hdlr, ISO/IEC 14496-12 section 8.4.3.
    0x00, 0x00, 0x00, 0x00,  // version + flags
    0x00, 0x00, 0x00, 0x00,  // pre_defined
    's',  'b',  't',  'l',   // handler = sbtl
    0x00, 0x00, 0x00, 0x00,  // reserved[0]
    0x00, 0x00, 0x00, 0x00,  // reserved[1]
    0x00, 0x00, 0x00, 0x00,  // reserved[2]
    0x00,                    // name, zero-terminated (empty)
};

// A stsd box suitable for timestamp subtitles.
const char kSubtitleStsdBox[] = {
    0x00, 0x00, 0x00, 0x54,  // length
    's', 't', 's', 'd',      // type == stsd, ISO/IEC 14496-12 section 8.5.2.
    0x00, 0x00, 0x00, 0x00,  // version + flags
    0x00, 0x00, 0x00, 0x01,  // entry_count == 1

    // SampleEntry, ISO/IEC 14496-12 section 8.5.2.2.
    0x00, 0x00, 0x00, 0x44,  // length
    't', 'x', '3', 'g',      // type == tx3g, 3GPP TS 26.245 section 5.16.
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
    0x00, 0x00,                      // startChar
    0x00, 0x00,                      // endChar
    0x00, 0x01,                      // font-ID
    0x00,                            // face-style-flags
    0x12,                            // font-size == 18 px
    '\xff', '\xff', '\xff', '\xff',  // text-color-rgba == opaque white

    // TextSampleEntry.FontTableBox
    0x00, 0x00, 0x00, 0x16,  // length
    'f', 't', 'a', 'b',      // type == ftab, section 5.16
    0x00, 0x01,              // entry-count == 1
    0x00, 0x01,              // font-ID == 1
    0x09,                    // font-name-length == 9
    'M', 'o', 'n', 'o', 's', 'p', 'a', 'c', 'e'};

// Convert from 90kHz units since 1970-01-01 00:00:00 UTC to
// seconds since 1904-01-01 00:00:00 UTC.
uint32_t ToIso14496Timestamp(uint64_t time_90k) {
  return time_90k / kTimeUnitsPerSecond + 24107 * 86400;
}

struct MovieBox {  // ISO/IEC 14496-12 section 8.2.1, moov.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'m', 'o', 'o', 'v'};
};

struct MovieHeaderBoxVersion0 {  // ISO/IEC 14496-12 section 8.2.2, mvhd.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'m', 'v', 'h', 'd'};
  const uint32_t version_and_flags = NET_UINT32_C(0);
  uint32_t creation_time = NET_UINT32_C(0);
  uint32_t modification_time = NET_UINT32_C(0);
  uint32_t timescale = ToNetworkU32(kTimeUnitsPerSecond);
  uint32_t duration = NET_UINT32_C(0);
  const int32_t rate = NET_UINT32_C(0x00010000);
  const int16_t volume = NET_INT16_C(0x0100);
  const int16_t reserved = NET_UINT16_C(0);
  const uint32_t more_reserved[2] = {NET_UINT32_C(0), NET_UINT32_C(0)};
  const int32_t matrix[9] = {
      NET_INT32_C(0x00010000), NET_INT32_C(0), NET_INT32_C(0), NET_INT32_C(0),
      NET_INT32_C(0x00010000), NET_INT32_C(0), NET_INT32_C(0), NET_INT32_C(0),
      NET_INT32_C(0x40000000)};
  const uint32_t pre_defined[6] = {NET_UINT32_C(0), NET_UINT32_C(0),
                                   NET_UINT32_C(0), NET_UINT32_C(0),
                                   NET_UINT32_C(0), NET_UINT32_C(0)};
  uint32_t next_track_id = NET_UINT32_C(2);
} __attribute__((packed));

struct TrackBox {  // ISO/IEC 14496-12 section 8.3.1, trak.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'t', 'r', 'a', 'k'};
} __attribute__((packed));

struct TrackHeaderBoxVersion0 {  // ISO/IEC 14496-12 section 8.3.2, tkhd.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'t', 'k', 'h', 'd'};
  // flags 7 = track_enabled | track_in_movie | track_in_preview
  uint32_t version_and_flags = NET_UINT32_C(7);
  uint32_t creation_time = NET_UINT32_C(0);
  uint32_t modification_time = NET_UINT32_C(0);
  uint32_t track_id = NET_UINT32_C(0);
  const uint32_t reserved1 = NET_UINT64_C(0);
  uint32_t duration = NET_UINT32_C(0);
  const uint32_t reserved2[2] = {NET_UINT32_C(0), NET_UINT32_C(0)};
  const uint16_t layer = NET_UINT16_C(0);
  const uint16_t alternate_group = NET_UINT16_C(0);
  const uint16_t volume = NET_UINT16_C(0);
  const uint16_t reserved3 = NET_UINT16_C(0);
  int32_t matrix[9] = {
      NET_INT32_C(0x00010000), NET_INT32_C(0), NET_INT32_C(0), NET_INT32_C(0),
      NET_INT32_C(0x00010000), NET_INT32_C(0), NET_INT32_C(0), NET_INT32_C(0),
      NET_INT32_C(0x40000000)};
  uint32_t width = NET_UINT32_C(0);
  uint32_t height = NET_UINT32_C(0);
} __attribute__((packed));

struct EditBox {  // ISO/IEC 14496-12 section 8.6.5, edts.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'e', 'd', 't', 's'};
} __attribute__((packed));

struct EditListBoxVersion0 {  // ISO/IEC 14496-12 section 8.6.6, elst.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'e', 'l', 's', 't'};
  const uint32_t version_and_flags = NET_UINT32_C(0);
  uint32_t entry_count = NET_UINT32_C(0);
};

struct MediaBox {  // ISO/IEC 14496-12 section 8.4.1, mdia.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'m', 'd', 'i', 'a'};
} __attribute__((packed));

struct MediaHeaderBoxVersion0 {  // ISO/IEC 14496-12 section 8.4.2, mdhd.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'m', 'd', 'h', 'd'};
  const uint32_t version_and_flags = NET_UINT32_C(0);
  uint32_t creation_time = NET_UINT32_C(0);
  uint32_t modification_time = NET_UINT32_C(0);
  uint32_t timescale = ToNetworkU32(kTimeUnitsPerSecond);
  uint32_t duration = NET_UINT32_C(0);
  uint16_t languages = NET_UINT16_C(0x55c4);  // undetermined
  const uint16_t pre_defined = NET_UINT32_C(0);
} __attribute__((packed));

struct MediaInformationBox {  // ISO/IEC 14496-12 section 8.4.4, minf.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'m', 'i', 'n', 'f'};
} __attribute__((packed));

struct SampleTableBox {  // ISO/IEC 14496-12 section 8.5.1, stbl.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'s', 't', 'b', 'l'};
} __attribute__((packed));

struct SampleDescriptionBoxVersion0 {  // ISO/IEC 14496-12 section 8.5.2, stsd.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'s', 't', 's', 'd'};
  const uint32_t version_and_flags = NET_UINT32_C(0 << 24);
  uint32_t entry_count = NET_UINT32_C(0);
} __attribute__((packed));

struct TimeToSampleBoxVersion0 {  // ISO/IEC 14496-12 section 8.6.1.2, stts.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'s', 't', 't', 's'};
  const uint32_t version_and_flags = NET_UINT32_C(0);
  uint32_t entry_count = NET_UINT32_C(0);
} __attribute__((packed));

struct SampleToChunkBoxVersion0 {  // ISO/IEC 14496-12 section 8.7.4, stsc.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'s', 't', 's', 'c'};
  const uint32_t version_and_flags = NET_UINT32_C(0);
  uint32_t entry_count = NET_UINT32_C(0);
} __attribute__((packed));

struct SampleSizeBoxVersion0 {  // ISO/IEC 14496-12 section 8.7.3, stsz.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'s', 't', 's', 'z'};
  const uint32_t version_and_flags = NET_UINT32_C(0);
  uint32_t sample_size = NET_UINT32_C(0);
  uint32_t sample_count = NET_UINT32_C(0);
} __attribute__((packed));

struct ChunkLargeOffsetBoxVersion0 {  // ISO/IEC 14496-12 section 8.7.5, co64.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'c', 'o', '6', '4'};
  const uint32_t version_and_flags = NET_UINT32_C(0);
  uint32_t entry_count = NET_UINT32_C(0);
} __attribute__((packed));

struct SyncSampleBoxVersion0 {  // ISO/IEC 14496-12 section 8.6.2, stss.
  uint32_t size = NET_UINT32_C(0);
  const char type[4] = {'s', 't', 's', 's'};
  const uint32_t version_and_flags = NET_UINT32_C(0);
  uint32_t entry_count = NET_UINT32_C(0);
} __attribute__((packed));

struct LargeMediaDataBox {  // ISO/IEC 14496-12 section 8.1.1, mdat.
  const uint32_t size = NET_UINT32_C(1);
  const char type[4] = {'m', 'd', 'a', 't'};
  uint64_t largesize = NET_UINT64_C(0);
};

// Grouping of a box's header and the slice representing the header.
// See also ScopedMp4Box, which calculates the length.
template <typename Header>
class Mp4Box {
 public:
  Mp4Box()
      : header_slice_(re2::StringPiece(reinterpret_cast<const char *>(&header_),
                                       sizeof(header_))) {}

  Header &header() { return header_; }
  const FileSlice *header_slice() const { return &header_slice_; }

 private:
  Header header_;
  StringPieceSlice header_slice_;
};

// Helper for adding a mp4 box which calculates the header's size field.
// Construction appends the box to the FileSlices; destruction automatically
// calculates the length including any other slices added in the meantime.
// See also CONSTRUCT_BOX macro.
template <typename Box>
class ScopedMp4Box {
 public:
  explicit ScopedMp4Box(FileSlices *slices, Box *box)
      : starting_size_(slices->size()), slices_(slices), box_(box) {
    slices_->Append(box->header_slice());
  }

  ScopedMp4Box(const ScopedMp4Box<Box> &) = delete;
  void operator=(const ScopedMp4Box<Box> &) = delete;

  ~ScopedMp4Box() {
    box_->header().size = ToNetwork32(slices_->size() - starting_size_);
  }

 private:
  int64_t starting_size_;
  FileSlices *slices_;
  Box *box_;
};

// Macro for less verbose ScopedMp4Box instantiation.
// For use only within Mp4File.
#define CONSTRUCT_BOX(box) \
  ScopedMp4Box<decltype(box)> _scoped_##box(&slices_, &box);

// .mp4 file, constructed from boxes arranged in the order suggested by
// ISO/IEC 14496-12 section 6.2.3 (see Table 1):
//
// * ftyp (file type and compatibility)
// * moov (container for all the metadata)
// ** mvhd (movie header, overall declarations)
//
// ** trak (video: container for an individual track or stream)
// *** tkhd (track header, overall information about the track)
// *** (optional) edts (edit list container)
// **** elst (an edit list)
// *** mdia (container for the media information in a track)
// **** mdhd (media header, overall information about the media)
// *** minf (media information container)
// **** vmhd (video media header, overall information (video track only))
// **** dinf (data information box, container)
// ***** dref (data reference box, declares source(s) of media data in track)
// **** stbl (sample table box, container for the time/space map)
// ***** stsd (sample descriptions (codec types, initilization etc.)
// ***** stts ((decoding) time-to-sample)
// ***** stsc (sample-to-chunk, partial data-offset information)
// ***** stsz (samples sizes (framing))
// ***** co64 (64-bit chunk offset)
// ***** stss (sync sample table)
//
// ** (optional) trak (subtitle: container for an individual track or stream)
// *** tkhd (track header, overall information about the track)
// *** mdia (container for the media information in a track)
// **** mdhd (media header, overall information about the media)
// *** minf (media information container)
// **** nmhd (null media header, overall information)
// **** dinf (data information box, container)
// ***** dref (data reference box, declares source(s) of media data in track)
// **** stbl (sample table box, container for the time/space map)
// ***** stsd (sample descriptions (codec types, initilization etc.)
// ***** stts ((decoding) time-to-sample)
// ***** stsc (sample-to-chunk, partial data-offset information)
// ***** stsz (samples sizes (framing))
// ***** co64 (64-bit chunk offset)
//
// * mdat (media data container)
class Mp4File : public VirtualFile {
 public:
  Mp4File(File *sample_file_dir,
          std::vector<std::unique_ptr<Mp4FileSegment>> segments,
          VideoSampleEntry &&video_sample_entry,
          bool include_timestamp_subtitle_track)
      : sample_file_dir_(sample_file_dir),
        segments_(std::move(segments)),
        video_sample_entry_(std::move(video_sample_entry)),
        ftyp_(re2::StringPiece(kFtypBox, sizeof(kFtypBox))),
        moov_video_trak_mdia_hdlr_(
            re2::StringPiece(kVideoHdlrBox, sizeof(kVideoHdlrBox))),
        moov_video_trak_mdia_minf_vmhddinf_(
            re2::StringPiece(kVmhdAndDinfBoxes, sizeof(kVmhdAndDinfBoxes))),
        moov_video_trak_mdia_minf_stbl_stsd_entry_(video_sample_entry_.data),
        moov_subtitle_trak_mdia_hdlr_(
            re2::StringPiece(kSubtitleHdlrBox, sizeof(kSubtitleHdlrBox))),
        moov_subtitle_trak_mdia_minf_nmhddinf_(
            re2::StringPiece(kNmhdAndDinfBoxes, sizeof(kNmhdAndDinfBoxes))),
        moov_subtitle_trak_mdia_minf_stbl_stsd_(
            re2::StringPiece(kSubtitleStsdBox, sizeof(kSubtitleStsdBox))),
        include_timestamp_subtitle_track_(include_timestamp_subtitle_track) {
    uint32_t duration = 0;
    int64_t max_time_90k = 0;
    for (const auto &segment : segments_) {
      duration += segment->pieces.end_90k() - segment->rel_start_90k;
      int64_t start_90k =
          segment->recording.start_time_90k + segment->rel_start_90k;
      int64_t end_90k =
          segment->recording.start_time_90k + segment->pieces.end_90k();
      int64_t start_ts = start_90k / kTimeUnitsPerSecond;
      int64_t end_ts =
          (end_90k + kTimeUnitsPerSecond - 1) / kTimeUnitsPerSecond;
      num_subtitle_samples_ += end_ts - start_ts;
      max_time_90k = std::max(max_time_90k, end_90k);
    }
    last_modified_ = max_time_90k / kTimeUnitsPerSecond;
    auto creation_ts = ToIso14496Timestamp(max_time_90k);

    slices_.Append(&ftyp_);
    AppendMoov(ToNetworkU32(duration), ToNetworkU32(creation_ts));

    // Add the mdat_ without using CONSTRUCT_BOX.
    // mdat_ is special because it uses largesize rather than size.
    int64_t size_before_mdat = slices_.size();
    slices_.Append(mdat_.header_slice());
    initial_sample_byte_pos_ = slices_.size();
    for (const auto &segment : segments_) {
      segment->sample_file_slice.Init(
          sample_file_dir_, segment->recording.sample_file_uuid.UnparseText(),
          segment->pieces.sample_pos());
      slices_.Append(&segment->sample_file_slice, FileSlices::kLazy);
    }
    if (include_timestamp_subtitle_track_) {
      subtitle_sample_byte_pos_ = slices_.size();
      mdat_subtitle_.Init(
          num_subtitle_samples_ * (sizeof(uint16_t) + kSubtitleLength),
          [this](std::string *s, std::string *error_message) {
            return FillMdatSubtitle(s, error_message);
          });
      slices_.Append(&mdat_subtitle_);
    }
    mdat_.header().largesize = ToNetworkU64(slices_.size() - size_before_mdat);

    auto etag_digest = Digest::SHA1();
    etag_digest->Update(
        re2::StringPiece(kFormatVersion, sizeof(kFormatVersion)));
    if (include_timestamp_subtitle_track_) {
      etag_digest->Update(":ts:");
    }
    std::string segment_times;
    for (const auto &segment : segments_) {
      segment_times.clear();
      Append64(segment->pieces.sample_pos().begin, &segment_times);
      Append64(segment->pieces.sample_pos().end, &segment_times);
      etag_digest->Update(segment_times);
      etag_digest->Update(segment->recording.sample_file_sha1);
    }
    etag_ = StrCat("\"", ToHex(etag_digest->Finalize()), "\"");
    VLOG(1) << "Constructed .mp4 has " << slices_.num_slices() << " slices for "
            << segments_.size() << " segments, " << slices_.size() << " bytes.";
  }

  time_t last_modified() const final { return last_modified_; }
  std::string etag() const final { return etag_; }
  std::string mime_type() const final { return "video/mp4"; }
  int64_t size() const final { return slices_.size(); }
  int64_t AddRange(ByteRange range, EvBuffer *buf,
                   std::string *error_message) const final {
    return slices_.AddRange(range, buf, error_message);
  }

 private:
  void AppendMoov(uint32_t net_duration, uint32_t net_creation_ts) {
    CONSTRUCT_BOX(moov_);
    {
      CONSTRUCT_BOX(moov_mvhd_);
      moov_mvhd_.header().creation_time = net_creation_ts;
      moov_mvhd_.header().modification_time = net_creation_ts;
      moov_mvhd_.header().duration = net_duration;
      moov_mvhd_.header().duration = net_duration;
    }
    {
      CONSTRUCT_BOX(moov_video_trak_);
      {
        CONSTRUCT_BOX(moov_video_trak_tkhd_);
        moov_video_trak_tkhd_.header().creation_time = net_creation_ts;
        moov_video_trak_tkhd_.header().modification_time = net_creation_ts;
        moov_video_trak_tkhd_.header().track_id = NET_UINT32_C(1);
        moov_video_trak_tkhd_.header().duration = net_duration;
        moov_video_trak_tkhd_.header().width =
            NET_UINT32_C(video_sample_entry_.width << 16);
        moov_video_trak_tkhd_.header().height =
            NET_UINT32_C(video_sample_entry_.height << 16);
      }
      MaybeAppendVideoEdts();
      {
        CONSTRUCT_BOX(moov_video_trak_mdia_);
        {
          CONSTRUCT_BOX(moov_video_trak_mdia_mdhd_);
          moov_video_trak_mdia_mdhd_.header().creation_time = net_creation_ts;
          moov_video_trak_mdia_mdhd_.header().modification_time =
              net_creation_ts;
          moov_video_trak_mdia_mdhd_.header().duration = net_duration;
        }
        slices_.Append(&moov_video_trak_mdia_hdlr_);
        {
          CONSTRUCT_BOX(moov_video_trak_mdia_minf_);
          slices_.Append(&moov_video_trak_mdia_minf_vmhddinf_);
          AppendVideoStbl();
        }
      }
    }
    if (include_timestamp_subtitle_track_) {
      AppendSubtitleTrack(net_duration, net_creation_ts);
    }
  }

  void MaybeAppendVideoEdts() {
    struct Entry {
      Entry(int32_t segment_duration, int32_t media_time)
          : segment_duration(segment_duration), media_time(media_time) {}
      int32_t segment_duration = 0;
      int32_t media_time = 0;
      int32_t end() const { return segment_duration + media_time; }
    };
    std::vector<Entry> entries;
    int64_t cur_media_time = 0;
    for (const auto &segment : segments_) {
      auto skip = segment->rel_start_90k - segment->pieces.start_90k();
      auto keep = segment->pieces.end_90k() - segment->rel_start_90k;
      DCHECK_GE(skip, 0);
      DCHECK_GT(keep, 0);
      cur_media_time += skip;
      if (!entries.empty() && entries.back().end() == cur_media_time) {
        entries.back().segment_duration += keep;
      } else {
        entries.emplace_back(keep, cur_media_time);
      }
      DCHECK_GT(segment->pieces.duration_90k(), 0);
      cur_media_time += keep;
    }
    if (entries.size() == 1 && entries[0].media_time == 0) {
      return;  // use implicit one-to-one mapping.
    }

    VLOG(1) << "Using edit list with " << entries.size() << " entries.";
    std::string *s = &moov_video_trak_edts_elst_entries_str_;
    for (const auto &entry : entries) {
      VLOG(2) << "...duration=" << entry.segment_duration
              << ", time=" << entry.media_time;
      AppendU32(entry.segment_duration, s);
      AppendU32(entry.media_time, s);
      AppendU16(1, s);  // media_rate_integer
      AppendU16(1, s);  // media_rate_fraction
    }
    CONSTRUCT_BOX(moov_video_trak_edts_);
    CONSTRUCT_BOX(moov_video_trak_edts_elst_);
    moov_video_trak_edts_elst_.header().entry_count =
        ToNetworkU32(entries.size());
    moov_video_trak_edts_elst_entries_.Init(
        moov_video_trak_edts_elst_entries_str_);
    slices_.Append(&moov_video_trak_edts_elst_entries_);
  }

  void AppendVideoStbl() {
    CONSTRUCT_BOX(moov_video_trak_mdia_minf_stbl_);
    {
      CONSTRUCT_BOX(moov_video_trak_mdia_minf_stbl_stsd_);
      moov_video_trak_mdia_minf_stbl_stsd_.header().entry_count =
          NET_UINT32_C(1);
      slices_.Append(&moov_video_trak_mdia_minf_stbl_stsd_entry_);
    }
    {
      CONSTRUCT_BOX(moov_video_trak_mdia_minf_stbl_stts_);
      int32_t stts_entry_count = 0;
      for (const auto &segment : segments_) {
        stts_entry_count += segment->pieces.stts_entry_count();
        slices_.Append(segment->pieces.stts_entries());
      }
      moov_video_trak_mdia_minf_stbl_stts_.header().entry_count =
          ToNetwork32(stts_entry_count);
    }
    {
      CONSTRUCT_BOX(moov_video_trak_mdia_minf_stbl_stsc_);
      moov_video_trak_mdia_minf_stbl_stsc_entries_.Init(
          3 * sizeof(uint32_t) * segments_.size(),
          [this](std::string *s, std::string *error_message) {
            return FillVideoStscEntries(s, error_message);
          });
      moov_video_trak_mdia_minf_stbl_stsc_.header().entry_count =
          ToNetwork32(segments_.size());
      slices_.Append(&moov_video_trak_mdia_minf_stbl_stsc_entries_);
    }
    {
      CONSTRUCT_BOX(moov_video_trak_mdia_minf_stbl_stsz_);
      uint32_t stsz_entry_count = 0;
      for (const auto &segment : segments_) {
        stsz_entry_count += segment->pieces.stsz_entry_count();
        slices_.Append(segment->pieces.stsz_entries());
      }
      moov_video_trak_mdia_minf_stbl_stsz_.header().sample_count =
          ToNetwork32(stsz_entry_count);
    }
    {
      CONSTRUCT_BOX(moov_video_trak_mdia_minf_stbl_co64_);
      moov_video_trak_mdia_minf_stbl_co64_entries_.Init(
          sizeof(uint64_t) * segments_.size(),
          [this](std::string *s, std::string *error_message) {
            return FillVideoCo64Entries(s, error_message);
          });
      moov_video_trak_mdia_minf_stbl_co64_.header().entry_count =
          ToNetwork32(segments_.size());
      slices_.Append(&moov_video_trak_mdia_minf_stbl_co64_entries_);
    }
    {
      CONSTRUCT_BOX(moov_video_trak_mdia_minf_stbl_stss_);
      uint32_t stss_entry_count = 0;
      for (const auto &segment : segments_) {
        stss_entry_count += segment->pieces.stss_entry_count();
        slices_.Append(segment->pieces.stss_entries());
      }
      moov_video_trak_mdia_minf_stbl_stss_.header().entry_count =
          ToNetwork32(stss_entry_count);
    }
  }

  bool FillVideoStscEntries(std::string *s, std::string *error_message) {
    uint32_t chunk = 0;
    for (const auto &segment : segments_) {
      AppendU32(++chunk, s);
      AppendU32(segment->pieces.samples(), s);
      AppendU32(1, s);  // TODO: sample_description_index.
    }
    return true;
  }

  bool FillVideoCo64Entries(std::string *s, std::string *error_message) {
    int64_t pos = initial_sample_byte_pos_;
    for (const auto &segment : segments_) {
      AppendU64(pos, s);
      pos += segment->sample_file_slice.size();
    }
    return true;
  }

  void AppendSubtitleTrack(uint32_t net_duration, uint32_t net_creation_ts) {
    CONSTRUCT_BOX(moov_subtitle_trak_);
    {
      CONSTRUCT_BOX(moov_subtitle_trak_tkhd_);
      auto &hdr = moov_subtitle_trak_tkhd_.header();
      hdr.creation_time = net_creation_ts;
      hdr.modification_time = net_creation_ts;
      hdr.track_id = NET_UINT32_C(2);
      hdr.duration = net_duration;
#if 0
      hdr.width = NET_UINT32_C(800 /*video_sample_entry_.width*/ << 16);
      hdr.height = NET_UINT32_C(60 /*video_sample_entry_.height*/ << 16);
      hdr.matrix[0] = NET_INT32_C(1 << 16);    // a
      hdr.matrix[1] = NET_INT32_C(0 << 16);    // b
      hdr.matrix[2] = NET_INT32_C(0 << 30);    // u
      hdr.matrix[3] = NET_INT32_C(0 << 16);    // c
      hdr.matrix[4] = NET_INT32_C(1 << 16);    // d
      hdr.matrix[5] = NET_INT32_C(0 << 30);    // v
      hdr.matrix[6] = NET_INT32_C(240 << 16);  // x
      hdr.matrix[7] = NET_INT32_C(660 << 16);  // y
      hdr.matrix[8] = NET_INT32_C(1 << 30);    // w
#endif
    }
    {
      CONSTRUCT_BOX(moov_subtitle_trak_mdia_);
      {
        CONSTRUCT_BOX(moov_video_trak_mdia_mdhd_);
        moov_subtitle_trak_mdia_mdhd_.header().creation_time = net_creation_ts;
        moov_subtitle_trak_mdia_mdhd_.header().modification_time =
            net_creation_ts;
        moov_subtitle_trak_mdia_mdhd_.header().duration = net_duration;
      }
      slices_.Append(&moov_subtitle_trak_mdia_hdlr_);
      {
        CONSTRUCT_BOX(moov_subtitle_trak_mdia_minf_);
        slices_.Append(&moov_subtitle_trak_mdia_minf_nmhddinf_);
        AppendSubtitleStbl();
      }
    }
  }

  void AppendSubtitleStbl() {
    CONSTRUCT_BOX(moov_subtitle_trak_mdia_minf_stbl_);
    slices_.Append(&moov_subtitle_trak_mdia_minf_stbl_stsd_);
    {
      CONSTRUCT_BOX(moov_subtitle_trak_mdia_minf_stbl_stts_);
      int32_t num_entries = 0;
      FillSubtitleSttsEntries(&num_entries);
      moov_subtitle_trak_mdia_minf_stbl_stts_.header().entry_count =
          ToNetwork32(num_entries);
      slices_.Append(&moov_subtitle_trak_mdia_minf_stbl_stts_entries_);
    }
    {
      CONSTRUCT_BOX(moov_subtitle_trak_mdia_minf_stbl_stsc_);
      moov_subtitle_trak_mdia_minf_stbl_stsc_entries_.Init(
          3 * sizeof(uint32_t),
          [this](std::string *s, std::string *error_message) {
            AppendU32(1, s);                      // first_chunk
            AppendU32(num_subtitle_samples_, s);  // samples_per_chunk
            AppendU32(1, s);                      // sample_description
            return true;
          });
      moov_subtitle_trak_mdia_minf_stbl_stsc_.header().entry_count =
          ToNetwork32(1);
      slices_.Append(&moov_subtitle_trak_mdia_minf_stbl_stsc_entries_);
    }
    {
      CONSTRUCT_BOX(moov_subtitle_trak_mdia_minf_stbl_stsz_);
      moov_subtitle_trak_mdia_minf_stbl_stsz_.header().sample_size =
          ToNetwork32(sizeof(uint16_t) + kSubtitleLength);
      moov_subtitle_trak_mdia_minf_stbl_stsz_.header().sample_count =
          ToNetwork32(num_subtitle_samples_);
    }
    {
      CONSTRUCT_BOX(moov_subtitle_trak_mdia_minf_stbl_co64_);
      moov_subtitle_trak_mdia_minf_stbl_co64_entries_.Init(
          sizeof(uint64_t), [this](std::string *s, std::string *error_message) {
            AppendU64(subtitle_sample_byte_pos_, s);
            return true;
          });
      moov_subtitle_trak_mdia_minf_stbl_co64_.header().entry_count =
          ToNetwork32(1);
      slices_.Append(&moov_subtitle_trak_mdia_minf_stbl_co64_entries_);
    }
  }

  // Fills |moov_subtitle_trak_mdia_minf_stbl_stts_entries_| and puts
  // the number of STTS entries into |num_entries| (in host byte order).
  void FillSubtitleSttsEntries(int32_t *num_entries) {
    std::string &s = moov_subtitle_trak_mdia_minf_stbl_stts_entries_str_;
    for (const auto &segment : segments_) {
      int64_t start_90k =
          segment->recording.start_time_90k + segment->rel_start_90k;
      int64_t end_90k =
          segment->recording.start_time_90k + segment->pieces.end_90k();
      int64_t start_next_90k =
          start_90k + kTimeUnitsPerSecond - (start_90k % kTimeUnitsPerSecond);

      if (end_90k <= start_next_90k) {
        ++*num_entries;
        AppendU32(1, &s);                    // sample_count
        AppendU32(end_90k - start_90k, &s);  // sample_duration
      } else {
        ++*num_entries;
        AppendU32(1, &s);                           // sample_count
        AppendU32(start_next_90k - start_90k, &s);  // sample_duration

        int64_t end_prev_90k = end_90k - (end_90k % kTimeUnitsPerSecond);
        if (start_next_90k < end_prev_90k) {
          ++*num_entries;
          int64_t interior =
              (end_prev_90k - start_next_90k) / kTimeUnitsPerSecond;
          AppendU32(interior, &s);  // sample_count
          AppendU32(kTimeUnitsPerSecond, &s);
        }

        ++*num_entries;
        AppendU32(1, &s);                       // sample_count
        AppendU32(end_90k - end_prev_90k, &s);  // sample_duration
      }
    }
    moov_subtitle_trak_mdia_minf_stbl_stts_entries_.Init(
        moov_subtitle_trak_mdia_minf_stbl_stts_entries_str_);
  }

  bool FillMdatSubtitle(std::string *s, std::string *error_message) {
    char buf[kSubtitleLength + 1 /* null */];
    struct tm mytm;
    memset(&mytm, 0, sizeof(mytm));
    for (const auto &segment : segments_) {
      int64_t start_90k =
          segment->recording.start_time_90k + segment->rel_start_90k;
      int64_t end_90k =
          segment->recording.start_time_90k + segment->pieces.end_90k();
      int64_t start_ts = start_90k / kTimeUnitsPerSecond;
      int64_t end_ts =
          (end_90k + kTimeUnitsPerSecond - 1) / kTimeUnitsPerSecond;
      for (time_t ts = start_ts; ts < end_ts; ++ts) {
        AppendU16(kSubtitleLength, s);
        localtime_r(&ts, &mytm);
        size_t r = strftime(buf, sizeof(buf), kSubtitleTemplate, &mytm);
        if (r != kSubtitleLength) {
          *error_message = StrCat("strftime unexpectedly returned ", r);
          return false;
        }
        s->append(buf, r);
      }
    }
    return true;
  }

  int64_t initial_sample_byte_pos_ = 0;
  int64_t subtitle_sample_byte_pos_ = 0;
  int64_t num_subtitle_samples_ = 0;
  File *sample_file_dir_ = nullptr;
  std::vector<std::unique_ptr<Mp4FileSegment>> segments_;
  VideoSampleEntry video_sample_entry_;
  FileSlices slices_;
  std::string etag_;
  time_t last_modified_ = -1;

  StringPieceSlice ftyp_;
  Mp4Box<MovieBox> moov_;
  Mp4Box<MovieHeaderBoxVersion0> moov_mvhd_;

  Mp4Box<TrackBox> moov_video_trak_;
  Mp4Box<TrackHeaderBoxVersion0> moov_video_trak_tkhd_;
  Mp4Box<EditBox> moov_video_trak_edts_;
  Mp4Box<EditListBoxVersion0> moov_video_trak_edts_elst_;
  StringPieceSlice moov_video_trak_edts_elst_entries_;
  std::string moov_video_trak_edts_elst_entries_str_;
  Mp4Box<MediaBox> moov_video_trak_mdia_;
  Mp4Box<MediaHeaderBoxVersion0> moov_video_trak_mdia_mdhd_;
  StringPieceSlice moov_video_trak_mdia_hdlr_;
  Mp4Box<MediaInformationBox> moov_video_trak_mdia_minf_;
  StringPieceSlice moov_video_trak_mdia_minf_vmhddinf_;
  Mp4Box<SampleTableBox> moov_video_trak_mdia_minf_stbl_;
  Mp4Box<SampleDescriptionBoxVersion0> moov_video_trak_mdia_minf_stbl_stsd_;
  StringPieceSlice moov_video_trak_mdia_minf_stbl_stsd_entry_;
  Mp4Box<TimeToSampleBoxVersion0> moov_video_trak_mdia_minf_stbl_stts_;
  Mp4Box<SampleToChunkBoxVersion0> moov_video_trak_mdia_minf_stbl_stsc_;
  FillerFileSlice moov_video_trak_mdia_minf_stbl_stsc_entries_;
  Mp4Box<SampleSizeBoxVersion0> moov_video_trak_mdia_minf_stbl_stsz_;
  Mp4Box<ChunkLargeOffsetBoxVersion0> moov_video_trak_mdia_minf_stbl_co64_;
  FillerFileSlice moov_video_trak_mdia_minf_stbl_co64_entries_;
  Mp4Box<SyncSampleBoxVersion0> moov_video_trak_mdia_minf_stbl_stss_;

  Mp4Box<TrackBox> moov_subtitle_trak_;
  Mp4Box<TrackHeaderBoxVersion0> moov_subtitle_trak_tkhd_;
  Mp4Box<MediaBox> moov_subtitle_trak_mdia_;
  Mp4Box<MediaHeaderBoxVersion0> moov_subtitle_trak_mdia_mdhd_;
  StringPieceSlice moov_subtitle_trak_mdia_hdlr_;
  Mp4Box<MediaInformationBox> moov_subtitle_trak_mdia_minf_;
  StringPieceSlice moov_subtitle_trak_mdia_minf_nmhddinf_;
  Mp4Box<SampleTableBox> moov_subtitle_trak_mdia_minf_stbl_;
  StringPieceSlice moov_subtitle_trak_mdia_minf_stbl_stsd_;
  Mp4Box<TimeToSampleBoxVersion0> moov_subtitle_trak_mdia_minf_stbl_stts_;
  StringPieceSlice moov_subtitle_trak_mdia_minf_stbl_stts_entries_;
  std::string moov_subtitle_trak_mdia_minf_stbl_stts_entries_str_;
  Mp4Box<SampleToChunkBoxVersion0> moov_subtitle_trak_mdia_minf_stbl_stsc_;
  FillerFileSlice moov_subtitle_trak_mdia_minf_stbl_stsc_entries_;
  Mp4Box<SampleSizeBoxVersion0> moov_subtitle_trak_mdia_minf_stbl_stsz_;
  Mp4Box<ChunkLargeOffsetBoxVersion0> moov_subtitle_trak_mdia_minf_stbl_co64_;
  FillerFileSlice moov_subtitle_trak_mdia_minf_stbl_co64_entries_;
  Mp4Box<SyncSampleBoxVersion0> moov_subtitle_trak_mdia_minf_stbl_stss_;
  FillerFileSlice mdat_subtitle_;

  Mp4Box<LargeMediaDataBox> mdat_;

  bool include_timestamp_subtitle_track_ = false;
};

#undef CONSTRUCT_BOX

}  // namespace

namespace internal {

bool Mp4SampleTablePieces::Init(const Recording *recording,
                                int sample_entry_index, int32_t sample_offset,
                                int32_t start_90k, int32_t end_90k,
                                std::string *error_message) {
  sample_entry_index_ = sample_entry_index;
  sample_offset_ = sample_offset;
  desired_end_90k_ = end_90k;
  SampleIndexIterator it = SampleIndexIterator(recording->video_index);
  auto recording_duration_90k =
      recording->end_time_90k - recording->start_time_90k;
  bool fast_path = start_90k == 0 && end_90k >= recording_duration_90k;
  if (fast_path) {
    VLOG(1) << "Fast path, frames=" << recording->video_samples
            << ", key=" << recording->video_sync_samples;
    sample_pos_.end = recording->sample_file_bytes;
    begin_ = it;
    frames_ = recording->video_samples;
    key_frames_ = recording->video_sync_samples;
    actual_end_90k_ = recording_duration_90k;
  } else {
    VLOG(1) << "Slow path.";
    if (!it.done() && !it.is_key()) {
      *error_message = "First frame must be a key frame.";
      return false;
    }
    for (; !it.done(); it.Next()) {
      VLOG(3) << "Processing frame with start " << it.start_90k()
              << (it.is_key() ? " (key)" : " (non-key)");
      // Find boundaries.
      if (it.start_90k() <= start_90k && it.is_key()) {
        VLOG(3) << "...new start candidate.";
        begin_ = it;
        sample_pos_.begin = begin_.pos();
        frames_ = 0;
        key_frames_ = 0;
      }
      if (it.start_90k() >= end_90k) {
        VLOG(3) << "...past end.";
        break;
      }

      // Process this frame.
      frames_++;
      if (it.is_key()) {
        key_frames_++;
      }

      // This is the current best candidate to end.
      actual_end_90k_ = it.end_90k();
    }
    sample_pos_.end = it.pos();
  }
  if (it.has_error()) {
    *error_message = it.error();
    return false;
  }
  actual_end_90k_ = std::min(actual_end_90k_, desired_end_90k_);
  VLOG(1) << "requested ts [" << start_90k << ", " << end_90k << "), got ts ["
          << begin_.start_90k() << ", " << actual_end_90k_ << "), " << frames_
          << " frames (" << key_frames_
          << " key), byte positions: " << sample_pos_;

  stts_entries_.Init(2 * sizeof(int32_t) * stts_entry_count(),
                     [this](std::string *s, std::string *error_message) {
                       return FillSttsEntries(s, error_message);
                     });
  stss_entries_.Init(sizeof(int32_t) * stss_entry_count(),
                     [this](std::string *s, std::string *error_message) {
                       return FillStssEntries(s, error_message);
                     });
  stsz_entries_.Init(sizeof(int32_t) * stsz_entry_count(),
                     [this](std::string *s, std::string *error_message) {
                       return FillStszEntries(s, error_message);
                     });
  return true;
}

bool Mp4SampleTablePieces::FillSttsEntries(std::string *s,
                                           std::string *error_message) const {
  SampleIndexIterator it;
  for (it = begin_; !it.done() && it.start_90k() < desired_end_90k_;
       it.Next()) {
    AppendU32(1, s);

    // The final sample may be shortened to the desired end.
    if (it.end_90k() > desired_end_90k_) {
      auto new_duration = desired_end_90k_ - it.start_90k();
      VLOG(1) << "Shortening final sample duration from " << it.duration_90k()
              << " to " << new_duration;
      AppendU32(new_duration, s);
      break;
    } else {
      AppendU32(it.duration_90k(), s);
    }
  }
  if (it.has_error()) {
    *error_message = it.error();
    return false;
  }
  return true;
}

bool Mp4SampleTablePieces::FillStssEntries(std::string *s,
                                           std::string *error_message) const {
  SampleIndexIterator it;
  uint32_t sample_num = sample_offset_;
  for (it = begin_; !it.done() && it.start_90k() < desired_end_90k_;
       it.Next()) {
    if (it.is_key()) {
      Append32(sample_num, s);
    }
    sample_num++;
  }
  if (it.has_error()) {
    *error_message = it.error();
    return false;
  }
  return true;
}

bool Mp4SampleTablePieces::FillStscEntries(std::string *s,
                                           std::string *error_message) const {
  Append32(sample_offset_, s);
  Append32(frames_, s);
  Append32(sample_entry_index_, s);
  return true;
}

bool Mp4SampleTablePieces::FillStszEntries(std::string *s,
                                           std::string *error_message) const {
  SampleIndexIterator it;
  for (it = begin_; !it.done() && it.start_90k() < desired_end_90k_;
       it.Next()) {
    Append32(it.bytes(), s);
  }
  if (it.has_error()) {
    *error_message = it.error();
    return false;
  }
  return true;
}

}  // namespace internal

Mp4FileBuilder &Mp4FileBuilder::Append(Recording &&recording,
                                       int32_t rel_start_90k,
                                       int32_t rel_end_90k) {
  std::unique_ptr<Mp4FileSegment> s(new Mp4FileSegment);
  s->recording = std::move(recording);
  s->rel_start_90k = rel_start_90k;
  s->rel_end_90k = rel_end_90k;
  segments_.push_back(std::move(s));
  return *this;
}

Mp4FileBuilder &Mp4FileBuilder::SetSampleEntry(const VideoSampleEntry &entry) {
  video_sample_entry_ = entry;
  return *this;
}

std::shared_ptr<VirtualFile> Mp4FileBuilder::Build(std::string *error_message) {
  int32_t sample_offset = 1;
  for (auto &segment : segments_) {
    if (segment->recording.video_sample_entry_id != video_sample_entry_.id) {
      *error_message = StrCat(
          "inconsistent video sample entries. builder has: ",
          video_sample_entry_.id, " (sha1 ", ToHex(video_sample_entry_.sha1),
          ", segment has: ", segment->recording.video_sample_entry_id);
      return std::shared_ptr<VirtualFile>();
    }

    if (!segment->pieces.Init(&segment->recording,
                              1,  // sample entry index
                              sample_offset, segment->rel_start_90k,
                              segment->rel_end_90k, error_message)) {
      return std::shared_ptr<VirtualFile>();
    }
    sample_offset += segment->pieces.samples();
  }

  if (segments_.empty()) {
    *error_message = "Can't construct empty .mp4";
    return std::shared_ptr<VirtualFile>();
  }

  return std::shared_ptr<VirtualFile>(new Mp4File(
      sample_file_dir_, std::move(segments_), std::move(video_sample_entry_),
      include_timestamp_subtitle_track_));
}

}  // namespace moonfire_nvr
