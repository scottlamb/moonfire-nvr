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
// moonfire-nvr-test.cc: tests of the moonfire-nvr.cc interface.

#include <fcntl.h>
#include <sys/stat.h>
#include <sys/types.h>

#include <gflags/gflags.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "moonfire-nvr.h"
#include "string.h"
#include "testutil.h"

DECLARE_bool(alsologtostderr);

using testing::_;
using testing::AnyNumber;
using testing::HasSubstr;
using testing::Invoke;
using testing::Return;

namespace moonfire_nvr {
namespace {

class MockVideoSource : public VideoSource {
 public:
  // Proxy, as gmock doesn't support non-copyable return values.
  std::unique_ptr<InputVideoPacketStream> OpenRtsp(
      const std::string &url, std::string *error_message) final {
    return std::unique_ptr<InputVideoPacketStream>(
        OpenRtspRaw(url, error_message));
  }
  std::unique_ptr<InputVideoPacketStream> OpenFile(
      const std::string &file, std::string *error_message) final {
    return std::unique_ptr<InputVideoPacketStream>(
        OpenFileRaw(file, error_message));
  }

  MOCK_METHOD2(OpenRtspRaw,
               InputVideoPacketStream *(const std::string &, std::string *));
  MOCK_METHOD2(OpenFileRaw,
               InputVideoPacketStream *(const std::string &, std::string *));
};

class StreamTest : public testing::Test {
 public:
  StreamTest() {
    std::string error_message;
    test_dir_ = PrepareTempDirOrDie("moonfire-nvr-stream-copier");
    env_.clock = &clock_;
    env_.video_source = &video_source_;
    int ret = moonfire_nvr::GetRealFilesystem()->Open(
        test_dir_.c_str(), O_DIRECTORY | O_RDONLY, &sample_file_dir_);
    CHECK_EQ(0, ret) << "open: " << strerror(ret);
    env_.sample_file_dir = sample_file_dir_.get();

    CHECK(db_.Open(StrCat(test_dir_, "/db").c_str(),
                   SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, &error_message))
        << error_message;
    std::string create_sql = ReadFileOrDie("../src/schema.sql");
    {
      DatabaseContext ctx(&db_);
      CHECK(RunStatements(&ctx, create_sql, &error_message)) << error_message;
      auto run = ctx.UseOnce(
          R"(
          insert into camera (uuid,  short_name,  host,  username,  password,
                              main_rtsp_path,  sub_rtsp_path,  retain_bytes)
                      values (:uuid, :short_name, :host, :username, :password,
                              :main_rtsp_path, :sub_rtsp_path, :retain_bytes);
          )");
      run.BindBlob(":uuid", GetRealUuidGenerator()->Generate().binary_view());
      run.BindText(":short_name", "test");
      run.BindText(":host", "test-camera");
      run.BindText(":username", "foo");
      run.BindText(":password", "bar");
      run.BindText(":main_rtsp_path", "/main");
      run.BindText(":sub_rtsp_path", "/sub");
      run.BindInt64(":retain_bytes", 1000000);
      CHECK_EQ(SQLITE_DONE, run.Step()) << run.error_message();
    }
    mdb_.SetUuidGeneratorForTesting(&uuidgen_);
    CHECK(mdb_.Init(&db_, &error_message)) << error_message;
    env_.mdb = &mdb_;

    ListCamerasRow row;
    int n_rows = 0;
    mdb_.ListCameras([&row, &n_rows](const ListCamerasRow &some_row) {
      ++n_rows;
      row = some_row;
      return IterationControl::kContinue;
    });
    CHECK_EQ(1, n_rows);

    clock_.Sleep({1430006400, 0});  // 2015-04-26 00:00:00 UTC

    stream_.reset(new Stream(&signal_, &env_, row, 0, 5));
  }

  // A function to use in OpenRtspRaw invocations which shuts down the stream
  // and indicates that the input video source can't be opened.
  InputVideoPacketStream *Shutdown(const std::string &url,
                                   std::string *error_message) {
    *error_message = "(shutting down)";
    signal_.Shutdown();
    return nullptr;
  }

  struct Frame {
    Frame(bool is_key, int64_t pts, int64_t duration)
        : is_key(is_key), pts(pts), duration(duration) {}
    bool is_key;
    int64_t pts;
    int64_t duration;

    bool operator==(const Frame &o) const {
      return is_key == o.is_key && pts == o.pts && duration == o.duration;
    }

    friend std::ostream &operator<<(std::ostream &os, const Frame &f) {
      return os << "Frame(" << f.is_key << ", " << f.pts << ", " << f.duration
                << ")";
    }
  };

#if 0
  std::vector<Frame> GetFrames(const std::string &path) {
    std::vector<Frame> frames;
    std::string error_message;
    std::string full_path = StrCat(test_dir_, "/test/", path);
    auto f = GetRealVideoSource()->OpenFile(full_path, &error_message);
    if (f == nullptr) {
      ADD_FAILURE() << full_path << ": " << error_message;
      return frames;
    }
    VideoPacket pkt;
    while (f->GetNext(&pkt, &error_message)) {
      frames.push_back(Frame(pkt.is_key(), pkt.pts(), pkt.pkt()->duration));
    }
    EXPECT_EQ("", error_message);
    return frames;
  }
#else
  std::vector<Frame> GetFrames(const re2::StringPiece uuid_text) {
    std::vector<Frame> frames;
    Uuid uuid;
    if (!uuid.ParseText(uuid_text)) {
      ADD_FAILURE() << "unparseable: " << uuid_text;
      return frames;
    }
    DatabaseContext ctx(&db_);
    auto run = ctx.UseOnce(
        "select video_index from recording where sample_file_uuid = :uuid;");
    run.BindBlob(":uuid", uuid.binary_view());
    if (run.Step() != SQLITE_ROW) {
      ADD_FAILURE() << run.error_message();
      return frames;
    }
    for (SampleIndexIterator it(run.ColumnBlob(0)); !it.done(); it.Next()) {
      frames.push_back(Frame(it.is_key(), it.start_90k(), it.duration_90k()));
    }
    return frames;
  }
#endif

  MockUuidGenerator uuidgen_;
  ShutdownSignal signal_;
  SimulatedClock clock_;
  testing::StrictMock<MockVideoSource> video_source_;
  Database db_;
  MoonfireDatabase mdb_;
  std::unique_ptr<moonfire_nvr::File> sample_file_dir_;
  Environment env_;
  std::string test_dir_;
  std::unique_ptr<Stream> stream_;
};

class ProxyingInputVideoPacketStream : public InputVideoPacketStream {
 public:
  explicit ProxyingInputVideoPacketStream(
      std::unique_ptr<InputVideoPacketStream> base, SimulatedClock *clock)
      : base_(std::move(base)), clock_(clock) {}

  bool GetNext(VideoPacket *pkt, std::string *error_message) final {
    if (pkts_left_-- == 0) {
      *error_message = "(pkt limit reached)";
      return false;
    }

    // Advance time to when this packet starts.
    clock_->Sleep(SecToTimespec(last_duration_sec_));
    if (!base_->GetNext(pkt, error_message)) {
      return false;
    }
    last_duration_sec_ =
        pkt->pkt()->duration * av_q2d(base_->stream()->time_base);

    // Adjust timestamps.
    if (ts_offset_pkts_left_ > 0) {
      pkt->pkt()->pts += ts_offset_;
      pkt->pkt()->dts += ts_offset_;
      --ts_offset_pkts_left_;
    }

    // Use a fixed duration, as the duration from a real RTSP stream is only
    // an estimate. Our test video is 1 fps, 90 kHz time base.
    pkt->pkt()->duration = 90000;

    return true;
  }

  const AVStream *stream() const final { return base_->stream(); }

  void set_ts_offset(int64_t offset, int pkts) {
    ts_offset_ = offset;
    ts_offset_pkts_left_ = pkts;
  }

  void set_pkts(int num) { pkts_left_ = num; }

 private:
  std::unique_ptr<InputVideoPacketStream> base_;
  SimulatedClock *clock_ = nullptr;
  double last_duration_sec_ = 0.;
  int64_t ts_offset_ = 0;
  int ts_offset_pkts_left_ = 0;
  int pkts_left_ = std::numeric_limits<int>::max();
};

TEST_F(StreamTest, Basic) {
  std::string error_message;

  // This is a ~1 fps test video with a timebase of 90 kHz.
  auto in_stream = GetRealVideoSource()->OpenFile("../src/testdata/clip.mp4",
                                                  &error_message);
  ASSERT_TRUE(in_stream != nullptr) << error_message;
  auto *proxy_stream =
      new ProxyingInputVideoPacketStream(std::move(in_stream), &clock_);

  // The starting pts of the input should be irrelevant.
  proxy_stream->set_ts_offset(180000, std::numeric_limits<int>::max());

  Uuid uuid1;
  ASSERT_TRUE(uuid1.ParseText("00000000-0000-0000-0000-000000000001"));
  Uuid uuid2;
  ASSERT_TRUE(uuid2.ParseText("00000000-0000-0000-0000-000000000002"));
  EXPECT_CALL(uuidgen_, Generate())
      .WillOnce(Return(uuid1))
      .WillOnce(Return(uuid2));

  EXPECT_CALL(video_source_, OpenRtspRaw("rtsp://foo:bar@test-camera/main", _))
      .WillOnce(Return(proxy_stream))
      .WillOnce(Invoke(this, &StreamTest::Shutdown));
  stream_->Run();
  // Compare frame-by-frame.
  // Note below that while the rotation is scheduled to happen near 5-second
  // boundaries (such as 2016-04-26 00:00:05), it gets deferred until the next
  // key frame, which in this case is 00:00:07.
  EXPECT_THAT(GetFrames("00000000-0000-0000-0000-000000000001"),
              testing::ElementsAre(
                  Frame(true, 0, 90379), Frame(false, 90379, 89884),
                  Frame(false, 180263, 89749), Frame(false, 270012, 89981),
                  Frame(true, 359993, 90055),
                  Frame(false, 450048,
                        89967),  // pts_time 5.000533, past rotation time.
                  Frame(false, 540015, 90021),
                  Frame(false, 630036, 89958)));
  EXPECT_THAT(
      GetFrames("00000000-0000-0000-0000-000000000002"),
      testing::ElementsAre(Frame(true, 0, 90011), Frame(false, 90011, 0)));
}

TEST_F(StreamTest, NonIncreasingTimestamp) {
  std::string error_message;
  auto in_stream = GetRealVideoSource()->OpenFile("../src/testdata/clip.mp4",
                                                  &error_message);
  ASSERT_TRUE(in_stream != nullptr) << error_message;
  auto *proxy_stream =
      new ProxyingInputVideoPacketStream(std::move(in_stream), &clock_);
  proxy_stream->set_ts_offset(12345678, 1);
  EXPECT_CALL(video_source_, OpenRtspRaw("rtsp://foo:bar@test-camera/main", _))
      .WillOnce(Return(proxy_stream))
      .WillOnce(Invoke(this, &StreamTest::Shutdown));

  Uuid uuid1;
  ASSERT_TRUE(uuid1.ParseText("00000000-0000-0000-0000-000000000001"));
  EXPECT_CALL(uuidgen_, Generate()).WillOnce(Return(uuid1));

  {
    ScopedMockLog log;
    EXPECT_CALL(log, Log(_, _, _)).Times(AnyNumber());
    EXPECT_CALL(log,
                Log(_, _, HasSubstr("Rejecting non-increasing pts=90379")));
    log.Start();
    stream_->Run();
  }

  // The output file should still be added to the file manager, with the one
  // packet that made it. The final packet on input error will have 0
  // duration.
  EXPECT_THAT(GetFrames("00000000-0000-0000-0000-000000000001"),
              testing::ElementsAre(Frame(true, 0, 0)));
}

TEST_F(StreamTest, RetryOnInputError) {
  std::string error_message;
  auto in_stream_1 = GetRealVideoSource()->OpenFile("../src/testdata/clip.mp4",
                                                    &error_message);
  ASSERT_TRUE(in_stream_1 != nullptr) << error_message;
  auto *proxy_stream_1 =
      new ProxyingInputVideoPacketStream(std::move(in_stream_1), &clock_);
  proxy_stream_1->set_pkts(1);

  auto in_stream_2 = GetRealVideoSource()->OpenFile("../src/testdata/clip.mp4",
                                                    &error_message);
  ASSERT_TRUE(in_stream_2 != nullptr) << error_message;
  auto *proxy_stream_2 =
      new ProxyingInputVideoPacketStream(std::move(in_stream_2), &clock_);
  proxy_stream_2->set_pkts(1);

  EXPECT_CALL(video_source_, OpenRtspRaw("rtsp://foo:bar@test-camera/main", _))
      .WillOnce(Return(proxy_stream_1))
      .WillOnce(Return(proxy_stream_2))
      .WillOnce(Invoke(this, &StreamTest::Shutdown));

  Uuid uuid1;
  ASSERT_TRUE(uuid1.ParseText("00000000-0000-0000-0000-000000000001"));
  Uuid uuid2;
  ASSERT_TRUE(uuid2.ParseText("00000000-0000-0000-0000-000000000002"));
  EXPECT_CALL(uuidgen_, Generate())
      .WillOnce(Return(uuid1))
      .WillOnce(Return(uuid2));
  stream_->Run();

  // Each attempt should have resulted in a file with one packet.
  EXPECT_THAT(GetFrames("00000000-0000-0000-0000-000000000001"),
              testing::ElementsAre(Frame(true, 0, 0)));
  EXPECT_THAT(GetFrames("00000000-0000-0000-0000-000000000002"),
              testing::ElementsAre(Frame(true, 0, 0)));
}

TEST_F(StreamTest, DiscardInitialNonKeyFrames) {
  std::string error_message;
  auto in_stream = GetRealVideoSource()->OpenFile("../src/testdata/clip.mp4",
                                                  &error_message);
  ASSERT_TRUE(in_stream != nullptr) << error_message;

  // Discard the initial key frame packet.
  VideoPacket dummy;
  ASSERT_TRUE(in_stream->GetNext(&dummy, &error_message)) << error_message;

  auto *proxy_stream =
      new ProxyingInputVideoPacketStream(std::move(in_stream), &clock_);
  EXPECT_CALL(video_source_, OpenRtspRaw("rtsp://foo:bar@test-camera/main", _))
      .WillOnce(Return(proxy_stream))
      .WillOnce(Invoke(this, &StreamTest::Shutdown));

  Uuid uuid1;
  ASSERT_TRUE(uuid1.ParseText("00000000-0000-0000-0000-000000000001"));
  Uuid uuid2;
  ASSERT_TRUE(uuid2.ParseText("00000000-0000-0000-0000-000000000002"));
  EXPECT_CALL(uuidgen_, Generate())
      .WillOnce(Return(uuid1))
      .WillOnce(Return(uuid2));
  stream_->Run();

  // Skipped: initial key frame packet (duration 90379)
  // Ignored: duration 89884, 89749, 89981 (total pts time: 2.99571... sec)
  // Thus, the first output file should start at 00:00:02.
  EXPECT_THAT(
      GetFrames("00000000-0000-0000-0000-000000000001"),
      testing::ElementsAre(
          Frame(true, 0, 90055),
          Frame(false, 90055, 89967),  // pts_time 5.000533, past rotation time.
          Frame(false, 180022, 90021), Frame(false, 270043, 89958)));
  EXPECT_THAT(
      GetFrames("00000000-0000-0000-0000-000000000002"),
      testing::ElementsAre(Frame(true, 0, 90011), Frame(false, 90011, 0)));
}

// TODO: test output stream error (on open, writing packet, closing).
// TODO: test rotation!

}  // namespace
}  // namespace moonfire_nvr

int main(int argc, char **argv) {
  FLAGS_alsologtostderr = true;
  google::ParseCommandLineFlags(&argc, &argv, true);
  testing::InitGoogleTest(&argc, argv);
  google::InitGoogleLogging(argv[0]);
  return RUN_ALL_TESTS();
}
