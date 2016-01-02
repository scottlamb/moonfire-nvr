// This file is part of Moonfire NVR, a security camera digital video recorder.
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
// util_test.cc: tests of the util.h interface.

#include <gflags/gflags.h>
#include <glog/logging.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include <fcntl.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>

#include "http.h"
#include "string.h"
#include "testutil.h"

DECLARE_bool(alsologtostderr);

using moonfire_nvr::internal::ParseRangeHeader;
using moonfire_nvr::internal::RangeHeaderType;

namespace moonfire_nvr {
namespace {

TEST(EvBufferTest, AddFileTest) {
  std::string dir = PrepareTempDirOrDie("http");
  std::string foo_filename = StrCat(dir, "/foo");
  WriteFileOrDie(foo_filename, "foo");

  int in_fd = open(foo_filename.c_str(), O_RDONLY);
  PCHECK(in_fd >= 0) << "open: " << foo_filename;
  std::string error_message;

  // Ensure adding the whole file succeeds.
  EvBuffer buf1;
  ASSERT_TRUE(buf1.AddFile(in_fd, 0, 3, &error_message)) << error_message;
  EXPECT_EQ(3u, evbuffer_get_length(buf1.get()));

  // Ensure adding an empty region succeeds.
  EvBuffer buf2;
  ASSERT_TRUE(buf2.AddFile(in_fd, 0, 0, &error_message)) << error_message;
  EXPECT_EQ(0u, evbuffer_get_length(buf2.get()));
}

// Test the specific examples enumerated in RFC 2616 section 14.35.1.
TEST(RangeHeaderTest, Rfc_2616_Section_14_35_1) {
  std::vector<ByteRange> ranges;
  EXPECT_EQ(RangeHeaderType::kSatisfiable,
            ParseRangeHeader("bytes=0-499", 10000, &ranges));
  EXPECT_THAT(ranges, testing::ElementsAre(ByteRange(0, 500)));

  EXPECT_EQ(RangeHeaderType::kSatisfiable,
            ParseRangeHeader("bytes=500-999", 10000, &ranges));
  EXPECT_THAT(ranges, testing::ElementsAre(ByteRange(500, 1000)));

  EXPECT_EQ(RangeHeaderType::kSatisfiable,
            ParseRangeHeader("bytes=-500", 10000, &ranges));
  EXPECT_THAT(ranges, testing::ElementsAre(ByteRange(9500, 10000)));

  EXPECT_EQ(RangeHeaderType::kSatisfiable,
            ParseRangeHeader("bytes=9500-", 10000, &ranges));
  EXPECT_THAT(ranges, testing::ElementsAre(ByteRange(9500, 10000)));

  EXPECT_EQ(RangeHeaderType::kSatisfiable,
            ParseRangeHeader("bytes=0-0,-1", 10000, &ranges));
  EXPECT_THAT(ranges,
              testing::ElementsAre(ByteRange(0, 1), ByteRange(9999, 10000)));

  // Non-canonical ranges. Possibly the point of these is that the adjacent
  // and overlapping ranges are supposed to be coalesced into one? I'm not
  // going to do that for now...just trying to get something working...
  EXPECT_EQ(RangeHeaderType::kSatisfiable,
            ParseRangeHeader("bytes=500-600,601-999", 10000, &ranges));
  EXPECT_THAT(ranges,
              testing::ElementsAre(ByteRange(500, 601), ByteRange(601, 1000)));
  EXPECT_EQ(RangeHeaderType::kSatisfiable,
            ParseRangeHeader("bytes=500-700,601-999", 10000, &ranges));
  EXPECT_THAT(ranges,
              testing::ElementsAre(ByteRange(500, 701), ByteRange(601, 1000)));
}

TEST(RangeHeaderTest, Satisfiability) {
  std::vector<ByteRange> ranges;
  EXPECT_EQ(RangeHeaderType::kNotSatisfiable,
            ParseRangeHeader("bytes=10000-", 10000, &ranges));
  EXPECT_EQ(RangeHeaderType::kSatisfiable,
            ParseRangeHeader("bytes=0-499,10000-", 10000, &ranges));
  EXPECT_THAT(ranges, testing::ElementsAre(ByteRange(0, 500)));
  EXPECT_EQ(RangeHeaderType::kNotSatisfiable,
            ParseRangeHeader("bytes=-1", 0, &ranges));
  EXPECT_EQ(RangeHeaderType::kNotSatisfiable,
            ParseRangeHeader("bytes=0-0", 0, &ranges));
  EXPECT_EQ(RangeHeaderType::kNotSatisfiable,
            ParseRangeHeader("bytes=0-", 0, &ranges));
  EXPECT_EQ(RangeHeaderType::kSatisfiable,
            ParseRangeHeader("bytes=0-0", 1, &ranges));
  EXPECT_THAT(ranges, testing::ElementsAre(ByteRange(0, 1)));
  EXPECT_EQ(RangeHeaderType::kSatisfiable,
            ParseRangeHeader("bytes=0-", 1, &ranges));
  EXPECT_THAT(ranges, testing::ElementsAre(ByteRange(0, 1)));
}

TEST(RangeHeaderTest, AbsentOrInvalid) {
  std::vector<ByteRange> ranges;
  EXPECT_EQ(RangeHeaderType::kAbsentOrInvalid,
            ParseRangeHeader(nullptr, 10000, &ranges));
  EXPECT_EQ(RangeHeaderType::kAbsentOrInvalid,
            ParseRangeHeader("", 10000, &ranges));
  EXPECT_EQ(RangeHeaderType::kAbsentOrInvalid,
            ParseRangeHeader("foo=0-499", 10000, &ranges));
  EXPECT_EQ(RangeHeaderType::kAbsentOrInvalid,
            ParseRangeHeader("foo=0-499", 10000, &ranges));
  EXPECT_EQ(RangeHeaderType::kAbsentOrInvalid,
            ParseRangeHeader("bytes=499-0", 10000, &ranges));
  EXPECT_EQ(RangeHeaderType::kAbsentOrInvalid,
            ParseRangeHeader("bytes=", 10000, &ranges));
  EXPECT_EQ(RangeHeaderType::kAbsentOrInvalid,
            ParseRangeHeader("bytes=,", 10000, &ranges));
  EXPECT_EQ(RangeHeaderType::kAbsentOrInvalid,
            ParseRangeHeader("bytes=-", 10000, &ranges));
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
