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

#include <fcntl.h>
#include <sys/stat.h>
#include <sys/types.h>

#include "coding.h"
#include "string.h"

namespace moonfire_nvr {

void SampleIndexEncoder::Init(Recording *recording, int64_t start_time_90k) {
  recording_ = recording;
  recording_->start_time_90k = start_time_90k;
  recording_->end_time_90k = start_time_90k;
  recording_->sample_file_bytes = 0;
  recording_->video_samples = 0;
  recording_->video_sync_samples = 0;
  recording_->video_index.clear();
  prev_duration_90k_ = 0;
  prev_bytes_key_ = 0;
  prev_bytes_nonkey_ = 0;
}

void SampleIndexEncoder::AddSample(int32_t duration_90k, int32_t bytes,
                                   bool is_key) {
  CHECK_GE(duration_90k, 0);
  CHECK_GT(bytes, 0);
  int32_t duration_delta = duration_90k - prev_duration_90k_;
  prev_duration_90k_ = duration_90k;
  int32_t bytes_delta;
  recording_->end_time_90k += duration_90k;
  recording_->sample_file_bytes += bytes;
  ++recording_->video_samples;
  if (is_key) {
    bytes_delta = bytes - prev_bytes_key_;
    prev_bytes_key_ = bytes;
    ++recording_->video_sync_samples;
  } else {
    bytes_delta = bytes - prev_bytes_nonkey_;
    prev_bytes_nonkey_ = bytes;
  }
  uint32_t zigzagged_bytes_delta = Zigzag32(bytes_delta);
  AppendVar32((Zigzag32(duration_delta) << 1) | is_key,
              &recording_->video_index);
  AppendVar32(zigzagged_bytes_delta, &recording_->video_index);
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

SampleFileWriter::SampleFileWriter(File *parent_dir)
    : parent_dir_(parent_dir), sha1_(Digest::SHA1()) {}

bool SampleFileWriter::Open(const char *filename, std::string *error_message) {
  if (is_open()) {
    *error_message = "already open!";
    return false;
  }
  int ret =
      parent_dir_->Open(filename, O_WRONLY | O_CREAT | O_EXCL, 0600, &file_);
  if (ret != 0) {
    *error_message = StrCat("open ", filename, " (within dir ",
                            parent_dir_->name(), "): ", strerror(ret));
    return false;
  }
  return true;
}

bool SampleFileWriter::Write(re2::StringPiece pkt, std::string *error_message) {
  if (!is_open()) {
    *error_message = "not open!";
    return false;
  }
  auto old_pos = pos_;
  re2::StringPiece remaining(pkt);
  while (!remaining.empty()) {
    size_t written;
    int write_ret = file_->Write(remaining, &written);
    if (write_ret != 0) {
      if (pos_ > old_pos) {
        int truncate_ret = file_->Truncate(old_pos);
        if (truncate_ret != 0) {
          *error_message =
              StrCat("write failed with: ", strerror(write_ret),
                     " and ftruncate failed with: ", strerror(truncate_ret));
          corrupt_ = true;
          return false;
        }
      }
      *error_message = StrCat("write: ", strerror(write_ret));
      return false;
    }
    remaining.remove_prefix(written);
    pos_ += written;
  }
  sha1_->Update(pkt);
  return true;
}

bool SampleFileWriter::Close(std::string *sha1, std::string *error_message) {
  if (!is_open()) {
    *error_message = "not open!";
    return false;
  }

  if (corrupt_) {
    *error_message = "File already corrupted.";
  } else {
    int ret = file_->Sync();
    if (ret != 0) {
      *error_message = StrCat("fsync failed with: ", strerror(ret));
      corrupt_ = true;
    }
  }

  int ret = file_->Close();
  if (ret != 0 && !corrupt_) {
    corrupt_ = true;
    *error_message = StrCat("close failed with: ", strerror(ret));
  }

  bool ok = !corrupt_;
  file_.reset();
  *sha1 = sha1_->Finalize();
  sha1_ = Digest::SHA1();
  pos_ = 0;
  corrupt_ = false;
  return ok;
}

std::string PrettyTimestamp(int64_t ts_90k) {
  struct tm mytm;
  memset(&mytm, 0, sizeof(mytm));
  time_t ts = ts_90k / kTimeUnitsPerSecond;
  localtime_r(&ts, &mytm);
  const size_t kTimeBufLen = 50;
  char tmbuf[kTimeBufLen];
  strftime(tmbuf, kTimeBufLen, "%a, %d %b %Y %H:%M:%S %Z", &mytm);
  return tmbuf;
}

}  // namespace moonfire_nvr
