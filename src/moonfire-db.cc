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
// moonfire-db.cc: implementation of moonfire-db.h interface.
// see top-level comments there on performance & efficiency.

#include "moonfire-db.h"

#include <string>

#include <glog/logging.h>

#include "http.h"
#include "mp4.h"
#include "recording.h"

namespace moonfire_nvr {

bool MoonfireDatabase::Init(Database *db, std::string *error_message) {
  CHECK(db_ == nullptr);
  db_ = db;

  {
    DatabaseContext ctx(db_);

    // This query scans the entirety of the recording table's index.
    // It is quite slow, so the results are cached.
    auto list_cameras_run = ctx.UseOnce(
        R"(
        select
          camera.id,
          camera.uuid,
          camera.short_name,
          camera.description,
          camera.host,
          camera.username,
          camera.password,
          camera.main_rtsp_path,
          camera.sub_rtsp_path,
          camera.retain_bytes,
          min(recording.start_time_90k),
          max(recording.start_time_90k + recording.duration_90k),
          sum(recording.duration_90k),
          sum(recording.sample_file_bytes)
        from
          camera
          left join recording on (camera.id = recording.camera_id)
        group by
          camera.id,
          camera.uuid,
          camera.short_name,
          camera.description,
          camera.retain_bytes;
        )");
    while (list_cameras_run.Step() == SQLITE_ROW) {
      CameraData data;
      data.id = list_cameras_run.ColumnInt64(0);
      Uuid uuid;
      if (!uuid.ParseBinary(list_cameras_run.ColumnBlob(1))) {
        *error_message =
            StrCat("bad uuid ", ToHex(list_cameras_run.ColumnBlob(1)),
                   " for camera id ", data.id);
        return false;
      }
      data.short_name = list_cameras_run.ColumnText(2).as_string();
      data.description = list_cameras_run.ColumnText(3).as_string();
      data.host = list_cameras_run.ColumnText(4).as_string();
      data.username = list_cameras_run.ColumnText(5).as_string();
      data.password = list_cameras_run.ColumnText(6).as_string();
      data.main_rtsp_path = list_cameras_run.ColumnText(7).as_string();
      data.sub_rtsp_path = list_cameras_run.ColumnText(8).as_string();
      data.retain_bytes = list_cameras_run.ColumnInt64(9);
      data.min_start_time_90k = list_cameras_run.ColumnType(10) == SQLITE_NULL
                                    ? -1
                                    : list_cameras_run.ColumnInt64(10);
      data.max_end_time_90k = list_cameras_run.ColumnType(11) == SQLITE_NULL
                                  ? -1
                                  : list_cameras_run.ColumnInt64(11);
      data.total_duration_90k = list_cameras_run.ColumnInt64(12);
      data.total_sample_file_bytes = list_cameras_run.ColumnInt64(13);

      auto ret = cameras_by_uuid_.insert(std::make_pair(uuid, data));
      if (!ret.second) {
        *error_message = StrCat("Duplicate camera uuid ", uuid.UnparseText());
        return false;
      }
      CameraData *data_p = &ret.first->second;
      if (!cameras_by_id_.insert(std::make_pair(data.id, data_p)).second) {
        *error_message = StrCat("Duplicate camera id ", data.id);
        return false;
      }
    }
    if (list_cameras_run.status() != SQLITE_DONE) {
      *error_message = StrCat("Camera list query failed: ",
                              list_cameras_run.error_message());
    }

    // It's simplest to just keep the video sample entries in RAM.
    auto video_sample_entries_run = ctx.UseOnce(
        R"(
        select
          id,
          sha1,
          width,
          height,
          data
        from
          video_sample_entry
        )");
    while (video_sample_entries_run.Step() == SQLITE_ROW) {
      VideoSampleEntry entry;
      entry.id = video_sample_entries_run.ColumnInt64(0);
      entry.sha1 = video_sample_entries_run.ColumnBlob(1).as_string();
      int64_t width_tmp = video_sample_entries_run.ColumnInt64(2);
      int64_t height_tmp = video_sample_entries_run.ColumnInt64(3);
      auto max = std::numeric_limits<uint16_t>::max();
      if (width_tmp <= 0 || width_tmp > max || height_tmp <= 0 ||
          height_tmp > max) {
        *error_message =
            StrCat("video_sample_entry id ", entry.id, " width ", width_tmp,
                   " / height ", height_tmp, " out of range.");
        return false;
      }
      entry.width = width_tmp;
      entry.height = height_tmp;
      entry.data = video_sample_entries_run.ColumnBlob(4).as_string();
      CHECK(
          video_sample_entries_.insert(std::make_pair(entry.id, entry)).second)
          << "duplicate: " << entry.id;
    }
  }

  std::string list_camera_recordings_sql = StrCat(
      R"(
      select
        recording.start_time_90k,
        recording.duration_90k,
        recording.video_samples,
        recording.sample_file_bytes,
        recording.video_sample_entry_id
      from
        recording
      where
        camera_id = :camera_id and
        recording.start_time_90k > :start_time_90k - )",
      kMaxRecordingDuration, " and\n",
      R"(
        recording.start_time_90k < :end_time_90k and
        recording.start_time_90k + recording.duration_90k > :start_time_90k
      order by
        recording.start_time_90k desc;)");
  list_camera_recordings_stmt_ =
      db_->Prepare(list_camera_recordings_sql, nullptr, error_message);
  if (!list_camera_recordings_stmt_.valid()) {
    return false;
  }

  std::string build_mp4_sql = StrCat(
      R"(
      select
        recording.id,
        recording.start_time_90k,
        recording.duration_90k,
        recording.sample_file_bytes,
        recording.sample_file_uuid,
        recording.sample_file_sha1,
        recording.video_index,
        recording.video_samples,
        recording.video_sync_samples,
        recording.video_sample_entry_id
      from
        recording
      where
        camera_id = :camera_id and
        recording.start_time_90k > :start_time_90k - )",
      kMaxRecordingDuration, " and\n",
      R"(
        recording.start_time_90k < :end_time_90k and
        recording.start_time_90k + recording.duration_90k > :start_time_90k
      order by
        recording.start_time_90k;)");
  build_mp4_stmt_ = db_->Prepare(build_mp4_sql, nullptr, error_message);
  if (!build_mp4_stmt_.valid()) {
    return false;
  }

  insert_reservation_stmt_ = db_->Prepare(
      "insert into reserved_sample_files (uuid,  state)\n"
      "                           values (:uuid, :state);",
      nullptr, error_message);
  if (!insert_reservation_stmt_.valid()) {
    return false;
  }

  delete_reservation_stmt_ =
      db_->Prepare("delete from reserved_sample_files where uuid = :uuid;",
                   nullptr, error_message);
  if (!delete_reservation_stmt_.valid()) {
    return false;
  }

  insert_video_sample_entry_stmt_ = db_->Prepare(
      R"(
      insert into video_sample_entry (sha1,  width,  height,  data)
                              values (:sha1, :width, :height, :data);
      )",
      nullptr, error_message);
  if (!insert_video_sample_entry_stmt_.valid()) {
    return false;
  }

  insert_recording_stmt_ = db_->Prepare(
      R"(
      insert into recording (camera_id, sample_file_bytes, start_time_90k,
                             duration_90k, local_time_delta_90k, video_samples,
                             video_sync_samples, video_sample_entry_id,
                             sample_file_uuid, sample_file_sha1, video_index)
                     values (:camera_id, :sample_file_bytes, :start_time_90k,
                             :duration_90k, :local_time_delta_90k,
                             :video_samples, :video_sync_samples,
                             :video_sample_entry_id, :sample_file_uuid,
                             :sample_file_sha1, :video_index);
      )",
      nullptr, error_message);
  if (!insert_recording_stmt_.valid()) {
    return false;
  }

  list_oldest_sample_files_stmt_ = db_->Prepare(
      R"(
      select
        id,
        sample_file_uuid,
        duration_90k,
        sample_file_bytes
      from
        recording
      where
        camera_id = :camera_id
      order by
        start_time_90k
      )",
      nullptr, error_message);
  if (!list_oldest_sample_files_stmt_.valid()) {
    return false;
  }

  delete_recording_stmt_ =
      db_->Prepare("delete from recording where id = :recording_id;", nullptr,
                   error_message);
  if (!delete_recording_stmt_.valid()) {
    return false;
  }

  camera_min_start_stmt_ = db_->Prepare(
      R"(
      select
        start_time_90k
      from
        recording
      where
        camera_id = :camera_id
      order by start_time_90k limit 1;
      )",
      nullptr, error_message);
  if (!camera_min_start_stmt_.valid()) {
    return false;
  }

  camera_max_start_stmt_ = db_->Prepare(
      R"(
      select
        start_time_90k,
        duration_90k
      from
        recording
      where
        camera_id = :camera_id
      order by start_time_90k desc;
      )",
      nullptr, error_message);
  if (!camera_max_start_stmt_.valid()) {
    return false;
  }

  return true;
}

void MoonfireDatabase::ListCameras(
    std::function<IterationControl(const ListCamerasRow &)> cb) {
  DatabaseContext ctx(db_);
  ListCamerasRow row;
  for (const auto &entry : cameras_by_uuid_) {
    row.id = entry.second.id;
    row.uuid = entry.first;
    row.short_name = entry.second.short_name;
    row.description = entry.second.description;
    row.host = entry.second.host;
    row.username = entry.second.username;
    row.password = entry.second.password;
    row.main_rtsp_path = entry.second.main_rtsp_path;
    row.sub_rtsp_path = entry.second.sub_rtsp_path;
    row.retain_bytes = entry.second.retain_bytes;
    row.min_start_time_90k = entry.second.min_start_time_90k;
    row.max_end_time_90k = entry.second.max_end_time_90k;
    row.total_duration_90k = entry.second.total_duration_90k;
    row.total_sample_file_bytes = entry.second.total_sample_file_bytes;
    if (cb(row) == IterationControl::kBreak) {
      return;
    }
  }
  return;
}

bool MoonfireDatabase::GetCamera(Uuid camera_uuid, GetCameraRow *row) {
  DatabaseContext ctx(db_);
  const auto it = cameras_by_uuid_.find(camera_uuid);
  if (it == cameras_by_uuid_.end()) {
    return false;
  }
  const CameraData &data = it->second;
  row->short_name = data.short_name;
  row->description = data.description;
  row->retain_bytes = data.retain_bytes;
  row->min_start_time_90k = data.min_start_time_90k;
  row->max_end_time_90k = data.max_end_time_90k;
  row->total_duration_90k = data.total_duration_90k;
  row->total_sample_file_bytes = data.total_sample_file_bytes;
  return true;
}

bool MoonfireDatabase::ListCameraRecordings(
    Uuid camera_uuid, int64_t start_time_90k, int64_t end_time_90k,
    std::function<IterationControl(const ListCameraRecordingsRow &)> cb,
    std::string *error_message) {
  DatabaseContext ctx(db_);
  const auto camera_it = cameras_by_uuid_.find(camera_uuid);
  if (camera_it == cameras_by_uuid_.end()) {
    *error_message = StrCat("no such camera ", camera_uuid.UnparseText());
    return false;
  }
  auto run = ctx.Borrow(&list_camera_recordings_stmt_);
  run.BindInt64(":camera_id", camera_it->second.id);
  run.BindInt64(":start_time_90k", start_time_90k);
  run.BindInt64(":end_time_90k", end_time_90k);
  ListCameraRecordingsRow row;
  while (run.Step() == SQLITE_ROW) {
    row.start_time_90k = run.ColumnInt64(0);
    row.end_time_90k = row.start_time_90k + run.ColumnInt64(1);
    row.video_samples = run.ColumnInt64(2);
    row.sample_file_bytes = run.ColumnInt64(3);
    int64_t video_sample_entry_id = run.ColumnInt64(4);
    const auto it = video_sample_entries_.find(video_sample_entry_id);
    if (it == video_sample_entries_.end()) {
      *error_message =
          StrCat("recording references invalid video sample entry ",
                 video_sample_entry_id);
      return false;
    }
    const VideoSampleEntry &entry = it->second;
    row.video_sample_entry_sha1 = entry.sha1;
    row.width = entry.width;
    row.height = entry.height;
    if (cb(row) == IterationControl::kBreak) {
      return true;
    }
  }
  if (run.status() != SQLITE_DONE) {
    *error_message = StrCat("sqlite query failed: ", run.error_message());
    return false;
  }
  return true;
}

bool MoonfireDatabase::ListMp4Recordings(
    Uuid camera_uuid, int64_t start_time_90k, int64_t end_time_90k,
    std::function<IterationControl(Recording &, const VideoSampleEntry &)>
        row_cb,
    std::string *error_message) {
  VLOG(1) << "...(1/4): Waiting for database lock";
  DatabaseContext ctx(db_);
  const auto it = cameras_by_uuid_.find(camera_uuid);
  if (it == cameras_by_uuid_.end()) {
    *error_message = StrCat("no such camera ", camera_uuid.UnparseText());
    return false;
  }
  const CameraData &data = it->second;
  VLOG(1) << "...(2/4): Querying database";
  auto run = ctx.Borrow(&build_mp4_stmt_);
  run.BindInt64(":camera_id", data.id);
  run.BindInt64(":end_time_90k", end_time_90k);
  run.BindInt64(":start_time_90k", start_time_90k);
  Recording recording;
  VideoSampleEntry sample_entry;
  while (run.Step() == SQLITE_ROW) {
    recording.id = run.ColumnInt64(0);
    recording.camera_id = data.id;
    recording.start_time_90k = run.ColumnInt64(1);
    recording.end_time_90k = recording.start_time_90k + run.ColumnInt64(2);
    recording.sample_file_bytes = run.ColumnInt64(3);
    if (!recording.sample_file_uuid.ParseBinary(run.ColumnBlob(4))) {
      *error_message =
          StrCat("recording ", recording.id, " has unparseable uuid ",
                 ToHex(run.ColumnBlob(4)));
      return false;
    }
    recording.sample_file_sha1 = run.ColumnBlob(5).as_string();
    recording.video_index = run.ColumnBlob(6).as_string();
    recording.video_samples = run.ColumnInt64(7);
    recording.video_sync_samples = run.ColumnInt64(8);
    recording.video_sample_entry_id = run.ColumnInt64(9);

    auto it = video_sample_entries_.find(recording.video_sample_entry_id);
    if (it == video_sample_entries_.end()) {
      *error_message = StrCat("recording ", recording.id,
                              " references unknown video sample entry ",
                              recording.video_sample_entry_id);
      return false;
    }
    const VideoSampleEntry &entry = it->second;

    if (row_cb(recording, entry) == IterationControl::kBreak) {
      return true;
    }
  }
  if (run.status() != SQLITE_DONE && run.status() != SQLITE_ROW) {
    *error_message = StrCat("sqlite query failed: ", run.error_message());
    return false;
  }
  return true;
}

bool MoonfireDatabase::ListReservedSampleFiles(std::vector<Uuid> *reserved,
                                               std::string *error_message) {
  reserved->clear();
  DatabaseContext ctx(db_);
  auto run = ctx.UseOnce("select uuid from reserved_sample_files;");
  while (run.Step() == SQLITE_ROW) {
    Uuid uuid;
    if (!uuid.ParseBinary(run.ColumnBlob(0))) {
      *error_message = StrCat("unparseable uuid ", ToHex(run.ColumnBlob(0)));
      return false;
    }
    reserved->push_back(uuid);
  }
  if (run.status() != SQLITE_DONE) {
    *error_message = run.error_message();
    return false;
  }
  return true;
}

std::vector<Uuid> MoonfireDatabase::ReserveSampleFiles(
    int n, std::string *error_message) {
  if (n == 0) {
    return std::vector<Uuid>();
  }
  std::vector<Uuid> uuids;
  uuids.reserve(n);
  for (int i = 0; i < n; ++i) {
    uuids.push_back(uuidgen_->Generate());
  }
  DatabaseContext ctx(db_);
  if (!ctx.BeginTransaction(error_message)) {
    return std::vector<Uuid>();
  }
  for (const auto &uuid : uuids) {
    auto run = ctx.Borrow(&insert_reservation_stmt_);
    run.BindBlob(":uuid", uuid.binary_view());
    run.BindInt64(":state", static_cast<int64_t>(ReservationState::kWriting));
    if (run.Step() != SQLITE_DONE) {
      ctx.RollbackTransaction();
      *error_message = run.error_message();
      return std::vector<Uuid>();
    }
  }
  if (!ctx.CommitTransaction(error_message)) {
    return std::vector<Uuid>();
  }
  return uuids;
}

bool MoonfireDatabase::InsertVideoSampleEntry(VideoSampleEntry *entry,
                                              std::string *error_message) {
  if (entry->id != -1) {
    *error_message = StrCat("video_sample_entry already has id ", entry->id);
    return false;
  }
  DatabaseContext ctx(db_);
  for (const auto &some_entry : video_sample_entries_) {
    if (some_entry.second.sha1 == entry->sha1) {
      if (entry->width != some_entry.second.width ||
          entry->height != some_entry.second.height) {
        *error_message =
            StrCat("inconsistent entry for sha1 ", ToHex(entry->sha1),
                   ": existing entry has ", some_entry.second.width, "x",
                   some_entry.second.height, ", new entry has ", entry->width,
                   "x", entry->height);
        return false;
      }
      entry->id = some_entry.first;
      return true;
    }
  }
  auto insert_run = ctx.Borrow(&insert_video_sample_entry_stmt_);
  insert_run.BindBlob(":sha1", entry->sha1);
  insert_run.BindInt64(":width", entry->width);
  insert_run.BindInt64(":height", entry->height);
  insert_run.BindBlob(":data", entry->data);
  if (insert_run.Step() != SQLITE_DONE) {
    *error_message =
        StrCat("insert video sample entry: ", insert_run.error_message(),
               ": sha1=", ToHex(entry->sha1), ", dimensions=", entry->width,
               "x", entry->height, ", data=", ToHex(entry->data));
    return false;
  }
  entry->id = ctx.last_insert_rowid();
  CHECK(video_sample_entries_.insert(std::make_pair(entry->id, *entry)).second)
      << "duplicate: " << entry->id;
  return true;
}

bool MoonfireDatabase::InsertRecording(Recording *recording,
                                       std::string *error_message) {
  if (recording->id != -1) {
    *error_message = StrCat("recording already has id ", recording->id);
    return false;
  }
  if (recording->end_time_90k < recording->start_time_90k) {
    *error_message =
        StrCat("end time ", recording->end_time_90k, " must be >= start time ",
               recording->start_time_90k);
    return false;
  }
  DatabaseContext ctx(db_);
  auto it = cameras_by_id_.find(recording->camera_id);
  if (it == cameras_by_id_.end()) {
    *error_message = StrCat("no camera with id ", recording->camera_id);
    return false;
  }
  CameraData *camera_data = it->second;
  if (!ctx.BeginTransaction(error_message)) {
    return false;
  }
  auto delete_run = ctx.Borrow(&delete_reservation_stmt_);
  delete_run.BindBlob(":uuid", recording->sample_file_uuid.binary_view());
  if (delete_run.Step() != SQLITE_DONE) {
    *error_message = delete_run.error_message();
    ctx.RollbackTransaction();
    return false;
  }
  if (ctx.changes() != 1) {
    *error_message = StrCat("uuid ", recording->sample_file_uuid.UnparseText(),
                            " is not reserved");
    ctx.RollbackTransaction();
    return false;
  }
  auto insert_run = ctx.Borrow(&insert_recording_stmt_);
  insert_run.BindInt64(":camera_id", recording->camera_id);
  insert_run.BindInt64(":sample_file_bytes", recording->sample_file_bytes);
  insert_run.BindInt64(":start_time_90k", recording->start_time_90k);
  insert_run.BindInt64(":duration_90k",
                       recording->end_time_90k - recording->start_time_90k);
  insert_run.BindInt64(":local_time_delta_90k",
                       recording->local_time_90k - recording->start_time_90k);
  insert_run.BindInt64(":video_samples", recording->video_samples);
  insert_run.BindInt64(":video_sync_samples", recording->video_sync_samples);
  insert_run.BindInt64(":video_sample_entry_id",
                       recording->video_sample_entry_id);
  insert_run.BindBlob(":sample_file_uuid",
                      recording->sample_file_uuid.binary_view());
  insert_run.BindBlob(":sample_file_sha1", recording->sample_file_sha1);
  insert_run.BindBlob(":video_index", recording->video_index);
  if (insert_run.Step() != SQLITE_DONE) {
    *error_message =
        StrCat("insert failed: ", insert_run.error_message(), ", camera_id=",
               recording->camera_id, ", sample_file_bytes=",
               recording->sample_file_bytes, ", start_time_90k=",
               recording->start_time_90k, ", duration_90k=",
               recording->end_time_90k - recording->start_time_90k,
               ", local_time_delta_90k=",
               recording->local_time_90k - recording->start_time_90k,
               ", video_samples=", recording->video_samples,
               ", video_sync_samples=", recording->video_sync_samples,
               ", video_sample_entry_id=", recording->video_sample_entry_id,
               ", sample_file_uuid=", recording->sample_file_uuid.UnparseText(),
               ", sample_file_sha1=", ToHex(recording->sample_file_sha1),
               ", video_index length ", recording->video_index.size());
    ctx.RollbackTransaction();
    return false;
  }
  if (!ctx.CommitTransaction(error_message)) {
    LOG(ERROR) << "commit failed";
    return false;
  }
  recording->id = ctx.last_insert_rowid();
  if (camera_data->min_start_time_90k == -1 ||
      camera_data->min_start_time_90k > recording->start_time_90k) {
    camera_data->min_start_time_90k = recording->start_time_90k;
  }
  if (camera_data->max_end_time_90k == -1 ||
      camera_data->max_end_time_90k < recording->end_time_90k) {
    camera_data->max_end_time_90k = recording->end_time_90k;
  }
  camera_data->total_duration_90k +=
      recording->end_time_90k - recording->start_time_90k;
  camera_data->total_sample_file_bytes += recording->sample_file_bytes;
  return true;
}

bool MoonfireDatabase::ListOldestSampleFiles(
    Uuid camera_uuid,
    std::function<IterationControl(const ListOldestSampleFilesRow &)> row_cb,
    std::string *error_message) {
  DatabaseContext ctx(db_);
  auto it = cameras_by_uuid_.find(camera_uuid);
  if (it == cameras_by_uuid_.end()) {
    *error_message = StrCat("no such camera ", camera_uuid.UnparseText());
    return false;
  }
  const CameraData &camera_data = it->second;
  auto run = ctx.Borrow(&list_oldest_sample_files_stmt_);
  run.BindInt64(":camera_id", camera_data.id);
  ListOldestSampleFilesRow row;
  while (run.Step() == SQLITE_ROW) {
    row.camera_id = camera_data.id;
    row.recording_id = run.ColumnInt64(0);
    if (!row.sample_file_uuid.ParseBinary(run.ColumnBlob(1))) {
      *error_message =
          StrCat("recording ", row.recording_id, " has unparseable uuid ",
                 ToHex(run.ColumnBlob(1)));
      return false;
    }
    row.duration_90k = run.ColumnInt64(2);
    row.sample_file_bytes = run.ColumnInt64(3);
    if (row_cb(row) == IterationControl::kBreak) {
      return true;
    }
  }
  if (run.status() != SQLITE_DONE) {
    *error_message = run.error_message();
    return false;
  }
  return true;
}

bool MoonfireDatabase::DeleteRecordings(
    const std::vector<ListOldestSampleFilesRow> &recordings,
    std::string *error_message) {
  if (recordings.empty()) {
    return true;
  }

  DatabaseContext ctx(db_);
  if (!ctx.BeginTransaction(error_message)) {
    return false;
  }
  struct State {
    int64_t deleted_duration_90k = 0;
    int64_t deleted_sample_file_bytes = 0;
    int64_t min_start_time_90k = -1;
    int64_t max_end_time_90k = -1;
    CameraData *camera_data = nullptr;
  };
  std::map<int64_t, State> state_by_camera_id;
  for (const auto &recording : recordings) {
    State &state = state_by_camera_id[recording.camera_id];
    state.deleted_duration_90k += recording.duration_90k;
    state.deleted_sample_file_bytes += recording.sample_file_bytes;

    auto delete_run = ctx.Borrow(&delete_recording_stmt_);
    delete_run.BindInt64(":recording_id", recording.recording_id);
    if (delete_run.Step() != SQLITE_DONE) {
      ctx.RollbackTransaction();
      *error_message = StrCat("delete: ", delete_run.error_message());
      return false;
    }
    if (ctx.changes() != 1) {
      ctx.RollbackTransaction();
      *error_message = StrCat("no such recording ", recording.recording_id);
      return false;
    }

    auto insert_run = ctx.Borrow(&insert_reservation_stmt_);
    insert_run.BindBlob(":uuid", recording.sample_file_uuid.binary_view());
    insert_run.BindInt64(":state",
                         static_cast<int64_t>(ReservationState::kDeleting));
    if (insert_run.Step() != SQLITE_DONE) {
      ctx.RollbackTransaction();
      *error_message = StrCat("insert: ", insert_run.error_message());
      return false;
    }
  }

  // Recompute start and end times for each camera.
  for (auto &state_entry : state_by_camera_id) {
    int64_t camera_id = state_entry.first;
    State &state = state_entry.second;
    auto it = cameras_by_id_.find(camera_id);
    if (it == cameras_by_id_.end()) {
      *error_message =
          StrCat("internal error; can't find camera id ", camera_id);
      return false;
    }
    state.camera_data = it->second;

    // The minimum is straightforward, taking advantage of the start_time_90k
    // index for speed.
    auto min_run = ctx.Borrow(&camera_min_start_stmt_);
    min_run.BindInt64(":camera_id", camera_id);
    if (min_run.Step() == SQLITE_ROW) {
      state.min_start_time_90k = min_run.ColumnInt64(0);
    } else if (min_run.Step() == SQLITE_DONE) {
      // There are no recordings left.
      state.min_start_time_90k = -1;
      state.max_end_time_90k = -1;
      continue;  // skip additional query below to calculate max.
    } else {
      ctx.RollbackTransaction();
      *error_message = StrCat("min: ", min_run.error_message());
      return false;
    }

    // The maximum is less straightforward in the case of overlap - all
    // recordings starting in the last kMaxRecordingDuration must be examined
    // to take advantage of the start_time_90k index.
    auto max_run = ctx.Borrow(&camera_max_start_stmt_);
    max_run.BindInt64(":camera_id", camera_id);
    if (max_run.Step() != SQLITE_ROW) {
      // If there was a min row, there should be a max row too, so this is an
      // error even in the SQLITE_DONE case.
      ctx.RollbackTransaction();
      *error_message = StrCat("max[0]: ", max_run.error_message());
      return false;
    }
    int64_t max_start_90k = max_run.ColumnInt64(0);
    do {
      auto end_time_90k = max_run.ColumnInt64(0) + max_run.ColumnInt64(1);
      state.max_end_time_90k = std::max(state.max_end_time_90k, end_time_90k);
    } while (max_run.Step() == SQLITE_ROW &&
             max_run.ColumnInt64(0) > max_start_90k - kMaxRecordingDuration);
    if (max_run.status() != SQLITE_DONE && max_run.status() != SQLITE_ROW) {
      *error_message = StrCat("max[1]: ", max_run.error_message());
      ctx.RollbackTransaction();
      return false;
    }
  }

  if (!ctx.CommitTransaction(error_message)) {
    *error_message = StrCat("commit: ", *error_message);
    return false;
  }

  for (auto &state_entry : state_by_camera_id) {
    State &state = state_entry.second;
    state.camera_data->total_duration_90k -= state.deleted_duration_90k;
    state.camera_data->total_sample_file_bytes -=
        state.deleted_sample_file_bytes;
    state.camera_data->min_start_time_90k = state.min_start_time_90k;
    state.camera_data->max_end_time_90k = state.max_end_time_90k;
  }
  return true;
}

bool MoonfireDatabase::MarkSampleFilesDeleted(const std::vector<Uuid> &uuids,
                                              std::string *error_message) {
  if (uuids.empty()) {
    return true;
  }
  DatabaseContext ctx(db_);
  if (!ctx.BeginTransaction(error_message)) {
    return false;
  }
  for (const auto &uuid : uuids) {
    auto run = ctx.Borrow(&delete_reservation_stmt_);
    run.BindBlob(":uuid", uuid.binary_view());
    if (run.Step() != SQLITE_DONE) {
      *error_message = run.error_message();
      ctx.RollbackTransaction();
      return false;
    }
    if (ctx.changes() != 1) {
      *error_message = StrCat("no reservation for uuid ", uuid.UnparseText());
      ctx.RollbackTransaction();
      return false;
    }
  }
  if (!ctx.CommitTransaction(error_message)) {
    return false;
  }
  return true;
}

}  // namespace moonfire_nvr
