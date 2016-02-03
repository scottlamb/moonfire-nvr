// This file is part of Moonfire NVR, a security camera network video recorder.
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
// h264.h: H.264 decoding. For the most part, Moonfire NVR does not try to
// understand the video codec. However, H.264 has two byte stream encodings:
// ISO/IEC 14496-10 Annex B, and ISO/IEC 14496-15 AVC access units.
// When streaming from RTSP, ffmpeg supplies the former. We need the latter
// to stick into .mp4 files. This file manages the conversion, both for
// the ffmpeg "extra data" (which should become the ISO/IEC 14496-15
// section 5.2.4.1 AVCDecoderConfigurationRecord) and the actual samples.
//
// ffmpeg of course has logic to do the same thing, but unfortunately it is
// not exposed except through ffmpeg's own generated .mp4 file. Extracting
// just this part of their .mp4 files would be more trouble than it's worth.

#ifndef MOONFIRE_NVR_H264_H
#define MOONFIRE_NVR_H264_H

#include <functional>
#include <string>

#include <re2/stringpiece.h>

#include "common.h"

namespace moonfire_nvr {

namespace internal {

using NalUnitFunction =
    std::function<IterationControl(re2::StringPiece nal_unit)>;

// Decode a H.264 Annex B byte stream into NAL units.
// For GetH264SampleEntry; exposed for testing.
// Calls |process_nal_unit| for each NAL unit in the byte stream.
//
// Note: this won't spot all invalid byte streams. For example, several 0x00s
// not followed by a 0x01 will just be considered part of a NAL unit rather
// than proof of an invalid stream.
bool DecodeH264AnnexB(re2::StringPiece data, NalUnitFunction process_nal_unit,
                      std::string *error_message);

}  // namespace

// Gets a H.264 sample entry (AVCSampleEntry, which extends
// VisualSampleEntry), given the "extradata", width, and height supplied by
// ffmpeg.
bool ParseExtraData(re2::StringPiece extradata, uint16_t width, uint16_t height,
                    std::string *sample_entry, bool *need_transform,
                    std::string *error_message);

bool TransformSampleData(re2::StringPiece annexb_sample,
                         std::string *avc_sample, std::string *error_message);

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_H264_H
