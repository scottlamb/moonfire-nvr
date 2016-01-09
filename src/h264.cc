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

// See ISO/IEC 14496-10 section 7.1.
const int kNalUnitSeqParameterSet = 7;
const int kNalUnitPicParameterSet = 8;

}  // namespace

// See T-REC-H.264-201003-S||PDF-E.PDF page 325 for byte stream NAL unit
// syntax

// See page 42 for nal_unit.

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

bool ParseH264ExtraData(re2::StringPiece extra_data,
                        std::string *avc_decoder_config,
                        std::string *error_message) {
  std::string sps;
  std::string pps;
  bool ok = true;
  internal::NalUnitFunction fn = [&ok, &sps, &pps,
                                  error_message](re2::StringPiece nal_unit) {
    uint8_t nal_type = nal_unit[0] & 0x1F;  // bottom 5 bits of first byte.
    switch (nal_type) {
      case kNalUnitSeqParameterSet:
        sps = nal_unit.as_string();
        break;
      case kNalUnitPicParameterSet:
        pps = nal_unit.as_string();
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
  if (sps.empty() || pps.empty()) {
    *error_message = "SPS and PPS must be specified.";
    return false;
  }
  if (sps.size() < 4) {
    *error_message = "SPS record is too short.";
    return false;
  }
  if (sps.size() > std::numeric_limits<uint16_t>::max() ||
      pps.size() > std::numeric_limits<uint16_t>::max()) {
    *error_message = "SPS or PPS is too long.";
    return false;
  }

  // The beginning of the AVCDecoderConfiguration takes a few values from
  // the SPS (ISO/IEC 14496-10 section 7.3.2.1.1). One caveat: that section
  // defines the syntax in terms of RBSP, not NAL. The difference is the
  // escaping of 00 00 01 and 00 00 02; see notes about
  // "emulation_prevention_three_byte" in ISO/IEC 14496-10 section 7.4.
  // It looks like 00 is not a valid value of profile_idc, so this distinction
  // shouldn't be relevant here. And ffmpeg seems to ignore it.
  avc_decoder_config->clear();
  avc_decoder_config->push_back(1);       // configurationVersion
  avc_decoder_config->push_back(sps[1]);  // profile_idc -> AVCProfileIndication
  avc_decoder_config->push_back(sps[2]);  // ... -> profile_compatibility
  avc_decoder_config->push_back(sps[3]);  // level_idc -> AVCLevelIndication

  // Hardcode lengthSizeMinusOne to 3. This needs to match what ffmpeg uses
  // when generating AVCParameterSamples (ISO/IEC 14496-15 section 5.3.2).
  // There doesn't seem to be a clean way to get this from ffmpeg, but it's
  // always 3.
  avc_decoder_config->push_back(static_cast<char>(0xff));

  // Only support one SPS and PPS.
  // ffmpeg's ff_isom_write_avcc has the same limitation, so it's probably fine.
  // This next byte is a reserved 0b111 + a 5-bit # of SPSs (1).
  avc_decoder_config->push_back(static_cast<char>(0xe1));
  AppendU16(sps.size(), avc_decoder_config);
  avc_decoder_config->append(sps.data(), sps.size());
  avc_decoder_config->push_back(1);  // # of PPSs.
  AppendU16(pps.size(), avc_decoder_config);
  avc_decoder_config->append(pps.data(), pps.size());

  return true;
}

}  // namespace moonfire_nvr
