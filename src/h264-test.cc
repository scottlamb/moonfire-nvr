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
// h264-test.cc: tests of the h264.h interface.

#include <gflags/gflags.h>
#include <glog/logging.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "h264.h"
#include "string.h"

DECLARE_bool(alsologtostderr);

namespace moonfire_nvr {
namespace {

const uint8_t kAnnexBTestInput[] = {
    0x00, 0x00, 0x00, 0x01, 0x67, 0x4d, 0x00, 0x1f, 0x9a, 0x66, 0x02, 0x80,
    0x2d, 0xff, 0x35, 0x01, 0x01, 0x01, 0x40, 0x00, 0x00, 0xfa, 0x00, 0x00,
    0x1d, 0x4c, 0x01, 0x00, 0x00, 0x00, 0x01, 0x68, 0xee, 0x3c, 0x80};

const uint8_t kAvcDecoderConfigTestInput[] = {
    0x01, 0x4d, 0x00, 0x1f, 0xff, 0xe1, 0x00, 0x17, 0x67, 0x4d,
    0x00, 0x1f, 0x9a, 0x66, 0x02, 0x80, 0x2d, 0xff, 0x35, 0x01,
    0x01, 0x01, 0x40, 0x00, 0x00, 0xfa, 0x00, 0x00, 0x1d, 0x4c,
    0x01, 0x01, 0x00, 0x04, 0x68, 0xee, 0x3c, 0x80};

const char kTestOutput[] =
    "00 00 00 84 61 76 63 31 00 00 00 00 00 00 00 01 "
    "00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 "
    "05 00 02 d0 00 48 00 00 00 48 00 00 00 00 00 00 "
    "00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 "
    "00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 "
    "00 00 00 18 ff ff 00 00 00 2e 61 76 63 43 01 4d "
    "00 1f ff e1 00 17 67 4d 00 1f 9a 66 02 80 2d ff "
    "35 01 01 01 40 00 00 fa 00 00 1d 4c 01 01 00 04 "
    "68 ee 3c 80";

TEST(H264Test, DecodeOnly) {
  std::vector<std::string> nal_units_hexed;
  re2::StringPiece test_input(reinterpret_cast<const char *>(kAnnexBTestInput),
                              sizeof(kAnnexBTestInput));
  internal::NalUnitFunction fn = [&nal_units_hexed](re2::StringPiece nal_unit) {
    nal_units_hexed.push_back(ToHex(nal_unit, true));
    return IterationControl::kContinue;
  };
  std::string error_message;
  ASSERT_TRUE(internal::DecodeH264AnnexB(test_input, fn, &error_message))
      << error_message;
  EXPECT_THAT(nal_units_hexed,
              testing::ElementsAre("67 4d 00 1f 9a 66 02 80 2d ff 35 01 01 01 "
                                   "40 00 00 fa 00 00 1d 4c 01",
                                   "68 ee 3c 80"));
}

TEST(H264Test, SampleEntryFromAnnexBExtraData) {
  re2::StringPiece test_input(reinterpret_cast<const char *>(kAnnexBTestInput),
                              sizeof(kAnnexBTestInput));
  std::string sample_entry;
  std::string error_message;
  bool need_transform;
  ASSERT_TRUE(ParseExtraData(test_input, 1280, 720, &sample_entry,
                             &need_transform, &error_message))
      << error_message;

  EXPECT_EQ(kTestOutput, ToHex(sample_entry, true));
  EXPECT_TRUE(need_transform);
}

TEST(H264Test, SampleEntryFromAvcDecoderConfigExtraData) {
  re2::StringPiece test_input(
      reinterpret_cast<const char *>(kAvcDecoderConfigTestInput),
      sizeof(kAvcDecoderConfigTestInput));
  std::string sample_entry;
  std::string error_message;
  bool need_transform;
  ASSERT_TRUE(ParseExtraData(test_input, 1280, 720, &sample_entry,
                             &need_transform, &error_message))
      << error_message;

  EXPECT_EQ(kTestOutput, ToHex(sample_entry, true));
  EXPECT_FALSE(need_transform);
}

TEST(H264Test, TransformSampleEntry) {
  const uint8_t kInput[] = {
      0x00, 0x00, 0x00, 0x01, 0x67, 0x4d, 0x00, 0x1f, 0x9a, 0x66,
      0x02, 0x80, 0x2d, 0xff, 0x35, 0x01, 0x01, 0x01, 0x40, 0x00,
      0x00, 0xfa, 0x00, 0x00, 0x1d, 0x4c, 0x01,

      0x00, 0x00, 0x00, 0x01, 0x68, 0xee, 0x3c, 0x80,

      0x00, 0x00, 0x00, 0x01, 0x06, 0x06, 0x01, 0xc4, 0x80,

      0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80, 0x10, 0x00, 0x08,
      0x7f, 0x00, 0x5d, 0x27, 0xb5, 0xc1, 0xff, 0x8c, 0xd6, 0x35,
      // (truncated)
  };
  const char kExpectedOutput[] =
      "00 00 00 17 "
      "67 4d 00 1f 9a 66 02 80 2d ff 35 01 01 01 40 00 00 fa 00 00 1d 4c 01 "
      "00 00 00 04 68 ee 3c 80 "
      "00 00 00 05 06 06 01 c4 80 "
      "00 00 00 10 "
      "65 88 80 10 00 08 7f 00 5d 27 b5 c1 ff 8c d6 35";
  re2::StringPiece input(reinterpret_cast<const char *>(kInput),
                         sizeof(kInput));
  std::string out;
  std::string error_message;
  ASSERT_TRUE(TransformSampleData(input, &out, &error_message))
      << error_message;
  EXPECT_EQ(kExpectedOutput, ToHex(out, true));
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
