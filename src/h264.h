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
// understand the video codec. There's one exception. It must construct the
// .mp4 sample description table, and for AVC, this includes the ISO/IEC
// 14496-15 section 5.2.4.1 AVCDecoderConfigurationRecord. ffmpeg supplies (as
// "extra data") an ISO/IEC 14496-10 Annex B byte stream containing SPS
// (sequence parameter set) and PPS (picture parameter set) NAL units from
// which this can be constructed.
//
// ffmpeg of course also has logic for converting "extra data" to the
// AVCDecoderConfigurationRecord, but unfortunately it is not exposed except
// through ffmpeg's own generated .mp4 file. Extracting just this part of
// their .mp4 files would be more trouble than it's worth.

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
// VisualSampleEntry), given the "extra_data", width, and height supplied by
// ffmpeg.
bool GetH264SampleEntry(re2::StringPiece extra_data, uint16_t width,
                        uint16_t height, std::string *out,
                        std::string *error_message);

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_H264_H
