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
// ffmpeg-test.cc: tests of the ffmpeg.h interface.

#include <gflags/gflags.h>
#include <glog/logging.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "ffmpeg.h"

DECLARE_bool(alsologtostderr);

namespace moonfire_nvr {
namespace {

class FfmpegTest : public testing::Test {
 public:
  static void SetUpTestCase() {
    FfmpegGlobalSetup();
  }
};

TEST_F(FfmpegTest, Read) {
  // The cwd should be the cmake build directory, which should be a subdir of
  // the project.
  InputVideoPacketStream stream;
  std::string error_msg;
  ASSERT_TRUE(
      stream.Open("../src/testdata/ffmpeg-bug-5018.mp4", &error_msg))
      << error_msg;
  VideoPacket pkt;
  std::vector<int64_t> ptses;
  while (stream.GetNext(&pkt, &error_msg)) {
    ptses.push_back(pkt.pts());
  }
  ASSERT_EQ("", error_msg);
  int64_t kExpectedPtses[] = {
      71001, 81005, 88995, 99005, 107043, 117022, 125000, 135022, 142992,
      153101, 161005, 171005, 178996, 189008, 197039, 207023, 215003, 225004,
      232993, 243007, 251000, 261014, 268991, 279009, 287079, 297037, 305008,
      315010, 322995, 333016, 341001, 351105, 359009, 369017, 377005, 387029
  };
  ASSERT_THAT(ptses, testing::ElementsAreArray(kExpectedPtses));
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
