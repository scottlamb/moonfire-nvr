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
// mp4.h: interface for building VirtualFiles representing ISO/IEC 14496-12
// (ISO base media format / MPEG-4 / .mp4) video. These can be constructed
// from one or more recordings and are suitable for HTTP range serving or
// download.

#ifndef MOONFIRE_NVR_MP4_H
#define MOONFIRE_NVR_MP4_H

#include <memory>
#include <vector>

#include "recording.h"
#include "http.h"

namespace moonfire_nvr {

namespace internal {

// Represents pieces of .mp4 sample tables for one recording. Many recordings,
// and thus many of these objects, may be spliced together into a single
// virtual .mp4 file. For internal use by Mp4FileBuilder. Exposed for testing.
class Mp4SampleTablePieces {
 public:
  Mp4SampleTablePieces() {}
  Mp4SampleTablePieces(const Mp4SampleTablePieces &) = delete;
  void operator=(const Mp4SampleTablePieces &) = delete;

  // |recording| must outlive the Mp4SampleTablePieces.
  //
  // |sample_entry_index| should be the (1-based) index into the "stsd" box
  // of an entry matching this recording's video_sample_entry_sha1. It may
  // be shared with other recordings.
  //
  // |sample_offset| should be the (1-based) index of the first sample in
  // this file. It should be 1 + the sum of all previous Mp4SampleTablePieces'
  // samples() values.
  //
  // |start_90k| and |end_90k| should be relative to the start of the recording.
  // They indicate the *desired* time range. The *actual* time range will be
  // from the last sync sample <= |start_90k| to the last sample with start time
  // <= |end_90k|. TODO: support edit lists and duration trimming to produce
  // the exact correct time range.
  bool Init(const Recording *recording, int sample_entry_index,
            int32_t sample_offset, int32_t start_90k, int32_t end_90k,
            std::string *error_message);

  int32_t stts_entry_count() const { return frames_; }
  const FileSlice *stts_entries() const { return &stts_entries_; }

  int32_t stss_entry_count() const { return key_frames_; }
  const FileSlice *stss_entries() const { return &stss_entries_; }

  int32_t stsz_entry_count() const { return frames_; }
  const FileSlice *stsz_entries() const { return &stsz_entries_; }

  int32_t samples() const { return frames_; }

  // Return the byte range in the sample file of the frames represented here.
  ByteRange sample_pos() const { return sample_pos_; }

  uint64_t duration_90k() const { return actual_end_90k_ - begin_.start_90k(); }

  int32_t start_90k() const { return begin_.start_90k(); }
  int32_t end_90k() const { return actual_end_90k_; }

 private:
  bool FillSttsEntries(std::string *s, std::string *error_message) const;
  bool FillStssEntries(std::string *s, std::string *error_message) const;
  bool FillStscEntries(std::string *s, std::string *error_message) const;
  bool FillStszEntries(std::string *s, std::string *error_message) const;

  // After Init(), |begin_| will be on the first sample after the start of the
  // range (or it will be done()).
  SampleIndexIterator begin_;

  ByteRange sample_pos_;

  FillerFileSlice stts_entries_;
  FillerFileSlice stss_entries_;
  FillerFileSlice stsz_entries_;

  int sample_entry_index_ = -1;
  int32_t sample_offset_ = -1;
  int32_t desired_end_90k_ = -1;
  int32_t actual_end_90k_ = -1;
  int32_t frames_ = 0;
  int32_t key_frames_ = 0;
};

struct Mp4FileSegment {
  Recording recording;
  Mp4SampleTablePieces pieces;
  RealFileSlice sample_file_slice;
  int32_t rel_start_90k = 0;
  int32_t rel_end_90k = std::numeric_limits<int32_t>::max();
};

}  // namespace internal

// Builder for a virtual .mp4 file.
class Mp4FileBuilder {
 public:
  // |sample_file_dir| must outlive the Mp4FileBuilder and the returned
  // VirtualFile.
  explicit Mp4FileBuilder(File *sample_file_dir)
      : sample_file_dir_(sample_file_dir) {}
  Mp4FileBuilder(const Mp4FileBuilder &) = delete;
  void operator=(const Mp4FileBuilder &) = delete;

  // Append part or all of a recording.
  // Note that |recording.video_sample_entry_sha1| must be added via
  // AddSampleEntry.
  Mp4FileBuilder &Append(Recording &&recording, int32_t rel_start_300ths,
                         int32_t rel_end_300ths);

  // TODO: support multiple sample entries?
  Mp4FileBuilder &SetSampleEntry(const VideoSampleEntry &entry);

  // Set if a subtitle track should be added with timestamps.
  Mp4FileBuilder &include_timestamp_subtitle_track(bool v) {
    include_timestamp_subtitle_track_ = v;
    return *this;
  }

  // TODO: MPEG-DASH / ISO BMFF Byte Stream Format support.

  // Build the .mp4 file, returning it to the caller.
  // The Mp4FileBuilder is left in an undefined state; it should not
  // be used afterward. On error, nullptr is returned, with |error_message|
  // populated.
  //
  // Errors include:
  // * TODO: width/height mismatch? or is this okay?
  // * No segments.
  // * Non-final segment has zero duration of last sample.
  // * Data error in one of the recording sample indexes.
  // * Invalid start/end.
  std::shared_ptr<VirtualFile> Build(std::string *error_message);

 private:
  File *sample_file_dir_;
  std::vector<std::unique_ptr<internal::Mp4FileSegment>> segments_;
  VideoSampleEntry video_sample_entry_;
  bool include_timestamp_subtitle_track_ = false;
};

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_MP4_H
