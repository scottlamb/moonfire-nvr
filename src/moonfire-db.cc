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

#include "moonfire-db.h"

#include <string>

#include <glog/logging.h>

#include "http.h"
#include "mp4.h"
#include "recording.h"

namespace moonfire_nvr {

bool MoonfireDatabase::Init(std::string *error_message) {
  list_cameras_query_ = db_->Prepare(
      R"(
      select
        camera.id,
        camera.uuid,
        camera.short_name,
        camera.description,
        camera.retain_bytes,
        min(recording.start_time_90k),
        max(recording.end_time_90k),
        sum(recording.end_time_90k - recording.start_time_90k),
        sum(recording.sample_file_bytes)
      from
        camera
        left join recording on
            (camera.id = recording.camera_id and
             recording.status = 1)
      group by
        camera.id,
        camera.uuid,
        camera.short_name,
        camera.description,
        camera.retain_bytes;
  )",
      nullptr, error_message);
  if (!list_cameras_query_.valid()) {
    return false;
  }

  get_camera_query_ = db_->Prepare(
      R"(
      select
        uuid,
        short_name,
        description,
        retain_bytes
      from
        camera
      where
        id = :camera_id;)",
      nullptr, error_message);
  if (!get_camera_query_.valid()) {
    return false;
  }

  list_camera_recordings_query_ = db_->Prepare(
      R"(
      select
        recording.start_time_90k,
        recording.end_time_90k,
        recording.video_samples,
        recording.sample_file_bytes,
        recording.video_sample_entry_sha1,
        video_sample_entry.width,
        video_sample_entry.height
      from
        recording
        join video_sample_entry on
            (recording.video_sample_entry_sha1 = video_sample_entry.sha1)
      where
        recording.status = 1 and
        camera_id = :camera_id
      order by
        recording.start_time_90k;)",
      nullptr, error_message);
  if (!list_camera_recordings_query_.valid()) {
    return false;
  }

  build_mp4_query_ = db_->Prepare(
      R"(
      select
        recording.rowid,
        recording.start_time_90k,
        recording.end_time_90k,
        recording.sample_file_bytes,
        recording.sample_file_uuid,
        recording.sample_file_sha1,
        recording.video_sample_entry_sha1,
        recording.video_index,
        recording.video_samples,
        recording.video_sync_samples,
        video_sample_entry.bytes,
        video_sample_entry.width,
        video_sample_entry.height
      from
        recording join video_sample_entry on
        (recording.video_sample_entry_sha1 = video_sample_entry.sha1)
      where
        recording.status = 1 and
        camera_id = :camera_id and
        recording.start_time_90k < :end_time_90k and
        recording.end_time_90k > :start_time_90k
      order by
        recording.start_time_90k;)",
      nullptr, error_message);
  if (!build_mp4_query_.valid()) {
    return false;
  }

  return true;
}

bool MoonfireDatabase::ListCameras(
    std::function<IterationControl(const ListCamerasRow &)> cb,
    std::string *error_message) {
  DatabaseContext ctx(db_);
  auto run = ctx.Borrow(&list_cameras_query_);
  ListCamerasRow row;
  while (run.Step() == SQLITE_ROW) {
    row.id = run.ColumnInt64(0);
    if (!row.uuid.ParseBinary(run.ColumnBlob(1))) {
      *error_message = StrCat("invalid uuid in row id ", row.id);
      return false;
    }
    row.short_name = run.ColumnText(2).as_string();
    row.description = run.ColumnText(3).as_string();
    row.retain_bytes = run.ColumnInt64(4);
    row.min_recording_start_time_90k = run.ColumnInt64(5);
    row.max_recording_end_time_90k = run.ColumnInt64(6);
    row.total_recording_duration_90k = run.ColumnInt64(7);
    row.total_sample_file_bytes = run.ColumnInt64(8);
    if (cb(row) == IterationControl::kBreak) {
      break;
    }
  }
  if (run.status() != SQLITE_DONE) {
    *error_message = StrCat("sqlite query failed: ", run.error_message());
    return false;
  }
  return true;
}

bool MoonfireDatabase::GetCamera(int64_t camera_id, GetCameraRow *row,
                                 std::string *error_message) {
  DatabaseContext ctx(db_);
  auto run = ctx.Borrow(&get_camera_query_);
  run.BindInt64(":camera_id", camera_id);
  if (run.Step() == SQLITE_ROW) {
    if (!row->uuid.ParseBinary(run.ColumnBlob(0))) {
      *error_message =
          StrCat("unable to parse uuid ", ToHex(run.ColumnBlob(0)));
      return false;
    }
    row->short_name = run.ColumnText(1).as_string();
    row->description = run.ColumnText(2).as_string();
    row->retain_bytes = run.ColumnInt64(3);
  } else if (run.status() == SQLITE_DONE) {
    *error_message = "no such camera";
    return false;
  }
  if (run.Step() == SQLITE_ROW) {
    *error_message = "multiple rows returned unexpectedly";
    return false;
  }
  return true;
}

bool MoonfireDatabase::ListCameraRecordings(
    int64_t camera_id,
    std::function<IterationControl(const ListCameraRecordingsRow &)> cb,
    std::string *error_message) {
  DatabaseContext ctx(db_);
  auto run = ctx.Borrow(&list_camera_recordings_query_);
  run.BindInt64(":camera_id", camera_id);
  ListCameraRecordingsRow row;
  while (run.Step() == SQLITE_ROW) {
    row.start_time_90k = run.ColumnInt64(0);
    row.end_time_90k = run.ColumnInt64(1);
    row.video_samples = run.ColumnInt64(2);
    row.sample_file_bytes = run.ColumnInt64(3);
    auto video_sample_entry_sha1 = run.ColumnBlob(4);
    row.video_sample_entry_sha1.assign(video_sample_entry_sha1.data(),
                                       video_sample_entry_sha1.size());
    row.width = run.ColumnInt64(5);
    row.height = run.ColumnInt64(6);
    if (cb(row) == IterationControl::kBreak) {
      break;
    }
  }
  if (run.status() != SQLITE_DONE) {
    *error_message = StrCat("sqlite query failed: ", run.error_message());
    return false;
  }
  return true;
}

std::shared_ptr<VirtualFile> MoonfireDatabase::BuildMp4(
    int64_t camera_id, int64_t start_time_90k, int64_t end_time_90k,
    std::string *error_message) {
  LOG(INFO) << "Building mp4 for camera: " << camera_id
            << ", start_time_90k: " << start_time_90k
            << ", end_time_90k: " << end_time_90k;

  Mp4FileBuilder builder;
  int64_t next_row_start_time_90k = start_time_90k;
  VideoSampleEntry sample_entry;
  int64_t rows = 0;
  {
    VLOG(1) << "...(1/4): Waiting for database lock";
    DatabaseContext ctx(db_);
    VLOG(1) << "...(2/4): Querying database";
    auto run = ctx.Borrow(&build_mp4_query_);
    run.BindInt64(":camera_id", camera_id);
    run.BindInt64(":end_time_90k", end_time_90k);
    run.BindInt64(":start_time_90k", start_time_90k);
    Recording recording;
    while (run.Step() == SQLITE_ROW) {
      recording.rowid = run.ColumnInt64(0);
      VLOG(2) << "row: " << recording.rowid;
      recording.start_time_90k = run.ColumnInt64(1);
      recording.end_time_90k = run.ColumnInt64(2);
      recording.sample_file_bytes = run.ColumnInt64(3);
      if (!recording.sample_file_uuid.ParseBinary(run.ColumnBlob(4))) {
        *error_message =
            StrCat("recording ", recording.rowid, " has unparseable uuid ",
                   ToHex(run.ColumnBlob(4)));
        return false;
      }
      recording.sample_file_path =
          StrCat("/home/slamb/new-moonfire/sample/",
                 recording.sample_file_uuid.UnparseText());
      recording.sample_file_sha1 = run.ColumnBlob(5).as_string();
      recording.video_sample_entry_sha1 = run.ColumnBlob(6).as_string();
      recording.video_index = run.ColumnBlob(7).as_string();
      recording.video_samples = run.ColumnInt64(8);
      recording.video_sync_samples = run.ColumnInt64(9);

      if (rows == 0 && recording.start_time_90k != next_row_start_time_90k) {
        *error_message =
            StrCat("recording starts late: ",
                   PrettyTimestamp(recording.start_time_90k), " (",
                   recording.start_time_90k, ") rather than requested: ",
                   PrettyTimestamp(start_time_90k), " (", start_time_90k, ")");
        return false;
      } else if (recording.start_time_90k != next_row_start_time_90k) {
        *error_message =
            StrCat("gap/overlap in recording: ",
                   PrettyTimestamp(next_row_start_time_90k), " (",
                   next_row_start_time_90k, ") to: ",
                   PrettyTimestamp(recording.start_time_90k), " (",
                   recording.start_time_90k, ") before row ", rows);
        return false;
      }

      next_row_start_time_90k = recording.end_time_90k;

      if (rows > 0 && recording.video_sample_entry_sha1 != sample_entry.sha1) {
        *error_message =
            StrCat("inconsistent video sample entries: this recording has ",
                   ToHex(recording.video_sample_entry_sha1), ", previous had ",
                   ToHex(sample_entry.sha1));
        return false;
      } else if (rows == 0) {
        sample_entry.sha1 = run.ColumnBlob(6).as_string();
        sample_entry.data = run.ColumnBlob(10).as_string();
        sample_entry.width = run.ColumnInt64(11);
        sample_entry.height = run.ColumnInt64(12);
        builder.SetSampleEntry(sample_entry);
      }

      // TODO: correct bounds within recording.
      // Currently this can return too much data.
      builder.Append(std::move(recording), 0,
                     std::numeric_limits<int32_t>::max());
      ++rows;
    }
    if (run.status() != SQLITE_DONE) {
      *error_message = StrCat("sqlite query failed: ", run.error_message());
      return false;
    }
  }
  if (rows == 0) {
    *error_message = StrCat("no recordings in range");
    return false;
  }
  if (next_row_start_time_90k != end_time_90k) {
    *error_message = StrCat("recording ends early: ",
                            PrettyTimestamp(next_row_start_time_90k), " (",
                            next_row_start_time_90k, "), not requested: ",
                            PrettyTimestamp(end_time_90k), " (", end_time_90k,
                            ") after ", rows, " rows");
    return false;
  }

  VLOG(1) << "...(3/4) building VirtualFile from " << rows << " recordings.";
  auto file = builder.Build(error_message);
  if (file == nullptr) {
    return false;
  }

  VLOG(1) << "...(4/4) success, " << file->size() << " bytes, etag "
          << file->etag();
  return file;
}

}  // namespace moonfire_nvr
