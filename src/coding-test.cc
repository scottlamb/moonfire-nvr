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
// coding-test.cc: tests of the coding.h interface.

#include <gflags/gflags.h>
#include <glog/logging.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "coding.h"

DECLARE_bool(alsologtostderr);

namespace moonfire_nvr {
namespace {

TEST(VarintTest, Simple) {
  // Encode.
  std::string foo;
  AppendVar32(UINT32_C(1), &foo);
  EXPECT_EQ("\x01", foo);
  AppendVar32(UINT32_C(300), &foo);
  EXPECT_EQ("\x01\xac\x02", foo);

  // Decode.
  re2::StringPiece p(foo);
  uint32_t out;
  std::string error_message;
  EXPECT_TRUE(DecodeVar32(&p, &out, &error_message));
  EXPECT_EQ(UINT32_C(1), out);
  EXPECT_TRUE(DecodeVar32(&p, &out, &error_message));
  EXPECT_EQ(UINT32_C(300), out);
  EXPECT_EQ(0, p.size());
}

TEST(VarintTest, AllDecodeSizes) {
  std::string error_message;
  const uint32_t kToDecode[]{
      1,
      1 | (2 << 7),
      1 | (2 << 7) | (3 << 14),
      1 | (2 << 7) | (3 << 14) | (4 << 21),
      1 | (2 << 7) | (3 << 14) | (4 << 21) | (5 << 28),
  };
  for (size_t i = 0; i < sizeof(kToDecode) / sizeof(kToDecode[0]); ++i) {
    auto in = kToDecode[i];
    std::string foo;
    AppendVar32(in, &foo);
    ASSERT_EQ(i + 1, foo.size());
    re2::StringPiece p(foo);
    uint32_t out;

    // Slow path: last bytes of the buffer.
    DecodeVar32(&p, &out, &error_message);
    EXPECT_EQ(in, out) << "i: " << i;
    EXPECT_EQ(0, p.size()) << "i: " << i;

    // Fast path: plenty of bytes in the buffer.
    foo.append(4, 0);
    p = foo;
    DecodeVar32(&p, &out, &error_message);
    EXPECT_EQ(in, out);
    EXPECT_EQ(4, p.size());
  }
}

TEST(VarintTest, DecodeErrors) {
  re2::StringPiece empty;
  uint32_t out;
  std::string error_message;

  for (auto input :
       {re2::StringPiece("", 0), re2::StringPiece("\x80", 1),
        re2::StringPiece("\x80\x80", 2), re2::StringPiece("\x80\x80\x80", 3),
        re2::StringPiece("\x80\x80\x80\x80", 4)}) {
    EXPECT_FALSE(DecodeVar32(&input, &out, &error_message)) << "input: "
                                                            << input;
    EXPECT_EQ("buffer underrun", error_message);
  }

  re2::StringPiece too_big("\x80\x80\x80\x80\x10", 5);
  EXPECT_FALSE(DecodeVar32(&too_big, &out, &error_message));
  EXPECT_EQ("integer overflow", error_message);
}

TEST(ZigzagTest, Encode) {
  EXPECT_EQ(UINT32_C(0), Zigzag32(INT32_C(0)));
  EXPECT_EQ(UINT32_C(1), Zigzag32(INT32_C(-1)));
  EXPECT_EQ(UINT32_C(2), Zigzag32(INT32_C(1)));
  EXPECT_EQ(UINT32_C(3), Zigzag32(INT32_C(-2)));
  EXPECT_EQ(UINT32_C(4294967294), Zigzag32(INT32_C(2147483647)));
  EXPECT_EQ(UINT32_C(4294967295), Zigzag32(INT32_C(-2147483648)));
}

TEST(ZigzagTest, Decode) {
  EXPECT_EQ(INT32_C(0), Unzigzag32(UINT32_C(0)));
  EXPECT_EQ(INT32_C(-1), Unzigzag32(UINT32_C(1)));
  EXPECT_EQ(INT32_C(1), Unzigzag32(UINT32_C(2)));
  EXPECT_EQ(INT32_C(-2), Unzigzag32(UINT32_C(3)));
  EXPECT_EQ(INT32_C(2147483647), Unzigzag32(UINT32_C(4294967294)));
  EXPECT_EQ(INT32_C(-2147483648), Unzigzag32(UINT32_C(4294967295)));
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
