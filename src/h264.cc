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
// h264.cc: see h264.h.

#include "h264.h"

#include <re2/re2.h>

#include "coding.h"
#include "string.h"

namespace moonfire_nvr {

namespace {

// See ISO/IEC 14496-10 table 7-1 - NAL unit type codes, syntax element
// categories, and NAL unit type classes.
const int kNalUnitSeqParameterSet = 7;
const int kNalUnitPicParameterSet = 8;

// Parse sequence parameter set and picture parameter set from ffmpeg's
// "extra_data".
bool ParseExtraData(re2::StringPiece extra_data, std::string *sps,
                    std::string *pps, std::string *error_message) {
  bool ok = true;
  internal::NalUnitFunction fn = [&ok, sps, pps,
                                  error_message](re2::StringPiece nal_unit) {
    // See ISO/IEC 14496-10 section 7.3.1, which defines nal_unit.
    uint8_t nal_type = nal_unit[0] & 0x1F;  // bottom 5 bits of first byte.
    switch (nal_type) {
      case kNalUnitSeqParameterSet:
        *sps = nal_unit.as_string();
        break;
      case kNalUnitPicParameterSet:
        *pps = nal_unit.as_string();
        break;
      default:
        *error_message =
            StrCat("Expected only SPS and PPS; got type ", nal_type);
        ok = false;
        return IterationControl::kBreak;
    }
    return IterationControl::kContinue;
  };
  if (!internal::DecodeH264AnnexB(extra_data, fn, error_message) || !ok) {
    return false;
  }
  if (sps->empty() || pps->empty()) {
    *error_message = "SPS and PPS must be specified.";
    return false;
  }
  return true;
}

}  // namespace

namespace internal {

// See ISO/IEC 14496-10 section B.2: Byte stream NAL unit decoding process.
// This is a relatively simple, unoptimized implementation given that it
// only processes a few dozen bytes per recording.
bool DecodeH264AnnexB(re2::StringPiece data, NalUnitFunction process_nal_unit,
                      std::string *error_message) {
  static const RE2 kStartCode("(\\x00{2,}\\x01)");

  if (!RE2::Consume(&data, kStartCode)) {
    *error_message = "stream does not start with Annex B start code";
    return false;
  }

  while (!data.empty()) {
    // Now at the start of a NAL unit. Find the end.
    re2::StringPiece next_start;
    re2::StringPiece this_nal = data;
    if (RE2::FindAndConsume(&data, kStartCode, &next_start)) {
      // It ends where another start code is found.
      this_nal = re2::StringPiece(this_nal.data(),
                                  next_start.data() - this_nal.data());
    } else {
      // It ends at the end of |data|. |this_nal| is already correct.
      // Set |data| to be empty so the while loop exits after this iteration.
      data = re2::StringPiece();
    }

    if (this_nal.empty()) {
      *error_message = "NAL unit can't be empty";
      return false;
    }

    if (process_nal_unit(this_nal) == IterationControl::kBreak) {
      break;
    }
  }
  return true;
}

}  // namespace internal

bool GetH264SampleEntry(re2::StringPiece extra_data, uint16_t width,
                        uint16_t height, std::string *out,
                        std::string *error_message) {
  std::string sps;
  std::string pps;
  if (!ParseExtraData(extra_data, &sps, &pps, error_message)) {
    return false;
  }

  // These match the size of all fields below.
  // Don't panic; they're verified at the end.
  uint32_t avcc_len = 19 + sps.size() + pps.size();
  uint32_t avc1_len = 86 + avcc_len;

  // This is a concatenation of the following boxes/classes.
  // SampleEntry, ISO/IEC 14496-10 section 8.5.2.
  uint32_t avc1_len_pos = out->size();
  AppendU32(avc1_len, out);  // length
  out->append("avc1");       // type
  out->append(6, '\x00');    // reserved
  AppendU16(1, out);         // data_reference_index = 1

  // VisualSampleEntry, ISO/IEC 14496-12 section 12.1.3.
  out->append(16, '\x00');  // pre_defined + reserved
  AppendU16(width, out);
  AppendU16(height, out);
  AppendU32(UINT32_C(0x00480000), out);  // horizresolution
  AppendU32(UINT32_C(0x00480000), out);  // vertresolution
  AppendU32(0, out);                     // reserved
  AppendU16(1, out);                     // frame count
  out->append(32, '\x00');               // compressorname
  AppendU16(0x0018, out);                // depth
  Append16(-1, out);                     // pre_defined

  // AVCSampleEntry, ISO/IEC 14496-15 section 5.3.4.1.
  // AVCConfigurationBox, ISO/IEC 14496-15 section 5.3.4.1.
  uint32_t avcc_len_pos = out->size();
  AppendU32(avcc_len, out);  // length
  out->append("avcC");       // type

  // AVCDecoderConfiguration, ISO/IEC 14496-15 section 5.2.4.1.
  // The beginning of the AVCDecoderConfiguration takes a few values from
  // the SPS (ISO/IEC 14496-10 section 7.3.2.1.1). One caveat: that section
  // defines the syntax in terms of RBSP, not NAL. The difference is the
  // escaping of 00 00 01 and 00 00 02; see notes about
  // "emulation_prevention_three_byte" in ISO/IEC 14496-10 section 7.4.
  // It looks like 00 is not a valid value of profile_idc, so this distinction
  // shouldn't be relevant here. And ffmpeg seems to ignore it.
  out->push_back(1);       // configurationVersion
  out->push_back(sps[1]);  // profile_idc -> AVCProfileIndication
  out->push_back(sps[2]);  // ...misc bits... -> profile_compatibility
  out->push_back(sps[3]);  // level_idc -> AVCLevelIndication

  // Hardcode lengthSizeMinusOne to 3. This needs to match what ffmpeg uses
  // when generating AVCParameterSamples (ISO/IEC 14496-15 section 5.3.2).
  // There doesn't seem to be a clean way to get this from ffmpeg, but it's
  // always 3.
  out->push_back(static_cast<char>(0xff));

  // Only support one SPS and PPS.
  // ffmpeg's ff_isom_write_avcc has the same limitation, so it's probably fine.
  // This next byte is a reserved 0b111 + a 5-bit # of SPSs (1).
  out->push_back(static_cast<char>(0xe1));
  AppendU16(sps.size(), out);
  out->append(sps.data(), sps.size());
  out->push_back(1);  // # of PPSs.
  AppendU16(pps.size(), out);
  out->append(pps.data(), pps.size());

  if (out->size() - avcc_len_pos != avcc_len) {
    *error_message =
        StrCat("internal error: anticipated AVCConfigurationBox length ",
               avcc_len, ", but was actually ", out->size() - avcc_len_pos,
               "; sps length ", sps.size(), ", pps length ", pps.size());
    return false;
  }
  if (out->size() - avc1_len_pos != avc1_len) {
    *error_message =
        StrCat("internal error: anticipated AVCSampleEntry length ", avc1_len,
               ", but was actually ", out->size() - avc1_len_pos,
               "; sps length ", sps.size(), ", pps length ", pps.size());
    return false;
  }

  return true;
}

}  // namespace moonfire_nvr
