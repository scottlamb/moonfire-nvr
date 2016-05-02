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
// moonfire-db-test.cc: tests of the moonfire-db.h interface.

#include <time.h>

#include <string>

#include <gflags/gflags.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "moonfire-db.h"
#include "sqlite.h"
#include "string.h"
#include "testutil.h"

DECLARE_bool(alsologtostderr);

using testing::_;
using testing::HasSubstr;
using testing::DoAll;
using testing::Return;
using testing::SetArgPointee;

namespace moonfire_nvr {
namespace {

class MoonfireDbTest : public testing::Test {
 protected:
  MoonfireDbTest() {
    tmpdir_ = PrepareTempDirOrDie("moonfire-db-test");
    std::string error_message;
    CHECK(db_.Open(StrCat(tmpdir_, "/db").c_str(),
                   SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, &error_message))
        << error_message;
    std::string create_sql = ReadFileOrDie("../src/schema.sql");
    DatabaseContext ctx(&db_);
    CHECK(RunStatements(&ctx, create_sql, &error_message)) << error_message;
  }

  int64_t AddCamera(Uuid uuid, re2::StringPiece short_name) {
    DatabaseContext ctx(&db_);
    auto run = ctx.UseOnce(
        R"(
        insert into camera (uuid,  short_name,  host,  username,  password,
                            main_rtsp_path,  sub_rtsp_path,  retain_bytes)
                    values (:uuid, :short_name, :host, :username, :password,
                            :main_rtsp_path, :sub_rtsp_path, :retain_bytes);
        )");
    run.BindBlob(":uuid", uuid.binary_view());
    run.BindText(":short_name", short_name);
    run.BindText(":host", "test-camera");
    run.BindText(":username", "foo");
    run.BindText(":password", "bar");
    run.BindText(":main_rtsp_path", "/main");
    run.BindText(":sub_rtsp_path", "/sub");
    run.BindInt64(":retain_bytes", 42);
    CHECK_EQ(SQLITE_DONE, run.Step()) << run.error_message();
    if (run.Step() != SQLITE_DONE) {
      ADD_FAILURE() << run.error_message();
      return -1;
    }
    return ctx.last_insert_rowid();
  }

  void ExpectNoRecordings(Uuid camera_uuid) {
    int rows = 0;
    mdb_->ListCameras([&](const ListCamerasRow &row) {
      ++rows;
      EXPECT_EQ(camera_uuid, row.uuid);
      EXPECT_EQ("test-camera", row.host);
      EXPECT_EQ("foo", row.username);
      EXPECT_EQ("bar", row.password);
      EXPECT_EQ("/main", row.main_rtsp_path);
      EXPECT_EQ("/sub", row.sub_rtsp_path);
      EXPECT_EQ(42, row.retain_bytes);
      EXPECT_EQ(-1, row.min_start_time_90k);
      EXPECT_EQ(-1, row.max_end_time_90k);
      EXPECT_EQ(0, row.total_duration_90k);
      EXPECT_EQ(0, row.total_sample_file_bytes);
      return IterationControl::kContinue;
    });
    EXPECT_EQ(1, rows);

    std::string error_message;
    rows = 0;
    EXPECT_TRUE(mdb_->ListCameraRecordings(
        camera_uuid, 0, std::numeric_limits<int64_t>::max(),
        [&](const ListCameraRecordingsRow &row) {
          ++rows;
          return IterationControl::kBreak;
        },
        &error_message))
        << error_message;
    EXPECT_EQ(0, rows);

    rows = 0;
    EXPECT_TRUE(mdb_->ListMp4Recordings(
        camera_uuid, 0, std::numeric_limits<int64_t>::max(),
        [&](Recording &recording, const VideoSampleEntry &entry) {
          ++rows;
          return IterationControl::kBreak;
        },
        &error_message))
        << error_message;
    EXPECT_EQ(0, rows);
  }

  void ExpectSingleRecording(Uuid camera_uuid, const Recording &recording,
                             const VideoSampleEntry &entry,
                             ListOldestSampleFilesRow *save_oldest_row) {
    std::string error_message;
    int rows = 0;
    mdb_->ListCameras([&](const ListCamerasRow &row) {
      ++rows;
      EXPECT_EQ(camera_uuid, row.uuid);
      EXPECT_EQ(recording.start_time_90k, row.min_start_time_90k);
      EXPECT_EQ(recording.end_time_90k, row.max_end_time_90k);
      EXPECT_EQ(recording.end_time_90k - recording.start_time_90k,
                row.total_duration_90k);
      EXPECT_EQ(recording.sample_file_bytes, row.total_sample_file_bytes);
      return IterationControl::kContinue;
    });
    EXPECT_EQ(1, rows);

    GetCameraRow camera_row;
    EXPECT_TRUE(mdb_->GetCamera(camera_uuid, &camera_row));
    EXPECT_EQ(recording.start_time_90k, camera_row.min_start_time_90k);
    EXPECT_EQ(recording.end_time_90k, camera_row.max_end_time_90k);
    EXPECT_EQ(recording.end_time_90k - recording.start_time_90k,
              camera_row.total_duration_90k);
    EXPECT_EQ(recording.sample_file_bytes, camera_row.total_sample_file_bytes);

    rows = 0;
    EXPECT_TRUE(mdb_->ListCameraRecordings(
        camera_uuid, 0, std::numeric_limits<int64_t>::max(),
        [&](const ListCameraRecordingsRow &row) {
          ++rows;
          EXPECT_EQ(recording.start_time_90k, row.start_time_90k);
          EXPECT_EQ(recording.end_time_90k, row.end_time_90k);
          EXPECT_EQ(recording.video_samples, row.video_samples);
          EXPECT_EQ(recording.sample_file_bytes, row.sample_file_bytes);
          EXPECT_EQ(entry.sha1, row.video_sample_entry_sha1);
          EXPECT_EQ(entry.width, row.width);
          EXPECT_EQ(entry.height, row.height);
          return IterationControl::kContinue;
        },
        &error_message))
        << error_message;
    EXPECT_EQ(1, rows);

    rows = 0;
    EXPECT_TRUE(mdb_->ListOldestSampleFiles(
        camera_uuid,
        [&](const ListOldestSampleFilesRow &row) {
          ++rows;
          EXPECT_EQ(recording.id, row.recording_id);
          EXPECT_EQ(recording.sample_file_uuid, row.sample_file_uuid);
          EXPECT_EQ(recording.end_time_90k - recording.start_time_90k,
                    row.duration_90k);
          EXPECT_EQ(recording.sample_file_bytes, row.sample_file_bytes);
          *save_oldest_row = row;
          return IterationControl::kContinue;
        },
        &error_message))
        << error_message;
    EXPECT_EQ(1, rows);

    rows = 0;
    EXPECT_TRUE(mdb_->ListMp4Recordings(
        camera_uuid, 0, std::numeric_limits<int64_t>::max(),
        [&](Recording &some_recording, const VideoSampleEntry &some_entry) {
          ++rows;

          EXPECT_EQ(recording.id, some_recording.id);
          EXPECT_EQ(recording.camera_id, some_recording.camera_id);
          EXPECT_EQ(recording.sample_file_sha1,
                    some_recording.sample_file_sha1);
          EXPECT_EQ(recording.sample_file_uuid,
                    some_recording.sample_file_uuid);
          EXPECT_EQ(recording.video_sample_entry_id,
                    some_recording.video_sample_entry_id);
          EXPECT_EQ(recording.start_time_90k, some_recording.start_time_90k);
          EXPECT_EQ(recording.end_time_90k, some_recording.end_time_90k);
          EXPECT_EQ(recording.sample_file_bytes,
                    some_recording.sample_file_bytes);
          EXPECT_EQ(recording.video_samples, some_recording.video_samples);
          EXPECT_EQ(recording.video_sync_samples,
                    some_recording.video_sync_samples);
          EXPECT_EQ(recording.video_index, some_recording.video_index);

          EXPECT_EQ(entry.id, some_entry.id);
          EXPECT_EQ(entry.sha1, some_entry.sha1);
          EXPECT_EQ(entry.data, some_entry.data);
          EXPECT_EQ(entry.width, some_entry.width);
          EXPECT_EQ(entry.height, some_entry.height);

          return IterationControl::kContinue;
        },
        &error_message))
        << error_message;
    EXPECT_EQ(1, rows);
  }

  std::string tmpdir_;
  Database db_;
  std::unique_ptr<MoonfireDatabase> mdb_;
};

TEST(AdjustDaysMapTest, Basic) {
  std::map<std::string, int64_t> days;

  // Create a day.
  const int64_t kTestTime = INT64_C(130647162600000);  // 2015-12-31 23:59:00
  moonfire_nvr::internal::AdjustDaysMap(
      kTestTime, kTestTime + 60 * kTimeUnitsPerSecond, 1, &days);
  EXPECT_THAT(days, testing::ElementsAre(std::make_pair(
                        "2015-12-31", 60 * kTimeUnitsPerSecond)));

  // Add to a day.
  moonfire_nvr::internal::AdjustDaysMap(
      kTestTime, kTestTime + 60 * kTimeUnitsPerSecond, 1, &days);
  EXPECT_THAT(days, testing::ElementsAre(std::make_pair(
                        "2015-12-31", 120 * kTimeUnitsPerSecond)));

  // Subtract from a day.
  moonfire_nvr::internal::AdjustDaysMap(
      kTestTime, kTestTime + 60 * kTimeUnitsPerSecond, -1, &days);
  EXPECT_THAT(days, testing::ElementsAre(std::make_pair(
                        "2015-12-31", 60 * kTimeUnitsPerSecond)));

  // Remove a day.
  moonfire_nvr::internal::AdjustDaysMap(
      kTestTime, kTestTime + 60 * kTimeUnitsPerSecond, -1, &days);
  EXPECT_THAT(days, testing::ElementsAre());

  // Create two days.
  moonfire_nvr::internal::AdjustDaysMap(
      kTestTime, kTestTime + 3 * 60 * kTimeUnitsPerSecond, 1, &days);
  EXPECT_THAT(days,
              testing::ElementsAre(
                  std::make_pair("2015-12-31", 1 * 60 * kTimeUnitsPerSecond),
                  std::make_pair("2016-01-01", 2 * 60 * kTimeUnitsPerSecond)));

  // Add to two days.
  moonfire_nvr::internal::AdjustDaysMap(
      kTestTime, kTestTime + 3 * 60 * kTimeUnitsPerSecond, 1, &days);
  EXPECT_THAT(days,
              testing::ElementsAre(
                  std::make_pair("2015-12-31", 2 * 60 * kTimeUnitsPerSecond),
                  std::make_pair("2016-01-01", 4 * 60 * kTimeUnitsPerSecond)));

  // Subtract from two days.
  moonfire_nvr::internal::AdjustDaysMap(
      kTestTime, kTestTime + 3 * 60 * kTimeUnitsPerSecond, -1, &days);
  EXPECT_THAT(days,
              testing::ElementsAre(
                  std::make_pair("2015-12-31", 1 * 60 * kTimeUnitsPerSecond),
                  std::make_pair("2016-01-01", 2 * 60 * kTimeUnitsPerSecond)));

  // Remove two days.
  moonfire_nvr::internal::AdjustDaysMap(
      kTestTime, kTestTime + 3 * 60 * kTimeUnitsPerSecond, -1, &days);
  EXPECT_THAT(days, testing::ElementsAre());
}

// Basic test of running some queries on an empty database.
TEST_F(MoonfireDbTest, EmptyDatabase) {
  std::string error_message;
  mdb_.reset(new MoonfireDatabase);
  ASSERT_TRUE(mdb_->Init(&db_, &error_message)) << error_message;

  mdb_->ListCameras([&](const ListCamerasRow &row) {
    ADD_FAILURE() << "row unexpected";
    return IterationControl::kBreak;
  });

  GetCameraRow get_camera_row;
  EXPECT_FALSE(mdb_->GetCamera(Uuid(), &get_camera_row));

  EXPECT_FALSE(
      mdb_->ListCameraRecordings(Uuid(), 0, std::numeric_limits<int64_t>::max(),
                                 [&](const ListCameraRecordingsRow &row) {
                                   ADD_FAILURE() << "row unexpected";
                                   return IterationControl::kBreak;
                                 },
                                 &error_message));

  EXPECT_FALSE(mdb_->ListMp4Recordings(
      Uuid(), 0, std::numeric_limits<int64_t>::max(),
      [&](Recording &recording, const VideoSampleEntry &entry) {
        ADD_FAILURE() << "row unexpected";
        return IterationControl::kBreak;
      },
      &error_message));
}

// Basic test of the full lifecycle of recording.
// Does not exercise many error cases.
TEST_F(MoonfireDbTest, FullLifecycle) {
  std::string error_message;
  const char kCameraShortName[] = "testcam";
  Uuid camera_uuid = GetRealUuidGenerator()->Generate();
  int64_t camera_id = AddCamera(camera_uuid, kCameraShortName);
  ASSERT_GT(camera_id, 0);
  mdb_.reset(new MoonfireDatabase);
  ASSERT_TRUE(mdb_->Init(&db_, &error_message)) << error_message;

  ExpectNoRecordings(camera_uuid);

  std::vector<Uuid> reserved;
  EXPECT_TRUE(mdb_->ListReservedSampleFiles(&reserved, &error_message))
      << error_message;
  EXPECT_THAT(reserved, testing::IsEmpty());

  std::vector<Uuid> uuids = mdb_->ReserveSampleFiles(2, &error_message);
  ASSERT_THAT(uuids, testing::SizeIs(2)) << error_message;

  EXPECT_TRUE(mdb_->ListReservedSampleFiles(&reserved, &error_message))
      << error_message;
  EXPECT_THAT(reserved, testing::UnorderedElementsAre(uuids[0], uuids[1]));

  VideoSampleEntry entry;
  entry.sha1.resize(20);
  entry.width = 768;
  entry.height = 512;
  entry.data.resize(100);
  ASSERT_TRUE(mdb_->InsertVideoSampleEntry(&entry, &error_message))
      << error_message;
  ASSERT_GT(entry.id, 0);

  Recording recording;
  recording.camera_id = camera_id;
  recording.sample_file_uuid = GetRealUuidGenerator()->Generate();
  recording.video_sample_entry_id = entry.id;
  SampleIndexEncoder encoder;
  encoder.Init(&recording, UINT64_C(1430006400) * kTimeUnitsPerSecond);
  encoder.AddSample(kTimeUnitsPerSecond, 42, true);

  // Inserting a recording should succeed and remove its uuid from the
  // reserved table.
  ASSERT_FALSE(mdb_->InsertRecording(&recording, &error_message));
  EXPECT_THAT(error_message, testing::HasSubstr("not reserved"));
  recording.sample_file_uuid = uuids.back();
  recording.sample_file_sha1.resize(20);
  ASSERT_TRUE(mdb_->InsertRecording(&recording, &error_message))
      << error_message;
  ASSERT_GT(recording.id, 0);
  EXPECT_TRUE(mdb_->ListReservedSampleFiles(&reserved, &error_message))
      << error_message;
  EXPECT_THAT(reserved, testing::ElementsAre(uuids[0]));

  // Queries should return the correct result (with caches updated on insert).
  ListOldestSampleFilesRow oldest;
  ExpectSingleRecording(camera_uuid, recording, entry, &oldest);

  // Queries on a fresh database should return the correct result (with caches
  // populated from existing database contents).
  mdb_.reset(new MoonfireDatabase);
  ASSERT_TRUE(mdb_->Init(&db_, &error_message)) << error_message;
  ExpectSingleRecording(camera_uuid, recording, entry, &oldest);

  // Deleting a recording should succeed, update the min/max times, and mark
  // the uuid as reserved.
  std::vector<ListOldestSampleFilesRow> to_delete;
  to_delete.push_back(oldest);
  ASSERT_TRUE(mdb_->DeleteRecordings(to_delete, &error_message))
      << error_message;
  EXPECT_TRUE(mdb_->ListReservedSampleFiles(&reserved, &error_message))
      << error_message;
  EXPECT_THAT(reserved, testing::UnorderedElementsAre(uuids[0], uuids[1]));
  ExpectNoRecordings(camera_uuid);

  EXPECT_TRUE(mdb_->MarkSampleFilesDeleted(uuids, &error_message))
      << error_message;
  EXPECT_TRUE(mdb_->ListReservedSampleFiles(&reserved, &error_message))
      << error_message;
  EXPECT_THAT(reserved, testing::IsEmpty());
}

}  // namespace
}  // namespace moonfire_nvr

int main(int argc, char **argv) {
  FLAGS_alsologtostderr = true;
  google::ParseCommandLineFlags(&argc, &argv, true);
  testing::InitGoogleTest(&argc, argv);
  google::InitGoogleLogging(argv[0]);

  // The calendar day math assumes this timezone.
  CHECK_EQ(0, setenv("TZ", "America/Los_Angeles", 1)) << strerror(errno);
  tzset();

  return RUN_ALL_TESTS();
}
