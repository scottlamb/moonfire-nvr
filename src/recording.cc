// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2015 Scott Lamb <slamb@slamb.org>
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
// recording.cc: see recording.h.

#include "recording.h"

#include "coding.h"
#include "string.h"

namespace moonfire_nvr {

void SampleIndexEncoder::AddSample(int32_t duration_90k, int32_t bytes,
                                   bool is_key) {
  CHECK_GE(duration_90k, 0);
  CHECK_GT(bytes, 0);
  int32_t duration_delta = duration_90k - prev_duration_90k_;
  prev_duration_90k_ = duration_90k;
  int32_t bytes_delta;
  if (is_key) {
    bytes_delta = bytes - prev_bytes_key_;
    prev_bytes_key_ = bytes;
  } else {
    bytes_delta = bytes - prev_bytes_nonkey_;
    prev_bytes_nonkey_ = bytes;
  }
  uint32_t zigzagged_bytes_delta = Zigzag32(bytes_delta);
  AppendVar32((Zigzag32(duration_delta) << 1) | is_key, &data_);
  AppendVar32(zigzagged_bytes_delta, &data_);
}

void SampleIndexEncoder::Clear() {
  data_.clear();
  prev_duration_90k_ = 0;
  prev_bytes_key_ = 0;
  prev_bytes_nonkey_ = 0;
}

void SampleIndexIterator::Next() {
  uint32_t raw1;
  uint32_t raw2;
  pos_ += bytes_internal();
  if (data_.empty() || !DecodeVar32(&data_, &raw1, &error_) ||
      !DecodeVar32(&data_, &raw2, &error_)) {
    done_ = true;
    return;
  }
  start_90k_ += duration_90k_;
  int32_t duration_90k_delta = Unzigzag32(raw1 >> 1);
  duration_90k_ += duration_90k_delta;
  if (duration_90k_ < 0) {
    error_ = StrCat("negative duration ", duration_90k_,
                    " after applying delta ", duration_90k_delta);
    done_ = true;
    return;
  }
  if (duration_90k_ == 0 && !data_.empty()) {
    error_ = StrCat("zero duration only allowed at end; have ", data_.size(),
                    "bytes left.");
    done_ = true;
    return;
  }
  is_key_ = raw1 & 0x01;
  int32_t bytes_delta = Unzigzag32(raw2);
  if (is_key_) {
    bytes_key_ += bytes_delta;
  } else {
    bytes_nonkey_ += bytes_delta;
  }
  if (bytes_internal() <= 0) {
    error_ = StrCat("non-positive bytes ", bytes_internal(),
                    " after applying delta ", bytes_delta, " to ",
                    (is_key_ ? "key" : "non-key"), " frame at ts ", start_90k_);
    done_ = true;
    return;
  }
  done_ = false;
  return;
}

void SampleIndexIterator::Clear() {
  data_.clear();
  error_.clear();
  pos_ = 0;
  start_90k_ = 0;
  duration_90k_ = 0;
  bytes_key_ = 0;
  bytes_nonkey_ = 0;
  is_key_ = false;
  done_ = true;
}

}  // namespace moonfire_nvr
