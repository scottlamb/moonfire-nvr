// This file is part of Moonfire DVR, a security camera digital video recorder.
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
// mp4_test.cc: tests of the mp4.h interface.

#include <fcntl.h>
#include <sys/stat.h>
#include <sys/types.h>

#include <gflags/gflags.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "ffmpeg.h"
#include "h264.h"
#include "http.h"
#include "mp4.h"
#include "string.h"
#include "testutil.h"

DECLARE_bool(alsologtostderr);

using moonfire_nvr::internal::Mp4SampleTablePieces;

namespace moonfire_nvr {
namespace {

std::string ToHex(const FileSlice *slice) {
  EvBuffer buf;
  std::string error_message;
  size_t size = slice->size();
  CHECK(slice->AddRange(ByteRange(0, size), &buf, &error_message))
      << error_message;
  CHECK_EQ(size, evbuffer_get_length(buf.get()));
  return ::moonfire_nvr::ToHex(re2::StringPiece(
      reinterpret_cast<const char *>(evbuffer_pullup(buf.get(), size)), size));
}

TEST(Mp4SampleTablePiecesTest, Stts) {
  SampleIndexEncoder encoder;
  for (int i = 1; i <= 5; ++i) {
    encoder.AddSample(i, 2 * i, true);
  }

  Mp4SampleTablePieces pieces;
  std::string error_message;
  // Time range [1, 1 + 2 + 3 + 4) means the 2nd, 3rd, 4th samples should be
  // included.
  ASSERT_TRUE(
      pieces.Init(encoder.data(), 2, 10, 1, 1 + 2 + 3 + 4, &error_message))
      << error_message;
  EXPECT_EQ(3, pieces.stts_entry_count());
  const char kExpectedEntries[] =
      "00 00 00 01 00 00 00 02 "
      "00 00 00 01 00 00 00 03 "
      "00 00 00 01 00 00 00 04";
  EXPECT_EQ(kExpectedEntries, ToHex(pieces.stts_entries()));
}

TEST(Mp4SampleTablePiecesTest, SttsAfterSyncSample) {
  SampleIndexEncoder encoder;
  for (int i = 1; i <= 5; ++i) {
    encoder.AddSample(i, 2 * i, i == 1);
  }

  Mp4SampleTablePieces pieces;
  std::string error_message;
  // Because only the 1st frame is a sync sample, it will be included also.
  ASSERT_TRUE(
      pieces.Init(encoder.data(), 2, 10, 1, 1 + 2 + 3 + 4, &error_message))
      << error_message;
  EXPECT_EQ(4, pieces.stts_entry_count());
  const char kExpectedEntries[] =
      "00 00 00 01 00 00 00 01 "
      "00 00 00 01 00 00 00 02 "
      "00 00 00 01 00 00 00 03 "
      "00 00 00 01 00 00 00 04";
  EXPECT_EQ(kExpectedEntries, ToHex(pieces.stts_entries()));
}

TEST(Mp4SampleTablePiecesTest, Stss) {
  SampleIndexEncoder encoder;
  encoder.AddSample(1, 1, true);
  encoder.AddSample(1, 1, false);
  encoder.AddSample(1, 1, true);
  encoder.AddSample(1, 1, false);
  Mp4SampleTablePieces pieces;
  std::string error_message;
  ASSERT_TRUE(pieces.Init(encoder.data(), 2, 10, 0, 4, &error_message))
      << error_message;
  EXPECT_EQ(2, pieces.stss_entry_count());
  const char kExpectedSampleNumbers[] = "00 00 00 0a 00 00 00 0c";
  EXPECT_EQ(kExpectedSampleNumbers, ToHex(pieces.stss_entries()));
}

TEST(Mp4SampleTablePiecesTest, Stsc) {
  SampleIndexEncoder encoder;
  encoder.AddSample(1, 1, true);
  encoder.AddSample(1, 1, false);
  encoder.AddSample(1, 1, true);
  encoder.AddSample(1, 1, false);
  Mp4SampleTablePieces pieces;
  std::string error_message;
  ASSERT_TRUE(pieces.Init(encoder.data(), 2, 10, 0, 4, &error_message))
      << error_message;
  EXPECT_EQ(1, pieces.stsc_entry_count());
  const char kExpectedEntries[] = "00 00 00 0a 00 00 00 04 00 00 00 02";
  EXPECT_EQ(kExpectedEntries, ToHex(pieces.stsc_entries()));
}

TEST(Mp4SampleTablePiecesTest, Stsz) {
  SampleIndexEncoder encoder;
  for (int i = 1; i <= 5; ++i) {
    encoder.AddSample(i, 2 * i, true);
  }

  Mp4SampleTablePieces pieces;
  std::string error_message;
  // Time range [1, 1 + 2 + 3 + 4) means the 2nd, 3rd, 4th samples should be
  // included.
  ASSERT_TRUE(
      pieces.Init(encoder.data(), 2, 10, 1, 1 + 2 + 3 + 4, &error_message))
      << error_message;
  EXPECT_EQ(3, pieces.stsz_entry_count());
  const char kExpectedEntries[] = "00 00 00 04 00 00 00 06 00 00 00 08";
  EXPECT_EQ(kExpectedEntries, ToHex(pieces.stsz_entries()));
}

class IntegrationTest : public testing::Test {
 protected:
  IntegrationTest() {
    tmpdir_path_ = PrepareTempDirOrDie("mp4-integration-test");
    int ret =
        GetRealFilesystem()->Open(tmpdir_path_.c_str(), O_RDONLY, &tmpdir_);
    CHECK_EQ(0, ret) << strerror(ret);
  }

  void CopyMp4ToSingleRecording() {
    std::string error_message;
    SampleIndexEncoder index;
    SampleFileWriter writer(tmpdir_.get());
    recording_.sample_file_path = StrCat(tmpdir_path_, "/clip.sample");
    if (!writer.Open("clip.sample", &error_message)) {
      ADD_FAILURE() << "open clip.sample: " << error_message;
      return;
    }
    auto in = GetRealVideoSource()->OpenFile("../src/testdata/clip.mp4",
                                             &error_message);
    if (in == nullptr) {
      ADD_FAILURE() << "open clip.mp4" << error_message;
      return;
    }

    video_sample_entry_.width = in->stream()->codec->width;
    video_sample_entry_.height = in->stream()->codec->height;
    if (!GetH264SampleEntry(GetExtradata(in.get()), in->stream()->codec->width,
                            in->stream()->codec->height,
                            &video_sample_entry_.data, &error_message)) {
      ADD_FAILURE() << "GetH264SampleEntry: " << error_message;
      return;
    }

    while (true) {
      VideoPacket pkt;
      if (!in->GetNext(&pkt, &error_message)) {
        if (!error_message.empty()) {
          ADD_FAILURE() << "GetNext: " << error_message;
          return;
        }
        break;
      }
      if (!writer.Write(GetData(pkt), &error_message)) {
        ADD_FAILURE() << "Write: " << error_message;
        return;
      }
      index.AddSample(pkt.pkt()->duration, pkt.pkt()->size, pkt.is_key());
    }

    if (!writer.Close(&recording_.sample_file_sha1, &error_message)) {
      ADD_FAILURE() << "Close: " << error_message;
    }
    recording_.video_index = index.data().as_string();
  }

  void CopySingleRecordingToNewMp4() {
    Mp4FileBuilder builder;
    builder.SetSampleEntry(video_sample_entry_);
    builder.Append(Recording(recording_), 0,
                   std::numeric_limits<int32_t>::max());
    std::string error_message;
    auto mp4 = builder.Build(&error_message);
    ASSERT_TRUE(mp4 != nullptr) << error_message;
    EvBuffer buf;
    ASSERT_TRUE(mp4->AddRange(ByteRange(0, mp4->size()), &buf, &error_message))
        << error_message;
    WriteFileOrDie(StrCat(tmpdir_path_, "/clip.new.mp4"), &buf);
  }

  void CompareMp4s() {
    std::string error_message;
    auto original = GetRealVideoSource()->OpenFile("../src/testdata/clip.mp4",
                                                   &error_message);
    ASSERT_TRUE(original != nullptr) << error_message;
    auto copied = GetRealVideoSource()->OpenFile(
        StrCat(tmpdir_path_, "/clip.new.mp4"), &error_message);
    ASSERT_TRUE(copied != nullptr) << error_message;

    EXPECT_EQ(GetExtradata(original.get()), GetExtradata(copied.get()));
    EXPECT_EQ(original->stream()->codec->width, copied->stream()->codec->width);
    EXPECT_EQ(original->stream()->codec->height,
              copied->stream()->codec->height);

    while (true) {
      VideoPacket original_pkt;
      VideoPacket copied_pkt;

      bool original_has_next = original->GetNext(&original_pkt, &error_message);
      ASSERT_TRUE(original_has_next || error_message.empty()) << error_message;
      bool copied_has_next = copied->GetNext(&copied_pkt, &error_message);
      ASSERT_TRUE(copied_has_next || error_message.empty()) << error_message;
      if (!original_has_next && !copied_has_next) {
        break;
      }
      ASSERT_TRUE(original_has_next);
      ASSERT_TRUE(copied_has_next);
      EXPECT_EQ(original_pkt.pkt()->pts, copied_pkt.pkt()->pts);
      EXPECT_EQ(original_pkt.pkt()->duration, copied_pkt.pkt()->duration);
      EXPECT_EQ(GetData(original_pkt), GetData(copied_pkt));
    }
  }

  re2::StringPiece GetExtradata(InputVideoPacketStream *stream) {
    return re2::StringPiece(
        reinterpret_cast<const char *>(stream->stream()->codec->extradata),
        stream->stream()->codec->extradata_size);
  }

  re2::StringPiece GetData(const VideoPacket &pkt) {
    return re2::StringPiece(reinterpret_cast<const char *>(pkt.pkt()->data),
                            pkt.pkt()->size);
  }

  std::string tmpdir_path_;
  std::unique_ptr<File> tmpdir_;
  Recording recording_;
  VideoSampleEntry video_sample_entry_;
};

TEST_F(IntegrationTest, RoundTrip) {
  CopyMp4ToSingleRecording();
  CopySingleRecordingToNewMp4();
  CompareMp4s();
}

}  // namespace
}  // namespace moonfire_nvr

int main(int argc, char **argv) {
  FLAGS_alsologtostderr = true;
  google::ParseCommandLineFlags(&argc, &argv, true);
  testing::InitGoogleTest(&argc, argv);
  google::InitGoogleLogging(argv[0]);
  return RUN_ALL_TESTS();
}
