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
// mp4-test.cc: tests of the mp4.h interface.

#include <fcntl.h>
#include <sys/stat.h>
#include <sys/types.h>

#include <event2/buffer.h>
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

std::string ToHex(const FileSlice *slice, bool pad) {
  EvBuffer buf;
  std::string error_message;
  size_t size = slice->size();
  CHECK(slice->AddRange(ByteRange(0, size), &buf, &error_message))
      << error_message;
  CHECK_EQ(size, evbuffer_get_length(buf.get()));
  return ::moonfire_nvr::ToHex(
      re2::StringPiece(
          reinterpret_cast<const char *>(evbuffer_pullup(buf.get(), size)),
          size),
      pad);
}

std::string Digest(const FileSlice *slice) {
  EvBuffer buf;
  std::string error_message;
  ByteRange left(0, slice->size());
  while (left.size() > 0) {
    auto ret = slice->AddRange(left, &buf, &error_message);
    CHECK_GT(ret, 0) << error_message;
    left.begin += ret;
  }
  evbuffer_iovec vec;
  auto digest = Digest::SHA1();
  while (evbuffer_peek(buf.get(), -1, nullptr, &vec, 1) > 0) {
    digest->Update(re2::StringPiece(
        reinterpret_cast<const char *>(vec.iov_base), vec.iov_len));
    evbuffer_drain(buf.get(), vec.iov_len);
  }
  return ::moonfire_nvr::ToHex(digest->Finalize());
}

TEST(Mp4SampleTablePiecesTest, AllSyncFrames) {
  Recording recording;
  SampleIndexEncoder encoder;
  encoder.Init(&recording, 42);
  for (int i = 1; i <= 5; ++i) {
    int64_t sample_duration_90k = 2 * i;
    int64_t sample_bytes = 3 * i;
    encoder.AddSample(sample_duration_90k, sample_bytes, true);
  }

  Mp4SampleTablePieces pieces;
  std::string error_message;
  // Time range [2, 2 + 4 + 6 + 8) means the 2nd, 3rd, 4th samples should be
  // included.
  ASSERT_TRUE(pieces.Init(&recording, 2, 10, 2, 2 + 4 + 6 + 8, &error_message))
      << error_message;

  EXPECT_EQ(3, pieces.stts_entry_count());
  const char kExpectedStts[] =
      "00 00 00 01 00 00 00 04 "  // run length / timestamps.
      "00 00 00 01 00 00 00 06 "
      "00 00 00 01 00 00 00 08";
  EXPECT_EQ(kExpectedStts, ToHex(pieces.stts_entries(), true));

  // Initial index "10" as given above.
  EXPECT_EQ(3, pieces.stss_entry_count());
  const char kExpectedStss[] = "00 00 00 0a 00 00 00 0b 00 00 00 0c";
  EXPECT_EQ(kExpectedStss, ToHex(pieces.stss_entries(), true));

  EXPECT_EQ(3, pieces.stsz_entry_count());
  const char kExpectedStsz[] = "00 00 00 06 00 00 00 09 00 00 00 0c";
  EXPECT_EQ(kExpectedStsz, ToHex(pieces.stsz_entries(), true));
}

TEST(Mp4SampleTablePiecesTest, HalfSyncFrames) {
  Recording recording;
  SampleIndexEncoder encoder;
  encoder.Init(&recording, 42);
  for (int i = 1; i <= 5; ++i) {
    int64_t sample_duration_90k = 2 * i;
    int64_t sample_bytes = 3 * i;
    encoder.AddSample(sample_duration_90k, sample_bytes, (i % 2) == 1);
  }

  Mp4SampleTablePieces pieces;
  std::string error_message;
  // Time range [2 + 4 + 6, 2 + 4 + 6 + 8) means the 4th samples should be
  // included. The 3rd gets pulled in also because it is a sync frame and the
  // 4th is not.
  ASSERT_TRUE(
      pieces.Init(&recording, 2, 10, 2 + 4 + 6, 2 + 4 + 6 + 8, &error_message))
      << error_message;

  EXPECT_EQ(2, pieces.stts_entry_count());
  const char kExpectedStts[] =
      "00 00 00 01 00 00 00 06 "
      "00 00 00 01 00 00 00 08";
  EXPECT_EQ(kExpectedStts, ToHex(pieces.stts_entries(), true));

  EXPECT_EQ(1, pieces.stss_entry_count());
  const char kExpectedStss[] = "00 00 00 0a";
  EXPECT_EQ(kExpectedStss, ToHex(pieces.stss_entries(), true));

  EXPECT_EQ(2, pieces.stsz_entry_count());
  const char kExpectedStsz[] = "00 00 00 09 00 00 00 0c";
  EXPECT_EQ(kExpectedStsz, ToHex(pieces.stsz_entries(), true));
}

TEST(Mp4SampleTablePiecesTest, FastPath) {
  Recording recording;
  SampleIndexEncoder encoder;
  encoder.Init(&recording, 42);
  for (int i = 1; i <= 5; ++i) {
    int64_t sample_duration_90k = 2 * i;
    int64_t sample_bytes = 3 * i;
    encoder.AddSample(sample_duration_90k, sample_bytes, (i % 2) == 1);
  }
  auto total_duration_90k = recording.end_time_90k - recording.start_time_90k;

  Mp4SampleTablePieces pieces;
  std::string error_message;
  // Time range [0, end - start) means to pull in everything.
  // This uses a fast path which can determine the size without examining the
  // index.
  ASSERT_TRUE(
      pieces.Init(&recording, 2, 10, 0, total_duration_90k, &error_message))
      << error_message;

  EXPECT_EQ(5, pieces.stts_entry_count());
  const char kExpectedStts[] =
      "00 00 00 01 00 00 00 02 "
      "00 00 00 01 00 00 00 04 "
      "00 00 00 01 00 00 00 06 "
      "00 00 00 01 00 00 00 08 "
      "00 00 00 01 00 00 00 0a";
  EXPECT_EQ(kExpectedStts, ToHex(pieces.stts_entries(), true));

  EXPECT_EQ(3, pieces.stss_entry_count());
  const char kExpectedStss[] = "00 00 00 0a 00 00 00 0c 00 00 00 0e";
  EXPECT_EQ(kExpectedStss, ToHex(pieces.stss_entries(), true));

  EXPECT_EQ(5, pieces.stsz_entry_count());
  const char kExpectedStsz[] =
      "00 00 00 03 00 00 00 06 00 00 00 09 00 00 00 0c 00 00 00 0f";
  EXPECT_EQ(kExpectedStsz, ToHex(pieces.stsz_entries(), true));
}

class IntegrationTest : public testing::Test {
 protected:
  IntegrationTest() {
    tmpdir_path_ = PrepareTempDirOrDie("mp4-integration-test");
    int ret = GetRealFilesystem()->Open(tmpdir_path_.c_str(),
                                        O_RDONLY | O_DIRECTORY, &tmpdir_);
    CHECK_EQ(0, ret) << strerror(ret);
  }

  Recording CopyMp4ToSingleRecording() {
    std::string error_message;
    Recording recording;
    SampleIndexEncoder index;

    // Set start time to 2015-04-26 00:00:00 UTC.
    index.Init(&recording, UINT64_C(1430006400) * kTimeUnitsPerSecond);
    SampleFileWriter writer(tmpdir_.get());
    std::string filename = recording.sample_file_uuid.UnparseText();
    if (!writer.Open(filename.c_str(), &error_message)) {
      ADD_FAILURE() << "open " << filename << ": " << error_message;
      return recording;
    }
    auto in = GetRealVideoSource()->OpenFile("../src/testdata/clip.mp4",
                                             &error_message);
    if (in == nullptr) {
      ADD_FAILURE() << "open clip.mp4" << error_message;
      return recording;
    }

    video_sample_entry_.width = in->stream()->codec->width;
    video_sample_entry_.height = in->stream()->codec->height;
    if (!GetH264SampleEntry(GetExtradata(in.get()), in->stream()->codec->width,
                            in->stream()->codec->height,
                            &video_sample_entry_.data, &error_message)) {
      ADD_FAILURE() << "GetH264SampleEntry: " << error_message;
      return recording;
    }

    while (true) {
      VideoPacket pkt;
      if (!in->GetNext(&pkt, &error_message)) {
        if (!error_message.empty()) {
          ADD_FAILURE() << "GetNext: " << error_message;
          return recording;
        }
        break;
      }
      if (!writer.Write(GetData(pkt), &error_message)) {
        ADD_FAILURE() << "Write: " << error_message;
        return recording;
      }
      index.AddSample(pkt.pkt()->duration, pkt.pkt()->size, pkt.is_key());
    }

    if (!writer.Close(&recording.sample_file_sha1, &error_message)) {
      ADD_FAILURE() << "Close: " << error_message;
    }
    return recording;
  }

  std::shared_ptr<VirtualFile> CreateMp4FromSingleRecording(
      const Recording &recording) {
    Mp4FileBuilder builder(tmpdir_.get());
    builder.SetSampleEntry(video_sample_entry_);
    builder.Append(Recording(recording), 0,
                   std::numeric_limits<int32_t>::max());
    std::string error_message;
    auto mp4 = builder.Build(&error_message);
    EXPECT_TRUE(mp4 != nullptr) << error_message;
    return mp4;
  }

  void WriteMp4(VirtualFile *f) {
    EvBuffer buf;
    std::string error_message;
    ByteRange left(0, f->size());
    while (left.size() > 0) {
      auto ret = f->AddRange(left, &buf, &error_message);
      ASSERT_GT(ret, 0) << error_message;
      left.begin += ret;
    }
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
  std::string etag_;
  VideoSampleEntry video_sample_entry_;
};

TEST_F(IntegrationTest, RoundTrip) {
  Recording recording = CopyMp4ToSingleRecording();
  if (HasFailure()) {
    return;
  }
  auto f = CreateMp4FromSingleRecording(recording);
  WriteMp4(f.get());
  CompareMp4s();
}

TEST_F(IntegrationTest, Metadata) {
  Recording recording = CopyMp4ToSingleRecording();
  if (HasFailure()) {
    return;
  }
  auto f = CreateMp4FromSingleRecording(recording);

  // This test is brittle, which is the point. Any time the digest comparison
  // here fails, it can be updated, but the etag must change as well!
  // Otherwise clients may combine ranges from the new format with ranges
  // from the old format!
  EXPECT_EQ("1e5331e8371bd97ac3158b3a86494abc87cdc70e", Digest(f.get()));
  EXPECT_EQ("\"62f5e00a6e1e6dd893add217b1bf7ed7446b8b9d\"", f->etag());

  // 10 seconds later than the segment's start time.
  EXPECT_EQ(1430006410, f->last_modified());
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
