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
// string-test.cc: tests of the string.h interface.

#include <gflags/gflags.h>
#include <glog/logging.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "string.h"

DECLARE_bool(alsologtostderr);

namespace moonfire_nvr {
namespace {

TEST(StrCatTest, Simple) {
  EXPECT_EQ("foo", StrCat("foo"));
  EXPECT_EQ("foobar", StrCat("foo", "bar"));
  EXPECT_EQ("foo", StrCat(std::string("foo")));

  EXPECT_EQ("42", StrCat(uint64_t(42)));
  EXPECT_EQ("0", StrCat(uint64_t(0)));
  EXPECT_EQ("18446744073709551615",
            StrCat(std::numeric_limits<uint64_t>::max()));

  EXPECT_EQ("42", StrCat(int64_t(42)));
  EXPECT_EQ("0", StrCat(int64_t(0)));
  EXPECT_EQ("-9223372036854775808",
            StrCat(std::numeric_limits<int64_t>::min()));
  EXPECT_EQ("9223372036854775807", StrCat(std::numeric_limits<int64_t>::max()));
}

TEST(JoinTest, Simple) {
  EXPECT_EQ("", Join(std::initializer_list<std::string>(), ","));
  EXPECT_EQ("a", Join(std::initializer_list<std::string>({"a"}), ","));
  EXPECT_EQ("a,b", Join(std::initializer_list<const char *>({"a", "b"}), ","));
  EXPECT_EQ(
      "a,b,c",
      Join(std::initializer_list<re2::StringPiece>({"a", "b", "c"}), ","));
}

TEST(IsWordTest, Simple) {
  EXPECT_TRUE(IsWord(""));
  EXPECT_TRUE(IsWord("0123456789"));
  EXPECT_TRUE(IsWord("abcdefghijklmnopqrstuvwxyz"));
  EXPECT_TRUE(IsWord("ABCDEFGHIJKLMNOPQRSTUVWXYZ"));
  EXPECT_TRUE(IsWord("_"));

  EXPECT_TRUE(IsWord("4bJ_"));

  EXPECT_FALSE(IsWord("/"));
  EXPECT_FALSE(IsWord("abc/"));
  EXPECT_FALSE(IsWord(" "));
  EXPECT_FALSE(IsWord("@"));
  EXPECT_FALSE(IsWord("["));
  EXPECT_FALSE(IsWord("`"));
  EXPECT_FALSE(IsWord("{"));
}

TEST(EscapeTest, Simple) {
  EXPECT_EQ("", moonfire_nvr::EscapeHtml(""));
  EXPECT_EQ("no special chars", moonfire_nvr::EscapeHtml("no special chars"));
  EXPECT_EQ("&lt;tag&gt; &amp; text", moonfire_nvr::EscapeHtml("<tag> & text"));
}

TEST(ToHexTest, Simple) {
  EXPECT_EQ("", ToHex("", false));
  EXPECT_EQ("", ToHex("", true));
  EXPECT_EQ("1234deadbeef", ToHex("\x12\x34\xde\xad\xbe\xef", false));
  EXPECT_EQ("12 34 de ad be ef", ToHex("\x12\x34\xde\xad\xbe\xef", true));
}

TEST(HumanizeTest, Simple) {
  EXPECT_EQ("1.0 B", HumanizeWithBinaryPrefix(1.f, "B"));
  EXPECT_EQ("1.0 KiB", HumanizeWithBinaryPrefix(UINT64_C(1) << 10, "B"));
  EXPECT_EQ("1.0 EiB", HumanizeWithBinaryPrefix(UINT64_C(1) << 60, "B"));
  EXPECT_EQ("1.5 EiB", HumanizeWithBinaryPrefix(
                           (UINT64_C(1) << 60) + (UINT64_C(1) << 59), "B"));
  EXPECT_EQ("16.0 EiB", HumanizeWithBinaryPrefix(
                            std::numeric_limits<uint64_t>::max(), "B"));

  EXPECT_EQ("1.0 Mbps", HumanizeWithDecimalPrefix(1e6f, "bps"));
  EXPECT_EQ("1000.0 Ebps", HumanizeWithDecimalPrefix(1e21, "bps"));
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
