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
// Performance note: camera-level operations do a sequential scan through
// essentially the entire database. This is unacceptable for full-sized
// databases; it will have to be measured and improved. Ideas:
//
// * separate the video index blob from the rest of the recording row,
//   as it's expected to be 10X-100X larger than everything else and not
//   necessary for these operations.
// * paged results + SQL indexes (but this may only help so much, as it'd be
//   useful to at least see what days have recordings in one go).
// * keep aggregates, either in-memory or as denormalized data in the camera
//   table. Likely integrating with the recording system, although triggers
//   may also be possible.

#ifndef MOONFIRE_NVR_MOONFIRE_DB_H
#define MOONFIRE_NVR_MOONFIRE_DB_H

#include <functional>
#include <memory>
#include <string>

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
  int64_t retain_bytes = -1;

  // Aggregates summarizing completed (status=1) recordings.
  int64_t min_recording_start_time_90k = -1;
  int64_t max_recording_end_time_90k = -1;
  int64_t total_recording_duration_90k = -1;
  int64_t total_sample_file_bytes = -1;
};

// For use with MoonfireDatabase::GetCamera.
// This is the same information as in ListCamerasRow minus the stuff
// that's calculable from ListCameraRecordingsRow, which the camera details
// webpage also grabs.
struct GetCameraRow {
  int64_t retain_bytes = -1;
  Uuid uuid;
  std::string short_name;
  std::string description;
};

// For use with MoonfireDatabase::ListCameraRecordings.
struct ListCameraRecordingsRow {
  // From the recording table.
  int64_t start_time_90k = -1;
  int64_t end_time_90k = -1;
  int64_t video_samples = -1;
  int64_t sample_file_bytes = -1;
  std::string video_sample_entry_sha1;

  // Joined from the video_sample_entry table.
  int64_t width = -1;
  int64_t height = -1;
};

class MoonfireDatabase {
 public:
  explicit MoonfireDatabase(Database *db) : db_(db) {}
  MoonfireDatabase(const MoonfireDatabase &) = delete;
  void operator=(const MoonfireDatabase &) = delete;

  bool Init(std::string *error_message);

  // List all cameras in the system, ordered by short name.
  // Holds database lock; callback should be quick.
  bool ListCameras(std::function<IterationControl(const ListCamerasRow &)> cb,
                   std::string *error_message);

  bool GetCamera(int64_t camera_id, GetCameraRow *row,
                 std::string *error_message);

  // List all recordings associated with a camera, ordered by start time..
  // Holds database lock; callback should be quick.
  bool ListCameraRecordings(
      int64_t camera_id,
      std::function<IterationControl(const ListCameraRecordingsRow &)>,
      std::string *error_message);

  std::shared_ptr<VirtualFile> BuildMp4(int64_t camera_id,
                                        int64_t start_time_90k,
                                        int64_t end_time_90k,
                                        std::string *error_message);

 private:
  Database *const db_;
  Statement list_cameras_query_;
  Statement get_camera_query_;
  Statement list_camera_recordings_query_;
  Statement build_mp4_query_;
};

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_MOONFIRE_DB_H
