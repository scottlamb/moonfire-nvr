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
// recording-test.cc: tests of the recording.h interface.

#include <fcntl.h>
#include <sys/stat.h>
#include <sys/types.h>

#include <gflags/gflags.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "recording.h"
#include "string.h"

DECLARE_bool(alsologtostderr);

using testing::_;
using testing::HasSubstr;
using testing::DoAll;
using testing::Return;
using testing::SetArgPointee;

namespace moonfire_nvr {
namespace {

// Example from design/schema.md.
TEST(SampleIndexTest, EncodeExample) {
  SampleIndexEncoder encoder;
  encoder.AddSample(10, 1000, true);
  encoder.AddSample(9, 10, false);
  encoder.AddSample(11, 15, false);
  encoder.AddSample(10, 12, false);
  encoder.AddSample(10, 1050, true);
  ASSERT_EQ("29 d0 0f 02 14 08 0a 02 05 01 64", ToHex(encoder.data(), true));
}

TEST(SampleIndexTest, RoundTrip) {
  SampleIndexEncoder encoder;
  encoder.AddSample(10, 30000, true);
  encoder.AddSample(9, 1000, false);
  encoder.AddSample(11, 1100, false);
  encoder.AddSample(18, 31000, true);

  SampleIndexIterator it = SampleIndexIterator(encoder.data());
  std::string error_message;
  ASSERT_FALSE(it.done()) << it.error();
  EXPECT_EQ(10, it.duration_90k());
  EXPECT_EQ(30000, it.bytes());
  EXPECT_TRUE(it.is_key());

  it.Next();
  ASSERT_FALSE(it.done()) << it.error();
  EXPECT_EQ(9, it.duration_90k());
  EXPECT_EQ(1000, it.bytes());
  EXPECT_FALSE(it.is_key());

  it.Next();
  ASSERT_FALSE(it.done()) << it.error();
  EXPECT_EQ(11, it.duration_90k());
  EXPECT_EQ(1100, it.bytes());
  EXPECT_FALSE(it.is_key());

  it.Next();
  ASSERT_FALSE(it.done()) << it.error();
  EXPECT_EQ(18, it.duration_90k());
  EXPECT_EQ(31000, it.bytes());
  EXPECT_TRUE(it.is_key());

  it.Next();
  ASSERT_TRUE(it.done());
  ASSERT_FALSE(it.has_error()) << it.error();
}

TEST(SampleIndexTest, IteratorErrors) {
  std::string bad_first_varint("\x80");
  SampleIndexIterator it(bad_first_varint);
  EXPECT_TRUE(it.has_error());
  EXPECT_EQ("buffer underrun", it.error());

  std::string bad_second_varint("\x00\x80", 2);
  it = SampleIndexIterator(bad_second_varint);
  EXPECT_TRUE(it.has_error());
  EXPECT_EQ("buffer underrun", it.error());

  std::string zero_durations("\x00\x02\x00\x00", 4);
  it = SampleIndexIterator(zero_durations);
  EXPECT_TRUE(it.has_error());
  EXPECT_THAT(it.error(), HasSubstr("zero duration"));

  std::string negative_duration("\x02\x02", 2);
  it = SampleIndexIterator(negative_duration);
  EXPECT_TRUE(it.has_error());
  EXPECT_THAT(it.error(), HasSubstr("negative duration"));

  std::string non_positive_bytes("\x04\x00", 2);
  it = SampleIndexIterator(non_positive_bytes);
  EXPECT_TRUE(it.has_error());
  EXPECT_THAT(it.error(), HasSubstr("non-positive bytes"));
}

TEST(SampleFileWriterTest, Simple) {
  testing::StrictMock<MockFile> parent;
  auto *f = new testing::StrictMock<MockFile>;

  re2::StringPiece write_1("write 1");
  re2::StringPiece write_2("write 2");

  EXPECT_CALL(parent, OpenRaw("foo", O_WRONLY | O_EXCL | O_CREAT, 0600, _))
      .WillOnce(DoAll(SetArgPointee<3>(f), Return(0)));
  EXPECT_CALL(*f, Write(write_1, _))
      .WillOnce(DoAll(SetArgPointee<1>(7), Return(0)));
  EXPECT_CALL(*f, Write(write_2, _))
      .WillOnce(DoAll(SetArgPointee<1>(7), Return(0)));
  EXPECT_CALL(*f, Sync()).WillOnce(Return(0));
  EXPECT_CALL(*f, Close()).WillOnce(Return(0));

  SampleFileWriter writer(&parent);
  std::string error_message;
  std::string sha1;
  ASSERT_TRUE(writer.Open("foo", &error_message)) << error_message;
  EXPECT_TRUE(writer.Write(write_1, &error_message)) << error_message;
  EXPECT_TRUE(writer.Write(write_2, &error_message)) << error_message;
  EXPECT_TRUE(writer.Close(&sha1, &error_message)) << error_message;
  EXPECT_EQ("6bc37325b36fb5fd205e57284429e75764338618", ToHex(sha1));
}

TEST(SampleFileWriterTest, PartialWriteIsRetried) {
  testing::StrictMock<MockFile> parent;
  auto *f = new testing::StrictMock<MockFile>;

  re2::StringPiece write_1("write 1");
  re2::StringPiece write_2("write 2");
  re2::StringPiece write_2b(write_2);
  write_2b.remove_prefix(3);

  EXPECT_CALL(parent, OpenRaw("foo", O_WRONLY | O_EXCL | O_CREAT, 0600, _))
      .WillOnce(DoAll(SetArgPointee<3>(f), Return(0)));
  EXPECT_CALL(*f, Write(write_1, _))
      .WillOnce(DoAll(SetArgPointee<1>(7), Return(0)));
  EXPECT_CALL(*f, Write(write_2, _))
      .WillOnce(DoAll(SetArgPointee<1>(3), Return(0)));
  EXPECT_CALL(*f, Write(write_2b, _))
      .WillOnce(DoAll(SetArgPointee<1>(4), Return(0)));
  EXPECT_CALL(*f, Sync()).WillOnce(Return(0));
  EXPECT_CALL(*f, Close()).WillOnce(Return(0));

  SampleFileWriter writer(&parent);
  std::string error_message;
  std::string sha1;
  ASSERT_TRUE(writer.Open("foo", &error_message)) << error_message;
  EXPECT_TRUE(writer.Write(write_1, &error_message)) << error_message;
  EXPECT_TRUE(writer.Write(write_2, &error_message)) << error_message;
  EXPECT_TRUE(writer.Close(&sha1, &error_message)) << error_message;
  EXPECT_EQ("6bc37325b36fb5fd205e57284429e75764338618", ToHex(sha1));
}

TEST(SampleFileWriterTest, PartialWriteIsTruncated) {
  testing::StrictMock<MockFile> parent;
  auto *f = new testing::StrictMock<MockFile>;

  re2::StringPiece write_1("write 1");
  re2::StringPiece write_2("write 2");
  re2::StringPiece write_2b(write_2);
  write_2b.remove_prefix(3);

  EXPECT_CALL(parent, OpenRaw("foo", O_WRONLY | O_EXCL | O_CREAT, 0600, _))
      .WillOnce(DoAll(SetArgPointee<3>(f), Return(0)));
  EXPECT_CALL(*f, Write(write_1, _))
      .WillOnce(DoAll(SetArgPointee<1>(7), Return(0)));
  EXPECT_CALL(*f, Write(write_2, _))
      .WillOnce(DoAll(SetArgPointee<1>(3), Return(0)));
  EXPECT_CALL(*f, Write(write_2b, _)).WillOnce(Return(ENOSPC));
  EXPECT_CALL(*f, Truncate(7)).WillOnce(Return(0));
  EXPECT_CALL(*f, Sync()).WillOnce(Return(0));
  EXPECT_CALL(*f, Close()).WillOnce(Return(0));

  SampleFileWriter writer(&parent);
  std::string error_message;
  std::string sha1;
  ASSERT_TRUE(writer.Open("foo", &error_message)) << error_message;
  EXPECT_TRUE(writer.Write(write_1, &error_message)) << error_message;
  EXPECT_FALSE(writer.Write(write_2, &error_message)) << error_message;
  EXPECT_TRUE(writer.Close(&sha1, &error_message)) << error_message;
  EXPECT_EQ("b1ccee339b935587c09997a9ec8bb2374e02b5d0", ToHex(sha1));
}

TEST(SampleFileWriterTest, PartialWriteTruncateFailureCausesCloseToFail) {
  testing::StrictMock<MockFile> parent;
  auto *f = new testing::StrictMock<MockFile>;

  re2::StringPiece write_1("write 1");
  re2::StringPiece write_2("write 2");
  re2::StringPiece write_2b(write_2);
  write_2b.remove_prefix(3);

  EXPECT_CALL(parent, OpenRaw("foo", O_WRONLY | O_EXCL | O_CREAT, 0600, _))
      .WillOnce(DoAll(SetArgPointee<3>(f), Return(0)));
  EXPECT_CALL(*f, Write(write_1, _))
      .WillOnce(DoAll(SetArgPointee<1>(7), Return(0)));
  EXPECT_CALL(*f, Write(write_2, _))
      .WillOnce(DoAll(SetArgPointee<1>(3), Return(0)));
  EXPECT_CALL(*f, Write(write_2b, _)).WillOnce(Return(EIO));
  EXPECT_CALL(*f, Truncate(7)).WillOnce(Return(EIO));
  EXPECT_CALL(*f, Close()).WillOnce(Return(0));

  SampleFileWriter writer(&parent);
  std::string error_message;
  std::string sha1;
  ASSERT_TRUE(writer.Open("foo", &error_message)) << error_message;
  EXPECT_TRUE(writer.Write(write_1, &error_message)) << error_message;
  EXPECT_FALSE(writer.Write(write_2, &error_message)) << error_message;
  EXPECT_FALSE(writer.Close(&sha1, &error_message)) << error_message;
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
