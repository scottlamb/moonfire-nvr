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

#include <memory>
#include <string>

#include <glog/logging.h>
#include <re2/stringpiece.h>

#include "crypto.h"
#include "filesystem.h"
#include "uuid.h"

namespace moonfire_nvr {

constexpr int64_t kTimeUnitsPerSecond = 90000;

// Recordings are never longer than this (5 minutes).
// Having such a limit dramatically speeds up some SQL queries.
// This limit should be more than the normal rotation time,
// as recording doesn't happen until the next key frame.
// 5 minutes is generously more than 1 minute, but still sufficient to
// allow the optimization to be useful. This value must match the CHECK
// constraint on duration_90k in schema.sql.
constexpr int64_t kMaxRecordingDuration = 5 * 60 * kTimeUnitsPerSecond;

// Various fields from the "recording" table which are useful when viewing
// recordings.
struct Recording {
  int64_t id = -1;
  int64_t camera_id = -1;
  std::string sample_file_sha1;
  std::string sample_file_path;
  Uuid sample_file_uuid;
  int64_t video_sample_entry_id = -1;
  int64_t local_time_90k = -1;

  // Fields populated by SampleIndexEncoder.
  int64_t start_time_90k = -1;
  int64_t end_time_90k = -1;
  int64_t sample_file_bytes = -1;
  int64_t video_samples = -1;
  int64_t video_sync_samples = -1;
  std::string video_index;
};

// Reusable object to encode sample index data to a Recording object.
class SampleIndexEncoder {
 public:
  SampleIndexEncoder() {}
  SampleIndexEncoder(const SampleIndexEncoder &) = delete;
  void operator=(const SampleIndexEncoder &) = delete;

  void Init(Recording *recording, int64_t start_time_90k);
  void AddSample(int32_t duration_90k, int32_t bytes, bool is_key);

 private:
  Recording *recording_;
  int32_t prev_duration_90k_ = 0;
  int32_t prev_bytes_key_ = 0;
  int32_t prev_bytes_nonkey_ = 0;
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

// Writes a sample file. Can be used repeatedly. Thread-compatible.
class SampleFileWriter {
 public:
  // |parent_dir| must outlive the writer.
  SampleFileWriter(File *parent_dir);
  SampleFileWriter(const SampleFileWriter &) = delete;
  void operator=(const SampleFileWriter &) = delete;

  // PRE: !is_open().
  bool Open(const char *filename, std::string *error_message);

  // Writes a single packet, returning success.
  // On failure, the stream should be closed. If Close() returns true, the
  // file contains the results of all packets up to (but not including) this
  // one.
  //
  // PRE: is_open().
  bool Write(re2::StringPiece pkt, std::string *error_message);

  // fsync() and close() the stream.
  // Note the caller is still responsible for fsync()ing the parent stream,
  // so that operations can be batched.
  // On success, |sha1| will be filled with the raw SHA-1 hash of the file.
  // On failure, the file should be considered corrupt and discarded.
  //
  // PRE: is_open().
  bool Close(std::string *sha1, std::string *error_message);

  bool is_open() const { return file_ != nullptr; }

 private:
  File *parent_dir_;
  std::unique_ptr<File> file_;
  std::unique_ptr<Digest> sha1_;
  int64_t pos_ = 0;
  bool corrupt_ = false;
};

struct VideoSampleEntry {
  int64_t id = -1;
  std::string sha1;
  std::string data;
  uint16_t width = 0;
  uint16_t height = 0;
};

std::string PrettyTimestamp(int64_t ts_90k);

inline int64_t To90k(const struct timespec &ts) {
  return (ts.tv_sec * kTimeUnitsPerSecond) +
         (ts.tv_nsec * kTimeUnitsPerSecond / 1000000000);
}

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_RECORDING_H
