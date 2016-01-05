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
// recording.h: Write and read recordings. See design/schema.md for a
// description of the storage schema.

#ifndef MOONFIRE_NVR_RECORDING_H
#define MOONFIRE_NVR_RECORDING_H

#include <stdint.h>

#include <string>

#include <glog/logging.h>
#include <re2/stringpiece.h>

namespace moonfire_nvr {

// Encodes a sample index.
class SampleIndexEncoder {
 public:
  SampleIndexEncoder() { Clear(); }
  void AddSample(int32_t duration_90k, int32_t bytes, bool is_key);
  void Clear();

  // Return the current data, which is invalidated by the next call to
  // AddSample() or Clear().
  re2::StringPiece data() { return data_; }

 private:
  std::string data_;
  int32_t prev_duration_90k_;
  int32_t prev_bytes_key_;
  int32_t prev_bytes_nonkey_;
};

// Iterates through an encoded index, decoding on the fly. Copyable.
// Example usage:
//
// SampleIndexIterator it;
// for (it = index; !it.done(); it.Next()) {
//   LOG(INFO) << "sample size: " << it.bytes();
// }
// if (it.has_error()) {
//   LOG(ERROR) << "error: " << it.error();
// }
class SampleIndexIterator {
 public:
  SampleIndexIterator() { Clear(); }

  // |index| must outlive the iterator.
  explicit SampleIndexIterator(re2::StringPiece index) {
    Clear();
    data_ = index;
    Next();
  }

  // Iteration control.
  void Next();
  bool done() const { return done_; }
  bool has_error() const { return !error_.empty(); }
  const std::string &error() const { return error_; }

  // Return properties of the current sample.
  // Note pos() and start_90k() are valid when done(); the others are not.
  int64_t pos() const { return pos_; }
  int32_t start_90k() const { return start_90k_; }
  int32_t duration_90k() const {
    DCHECK(!done_);
    return duration_90k_;
  }
  int32_t end_90k() const { return start_90k_ + duration_90k(); }
  int32_t bytes() const {
    DCHECK(!done_);
    return bytes_internal();
  }
  bool is_key() const {
    DCHECK(!done_);
    return is_key_;
  }

 private:
  void Clear();

  // Return the bytes taken by the current sample, or 0 after Clear().
  int64_t bytes_internal() const {
    return is_key_ ? bytes_key_ : bytes_nonkey_;
  }

  re2::StringPiece data_;
  std::string error_;
  int64_t pos_;
  int32_t start_90k_;
  int32_t duration_90k_;
  int32_t bytes_key_;
  int32_t bytes_nonkey_;
  bool is_key_;
  bool done_;
};

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_RECORDING_H
