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

using testing::_;
using testing::AnyNumber;
using testing::DoAll;
using testing::Return;
using testing::SetArgPointee;

namespace moonfire_nvr {
namespace {

class MockFileSlice : public FileSlice {
 public:
  MOCK_CONST_METHOD0(size, int64_t());
  MOCK_CONST_METHOD3(AddRange, bool(ByteRange, EvBuffer *, std::string *));
};

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

class FileSlicesTest : public testing::Test {
 protected:
  FileSlicesTest() {
    EXPECT_CALL(a_, size()).Times(AnyNumber()).WillRepeatedly(Return(5));
    EXPECT_CALL(b_, size()).Times(AnyNumber()).WillRepeatedly(Return(13));
    EXPECT_CALL(c_, size()).Times(AnyNumber()).WillRepeatedly(Return(7));
    EXPECT_CALL(d_, size()).Times(AnyNumber()).WillRepeatedly(Return(17));
    EXPECT_CALL(e_, size()).Times(AnyNumber()).WillRepeatedly(Return(19));

    slices_.Append(&a_);
    slices_.Append(&b_);
    slices_.Append(&c_);
    slices_.Append(&d_);
    slices_.Append(&e_);
  }

  FileSlices slices_;
  testing::StrictMock<MockFileSlice> a_;
  testing::StrictMock<MockFileSlice> b_;
  testing::StrictMock<MockFileSlice> c_;
  testing::StrictMock<MockFileSlice> d_;
  testing::StrictMock<MockFileSlice> e_;
};

TEST_F(FileSlicesTest, Size) {
  EXPECT_EQ(5 + 13 + 7 + 17 + 19, slices_.size());
}

TEST_F(FileSlicesTest, ExactSlice) {
  // Exactly slice b.
  std::string error_message;
  EXPECT_CALL(b_, AddRange(ByteRange(0, 13), _, _)).WillOnce(Return(true));
  EXPECT_TRUE(slices_.AddRange(ByteRange(5, 18), nullptr, &error_message))
      << error_message;
}

TEST_F(FileSlicesTest, Offset) {
  // Part of slice b, all of slice c, and part of slice d.
  std::string error_message;
  EXPECT_CALL(b_, AddRange(ByteRange(12, 13), _, _)).WillOnce(Return(true));
  EXPECT_CALL(c_, AddRange(ByteRange(0, 7), _, _)).WillOnce(Return(true));
  EXPECT_CALL(d_, AddRange(ByteRange(0, 1), _, _)).WillOnce(Return(true));
  EXPECT_TRUE(slices_.AddRange(ByteRange(17, 26), nullptr, &error_message))
      << error_message;
}

TEST_F(FileSlicesTest, Everything) {
  std::string error_message;
  EXPECT_CALL(a_, AddRange(ByteRange(0, 5), _, _)).WillOnce(Return(true));
  EXPECT_CALL(b_, AddRange(ByteRange(0, 13), _, _)).WillOnce(Return(true));
  EXPECT_CALL(c_, AddRange(ByteRange(0, 7), _, _)).WillOnce(Return(true));
  EXPECT_CALL(d_, AddRange(ByteRange(0, 17), _, _)).WillOnce(Return(true));
  EXPECT_CALL(e_, AddRange(ByteRange(0, 19), _, _)).WillOnce(Return(true));
  EXPECT_TRUE(slices_.AddRange(ByteRange(0, 61), nullptr, &error_message))
      << error_message;
}

TEST_F(FileSlicesTest, PropagateError) {
  std::string error_message;
  EXPECT_CALL(a_, AddRange(ByteRange(0, 5), _, _)).WillOnce(Return(true));
  EXPECT_CALL(b_, AddRange(ByteRange(0, 13), _, _))
      .WillOnce(DoAll(SetArgPointee<2>("asdf"), Return(false)));
  EXPECT_FALSE(slices_.AddRange(ByteRange(0, 61), nullptr, &error_message));
  EXPECT_EQ("asdf", error_message);
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
