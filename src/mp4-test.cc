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

#include <gflags/gflags.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "http.h"
#include "mp4.h"
#include "string.h"

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

}  // namespace
}  // namespace moonfire_nvr

int main(int argc, char **argv) {
  FLAGS_alsologtostderr = true;
  google::ParseCommandLineFlags(&argc, &argv, true);
  testing::InitGoogleTest(&argc, argv);
  google::InitGoogleLogging(argv[0]);
  return RUN_ALL_TESTS();
}
