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

#include "mp4.h"

#include "coding.h"

namespace moonfire_nvr {

namespace internal {

bool Mp4SampleTablePieces::Init(re2::StringPiece video_index_blob,
                                int sample_entry_index, int32_t sample_offset,
                                int32_t start_90k, int32_t end_90k,
                                std::string *error_message) {
  video_index_blob_ = video_index_blob;
  sample_entry_index_ = sample_entry_index;
  sample_offset_ = sample_offset;
  desired_end_90k_ = end_90k;
  SampleIndexIterator it = SampleIndexIterator(video_index_blob_);
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
  if (it.has_error()) {
    *error_message = it.error();
    return false;
  }
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
  stsc_entries_.Init(3 * sizeof(int32_t) * stsc_entry_count(),
                     [this](std::string *s, std::string *error_message) {
                       return FillStscEntries(s, error_message);
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
    AppendU32(it.duration_90k(), s);
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

}  // namespace moonfire_nvr
