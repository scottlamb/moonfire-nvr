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
// moonfire-db.h: database access logic for the Moonfire NVR SQLite schema.
// Currently focused on stuff needed by WebInterface to build a HTML or JSON
// interface.
//
// This caches data in RAM, making the assumption that only one process is
// accessing the database at a time. (TODO: enforce with flock or some such.)
// Performance and efficiency notes:
//
// * several query operations here feature row callbacks. The callback is
//   invoked with the database lock. Thus, the caller mustn't perform database
//   operations or other long-running operations.
//
// * startup may be slow, as it scans the entire index for the recording
//   table. This seems acceptable.
//
// * the operations used for web file serving should return results with
//   acceptable latency.
//
// * however, the database lock may be held for longer than is acceptable for
//   the critical path of recording frames. It may be necessary to preallocate
//   sample file uuids and such to avoid this.
//
// * the caller may need to perform several different types of write
//   operations in a row. It might be worth creating an interface for batching
//   these inside a transaction, to reduce latency and SSD write cycles. The
//   pre-commit and post-commit logic of each operation would have to be
//   pulled apart, with the latter being called by this wrapper class on
//   commit of the overall transaction.

#ifndef MOONFIRE_NVR_MOONFIRE_DB_H
#define MOONFIRE_NVR_MOONFIRE_DB_H

#include <functional>
#include <memory>
#include <string>
#include <vector>

#include "common.h"
#include "http.h"
#include "mp4.h"
#include "sqlite.h"
#include "uuid.h"

namespace moonfire_nvr {

// For use with MoonfireDatabase::ListCameras.
struct ListCamerasRow {
  int64_t id = -1;
  Uuid uuid;
  std::string short_name;
  std::string description;
  std::string host;
  std::string username;
  std::string password;
  std::string main_rtsp_path;
  std::string sub_rtsp_path;
  int64_t retain_bytes = -1;

  // Aggregates summarizing completed recordings.
  int64_t min_start_time_90k = -1;
  int64_t max_end_time_90k = -1;
  int64_t total_duration_90k = -1;
  int64_t total_sample_file_bytes = -1;
};

// For use with MoonfireDatabase::GetCamera.
// This includes everything in ListCamerasRow. In the future, it will include
// more data. Likely, that will mean a list of calendar days (in the system
// time zone) in which there is any data.
struct GetCameraRow {
  std::string short_name;
  std::string description;
  int64_t retain_bytes = -1;
  int64_t min_start_time_90k = -1;
  int64_t max_end_time_90k = -1;
  int64_t total_duration_90k = -1;
  int64_t total_sample_file_bytes = -1;
  std::map<std::string, int64_t> days;  // YYYY-mm-dd -> duration_90k.
};

// For use with MoonfireDatabase::ListCameraRecordings.
struct ListCameraRecordingsRow {
  // From the recording table.
  int64_t start_time_90k = -1;
  int64_t end_time_90k = -1;
  int64_t video_samples = -1;
  int64_t sample_file_bytes = -1;

  // Joined from the video_sample_entry table.
  // |video_sample_entry_sha1| is valid as long as the MoonfireDatabase.
  re2::StringPiece video_sample_entry_sha1;
  uint16_t width = 0;
  uint16_t height = 0;
};

// For use with MoonfireDatabase::ListOldestSampleFiles.
struct ListOldestSampleFilesRow {
  int64_t camera_id = -1;
  int64_t recording_id = -1;
  Uuid sample_file_uuid;
  int64_t start_time_90k = -1;
  int64_t duration_90k = -1;
  int64_t sample_file_bytes = -1;
};

// Thread-safe after Init.
// (Uses a DatabaseContext for locking.)
class MoonfireDatabase {
 public:
  MoonfireDatabase() {}
  MoonfireDatabase(const MoonfireDatabase &) = delete;
  void operator=(const MoonfireDatabase &) = delete;

  // |db| must outlive the MoonfireDatabase.
  bool Init(Database *db, std::string *error_message);

  // List all cameras in the system, ordered by short name.
  void ListCameras(std::function<IterationControl(const ListCamerasRow &)> cb);

  // Get a single camera.
  // Return true iff the camera exists.
  bool GetCamera(Uuid camera_uuid, GetCameraRow *row);

  // List all recordings associated with a camera, descending by end time.
  bool ListCameraRecordings(
      Uuid camera_uuid, int64_t start_time_90k, int64_t end_time_90k,
      std::function<IterationControl(const ListCameraRecordingsRow &)>,
      std::string *error_message);

  bool ListMp4Recordings(
      Uuid camera_uuid, int64_t start_time_90k, int64_t end_time_90k,
      std::function<IterationControl(Recording &, const VideoSampleEntry &)>
          row_cb,
      std::string *error_message);

  bool ListReservedSampleFiles(std::vector<Uuid> *reserved,
                               std::string *error_message);

  // Reserve |n| new sample file uuids.
  // Returns an empty vector on error.
  std::vector<Uuid> ReserveSampleFiles(int n, std::string *error_message);

  // Insert a video sample entry if not already inserted.
  // On success, |entry->id| is filled in with the id of a freshly-created or
  // existing row.
  bool InsertVideoSampleEntry(VideoSampleEntry *entry,
                              std::string *error_message);

  // Insert a new recording.
  // The uuid must have been already reserved with ReserveSampleFileUuid above.
  // On success, |recording->id| is filled in.
  bool InsertRecording(Recording *recording, std::string *error_message);

  // List sample files, starting from the oldest.
  // The caller is expected to supply a |row_cb| that returns kBreak when
  // enough have been listed.
  bool ListOldestSampleFiles(
      Uuid camera_uuid,
      std::function<IterationControl(const ListOldestSampleFilesRow &)> row_cb,
      std::string *error_message);

  // Delete recording rows, moving their sample file uuids to the deleting
  // state.
  bool DeleteRecordings(const std::vector<ListOldestSampleFilesRow> &rows,
                        std::string *error_message);

  // Mark a set of sample files as deleted.
  // This shouldn't be called until the files have been unlinke()ed and the
  // parent directory fsync()ed.
  // Returns error if any sample files are not in the deleting state.
  bool MarkSampleFilesDeleted(const std::vector<Uuid> &uuids,
                              std::string *error_message);

  // Replace the default real UUID generator with the supplied one.
  // Exposed only for testing; not thread-safe.
  void SetUuidGeneratorForTesting(UuidGenerator *uuidgen) {
    uuidgen_ = uuidgen;
  }

 private:
  struct CameraData {
    // Cached values of the matching fields from the camera row.
    int64_t id = -1;
    std::string short_name;
    std::string description;
    std::string host;
    std::string username;
    std::string password;
    std::string main_rtsp_path;
    std::string sub_rtsp_path;
    int64_t retain_bytes = -1;

    // Aggregates of all recordings associated with the camera.
    int64_t min_start_time_90k = std::numeric_limits<int64_t>::max();
    int64_t max_end_time_90k = std::numeric_limits<int64_t>::min();
    int64_t total_sample_file_bytes = 0;
    int64_t total_duration_90k = 0;

    // A map of calendar days (in the local timezone, "YYYY-mm-DD") to the
    // total duration (in 90k units) of recorded data in the day. A day is
    // present in the map ff the value is non-zero.
    std::map<std::string, int64_t> days;
  };

  enum class ReservationState { kWriting = 0, kDeleting = 1 };

  // Efficiently (re-)compute the bounds of recorded time for a given camera.
  bool ComputeCameraRecordingBounds(DatabaseContext *ctx, int64_t camera_id,
                                    int64_t *min_start_time_90k,
                                    int64_t *max_end_time_90k,
                                    std::string *error_message);

  Database *db_ = nullptr;
  UuidGenerator *uuidgen_ = GetRealUuidGenerator();
  Statement list_camera_recordings_stmt_;
  Statement build_mp4_stmt_;
  Statement insert_reservation_stmt_;
  Statement delete_reservation_stmt_;
  Statement insert_video_sample_entry_stmt_;
  Statement insert_recording_stmt_;
  Statement list_oldest_sample_files_stmt_;
  Statement delete_recording_stmt_;
  Statement camera_min_start_stmt_;
  Statement camera_max_start_stmt_;

  std::map<Uuid, CameraData> cameras_by_uuid_;
  std::map<int64_t, CameraData *> cameras_by_id_;
  std::map<int64_t, VideoSampleEntry> video_sample_entries_;
};

// Given a key in the day-to-duration map, produce the start and end times of
// the day. (Typically the end time is 24 hours later than the start; but it's
// 23 or 25 hours for the days of spring forward and fall back, respectively.)
bool GetDayBounds(const std::string &day, int64_t *start_time_90k,
                  int64_t *end_time_90k, std::string *error_message);

namespace internal {

// Adjust a day-to-duration map (see MoonfireDatabase::CameraData::days_)
// to reflect a recording.
void AdjustDaysMap(int64_t start_time_90k, int64_t end_time_90k, int sign,
                   std::map<std::string, int64_t> *days);

}  // namespace internal

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_MOONFIRE_DB_H
