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
// moonfire-nvr.cc: implementation of moonfire-nvr.h.
//
// Caveats:
//
// Currently the recording thread blocks while a just-finished recording
// is synced to disk and written to the database, which can be 250+ ms.
// Likewise when recordings are being deleted. It would be better to hand
// off to a separate syncer thread, only blocking the recording when there
// would otherwise be insufficient disk space.
//
// This also commits to the SQLite database potentially several times per
// minute per camera:
//
// 1. (rarely) to get a new video sample entry id
// 2. to reserve a new uuid
// 3. to move uuids planned for deletion from "recording" to
//    "reserved_sample_Files"
// 4. to mark those uuids as deleted
// 5. to insert the new recording
//
// These could be combined into a single batch per minute per camera or even
// per minute by doing some operations sooner (such as reserving the next
// minute's uuid when inserting the previous minute's recording) and some
// later (such as marking uuids as deleted).

#define _BSD_SOURCE  // for timegm(3).

#include "moonfire-nvr.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <sys/time.h>
#include <sys/types.h>
#include <unistd.h>

#include <event2/http.h>
#include <gflags/gflags.h>
#include <glog/logging.h>
#include <re2/re2.h>

#include "filesystem.h"
#include "h264.h"
#include "http.h"
#include "recording.h"
#include "string.h"
#include "time.h"

using std::string;

namespace moonfire_nvr {

namespace {

const int kRotateIntervalSec = 60;

}  // namespace

// Call from dedicated thread. Runs until shutdown requested.
void Stream::Run() {
  std::string error_message;

  // Do an initial rotation so that if retain_bytes has been reduced, the
  // bulk deletion happens now, rather than while an input stream is open.
  if (!RotateFiles(&error_message)) {
    LOG(WARNING) << row_.short_name
                 << ": initial rotation failed: " << error_message;
  }

  while (!signal_->ShouldShutdown()) {
    if (in_ == nullptr && !OpenInput(&error_message)) {
      LOG(WARNING) << row_.short_name
                   << ": Failed to open input; sleeping before retrying: "
                   << error_message;
      env_->clock->Sleep({1, 0});
      continue;
    }

    LOG(INFO) << row_.short_name << ": Calling ProcessPackets.";
    ProcessPacketsResult res = ProcessPackets(&error_message);
    if (res == kInputError) {
      CloseOutput(-1);
      in_.reset();
      start_localtime_90k_ = -1;
      LOG(WARNING) << row_.short_name
                   << ": Input error; sleeping before retrying: "
                   << error_message;
      env_->clock->Sleep({1, 0});
      continue;
    } else if (res == kOutputError) {
      CloseOutput(-1);
      LOG(WARNING) << row_.short_name
                   << ": Output error; sleeping before retrying: "
                   << error_message;
      env_->clock->Sleep({1, 0});
      continue;
    }
  }
  CloseOutput(-1);
}

Stream::ProcessPacketsResult Stream::ProcessPackets(
    std::string *error_message) {
  moonfire_nvr::VideoPacket pkt;
  CHECK(in_ != nullptr);
  CHECK(!writer_.is_open());
  while (!signal_->ShouldShutdown()) {
    if (!in_->GetNext(&pkt, error_message)) {
      if (error_message->empty()) {
        *error_message = "unexpected end of stream";
      }
      return kInputError;
    }

    // With gcc 4.9 (Raspbian Jessie),
    // #define AV_NOPTS_VALUE INT64_C(0x8000000000000000)
    // produces an unsigned value. Argh. Work around.
    static const int64_t kAvNoptsValue = AV_NOPTS_VALUE;
    if (pkt.pkt()->pts == kAvNoptsValue || pkt.pkt()->dts == kAvNoptsValue) {
      *error_message = "Rejecting packet with missing pts/dts";
      return kInputError;
    }

    if (pkt.pkt()->pts != pkt.pkt()->dts) {
      *error_message =
          StrCat("Rejecting packet with pts=", pkt.pkt()->pts, " != dts=",
                 pkt.pkt()->dts, "; expecting only I or P frames.");
      return kInputError;
    }

    if (pkt.pkt()->pts < min_next_pts_) {
      *error_message = StrCat("Rejecting non-increasing pts=", pkt.pkt()->pts,
                              "; expected at least ", min_next_pts_);
      return kInputError;
    }
    min_next_pts_ = pkt.pkt()->pts + 1;

    frame_realtime_ = env_->clock->Now();

    if (writer_.is_open() && frame_realtime_.tv_sec >= rotate_time_ &&
        pkt.is_key()) {
      LOG(INFO) << row_.short_name << ": Reached rotation time; closing "
                << recording_.sample_file_uuid.UnparseText() << ".";
      CloseOutput(pkt.pkt()->pts - start_pts_);
    } else if (writer_.is_open()) {
      VLOG(3) << row_.short_name << ": Rotation time=" << rotate_time_
              << " vs current time=" << frame_realtime_.tv_sec;
    }

    // Discard the initial, non-key frames from the input.
    if (!seen_key_frame_ && !pkt.is_key()) {
      continue;
    } else if (!seen_key_frame_) {
      seen_key_frame_ = true;
    }

    if (!writer_.is_open()) {
      start_pts_ = pkt.pts();
      if (!OpenOutput(error_message)) {
        return kOutputError;
      }
      rotate_time_ = frame_realtime_.tv_sec -
                     (frame_realtime_.tv_sec % rotate_interval_sec_) +
                     rotate_offset_sec_;
      if (rotate_time_ <= frame_realtime_.tv_sec) {
        rotate_time_ += rotate_interval_sec_;
      }
    }

    auto start_time_90k = pkt.pkt()->pts - start_pts_;
    if (prev_pkt_start_time_90k_ != -1) {
      index_.AddSample(start_time_90k - prev_pkt_start_time_90k_,
                       prev_pkt_bytes_, prev_pkt_key_);
    }
    re2::StringPiece data = pkt.data();
    if (need_transform_) {
      if (!TransformSampleData(data, &transform_tmp_, error_message)) {
        return kInputError;
      }
      data = transform_tmp_;
    }
    if (!writer_.Write(data, error_message)) {
      return kOutputError;
    }
    prev_pkt_start_time_90k_ = start_time_90k;
    prev_pkt_bytes_ = data.size();
    prev_pkt_key_ = pkt.is_key();
  }
  return kStopped;
}

bool Stream::OpenInput(std::string *error_message) {
  CHECK(in_ == nullptr);
  string url = StrCat("rtsp://", row_.username, ":", row_.password, "@",
                      row_.host, row_.main_rtsp_path);
  string redacted_url = StrCat("rtsp://", row_.username, ":redacted@",
                               row_.host, row_.main_rtsp_path);
  LOG(INFO) << row_.short_name << ": Opening input: " << redacted_url;
  in_ = env_->video_source->OpenRtsp(url, error_message);
  min_next_pts_ = std::numeric_limits<int64_t>::min();
  seen_key_frame_ = false;
  if (in_ == nullptr) {
    return false;
  }

  // The time base should match the 90kHz frequency specified in RFC 3551
  // section 5.
  if (in_->stream()->time_base.num != 1 ||
      in_->stream()->time_base.den != kTimeUnitsPerSecond) {
    *error_message =
        StrCat("unexpected time base ", in_->stream()->time_base.num, "/",
               in_->stream()->time_base.den);
    return false;
  }

  // width and height must fix into 16-bit ints for MP4 encoding.
  int max_dimension = std::numeric_limits<uint16_t>::max();
  if (in_->stream()->codec->width > max_dimension ||
      in_->stream()->codec->height > max_dimension) {
    *error_message =
        StrCat("input dimensions ", in_->stream()->codec->width, "x",
               in_->stream()->codec->height, " are too large.");
    return false;
  }
  entry_.id = -1;
  entry_.width = in_->stream()->codec->width;
  entry_.height = in_->stream()->codec->height;
  re2::StringPiece extradata = in_->extradata();
  if (!ParseExtraData(extradata, entry_.width, entry_.height, &entry_.data,
                      &need_transform_, error_message)) {
    in_.reset();
    return false;
  }
  auto sha1 = Digest::SHA1();
  sha1->Update(entry_.data);
  entry_.sha1 = sha1->Finalize();
  if (!env_->mdb->InsertVideoSampleEntry(&entry_, error_message)) {
    in_.reset();
    return false;
  }
  return true;
}

void Stream::CloseOutput(int64_t pts) {
  if (!writer_.is_open()) {
    return;
  }
  std::string error_message;
  if (prev_pkt_start_time_90k_ != -1) {
    int64_t duration_90k = pts - prev_pkt_start_time_90k_;
    index_.AddSample(duration_90k > 0 ? duration_90k : 0, prev_pkt_bytes_,
                     prev_pkt_key_);
  }
  if (!writer_.Close(&recording_.sample_file_sha1, &error_message)) {
    LOG(ERROR) << row_.short_name << ": Closing output "
               << recording_.sample_file_uuid.UnparseText()
               << " failed with error: " << error_message;
    uuids_to_unlink_.push_back(recording_.sample_file_uuid);
    TryUnlink();
    return;
  }
  int ret = env_->sample_file_dir->Sync();
  if (ret != 0) {
    LOG(ERROR) << row_.short_name
               << ": Unable to sync sample file dir after writing "
               << recording_.sample_file_uuid.UnparseText() << ": "
               << strerror(ret);
    uuids_to_unlink_.push_back(recording_.sample_file_uuid);
    TryUnlink();
    return;
  }
  if (!env_->mdb->InsertRecording(&recording_, &error_message)) {
    LOG(ERROR) << row_.short_name << ": Unable to insert recording "
               << recording_.sample_file_uuid.UnparseText() << ": "
               << error_message;
    uuids_to_unlink_.push_back(recording_.sample_file_uuid);
    TryUnlink();
    return;
  }
  row_.total_sample_file_bytes += recording_.sample_file_bytes;
  VLOG(1) << row_.short_name << ": ...wrote "
          << recording_.sample_file_uuid.UnparseText() << "; usage now "
          << HumanizeWithBinaryPrefix(row_.total_sample_file_bytes, "B");
}

void Stream::TryUnlink() {
  std::vector<Uuid> still_not_unlinked;
  for (const auto &uuid : uuids_to_unlink_) {
    std::string text = uuid.UnparseText();
    int ret = env_->sample_file_dir->Unlink(text.c_str());
    if (ret == ENOENT) {
      LOG(WARNING) << row_.short_name << ": Sample file " << text
                   << " already deleted!";
    } else if (ret != 0) {
      LOG(WARNING) << row_.short_name << ": Unable to unlink " << text << ": "
                   << strerror(ret);
      still_not_unlinked.push_back(uuid);
      continue;
    }
    uuids_to_mark_deleted_.push_back(uuid);
  }
  uuids_to_unlink_ = std::move(still_not_unlinked);
}

bool Stream::OpenOutput(std::string *error_message) {
  int64_t frame_localtime_90k = To90k(frame_realtime_);
  if (start_localtime_90k_ == -1) {
    start_localtime_90k_ = frame_localtime_90k - start_pts_;
  }
  if (!RotateFiles(error_message)) {
    return false;
  }
  std::vector<Uuid> reserved = env_->mdb->ReserveSampleFiles(1, error_message);
  if (reserved.size() != 1) {
    return false;
  }
  CHECK(!writer_.is_open());
  string filename = reserved[0].UnparseText();
  recording_.id = -1;
  recording_.camera_id = row_.id;
  recording_.sample_file_uuid = reserved[0];
  recording_.video_sample_entry_id = entry_.id;
  recording_.local_time_90k = frame_localtime_90k;
  index_.Init(&recording_, start_localtime_90k_ + start_pts_);
  if (!writer_.Open(filename.c_str(), error_message)) {
    return false;
  }
  prev_pkt_start_time_90k_ = -1;
  prev_pkt_bytes_ = -1;
  prev_pkt_key_ = false;
  LOG(INFO) << row_.short_name << ": Opened output " << filename
            << ", using start_pts=" << start_pts_
            << ", input timebase=" << in_->stream()->time_base.num << "/"
            << in_->stream()->time_base.den;
  return true;
}

bool Stream::RotateFiles(std::string *error_message) {
  int64_t bytes_needed = row_.total_sample_file_bytes - row_.retain_bytes;
  int64_t bytes_to_delete = 0;
  if (bytes_needed <= 0) {
    VLOG(1) << row_.short_name << ": have remaining quota of "
            << HumanizeWithBinaryPrefix(-bytes_needed, "B");
    return true;
  }
  LOG(INFO) << row_.short_name << ": need to delete "
            << HumanizeWithBinaryPrefix(bytes_needed, "B");
  std::vector<ListOldestSampleFilesRow> to_delete;
  auto row_cb = [&](const ListOldestSampleFilesRow &row) {
    bytes_needed -= row.sample_file_bytes;
    bytes_to_delete += row.sample_file_bytes;
    to_delete.push_back(row);
    return bytes_needed < 0 ? IterationControl::kBreak
                            : IterationControl::kContinue;
  };
  if (!env_->mdb->ListOldestSampleFiles(row_.uuid, row_cb, error_message)) {
    return false;
  }
  if (bytes_needed > 0) {
    *error_message =
        StrCat("couldn't find enough files to delete; ",
               HumanizeWithBinaryPrefix(bytes_needed, "B"), " left.");
    return false;
  }
  if (!env_->mdb->DeleteRecordings(to_delete, error_message)) {
    return false;
  }
  for (const auto &to_delete_row : to_delete) {
    uuids_to_unlink_.push_back(to_delete_row.sample_file_uuid);
  }
  row_.total_sample_file_bytes -= bytes_to_delete;
  TryUnlink();
  if (!uuids_to_unlink_.empty()) {
    *error_message =
        StrCat("failed to unlink ", uuids_to_unlink_.size(), " files.");
    return false;
  }
  int ret = env_->sample_file_dir->Sync();
  if (ret != 0) {
    *error_message = StrCat("fsync sample directory: ", strerror(ret));
    return false;
  }
  if (!env_->mdb->MarkSampleFilesDeleted(uuids_to_mark_deleted_,
                                         error_message)) {
    *error_message = StrCat("unable to mark ", uuids_to_mark_deleted_.size(),
                            " sample files as deleted");
    return false;
  }
  uuids_to_mark_deleted_.clear();
  VLOG(1) << row_.short_name << ": ...deleted successfully; usage now "
          << HumanizeWithBinaryPrefix(row_.total_sample_file_bytes, "B");
  return true;
}

Nvr::~Nvr() {
  signal_.Shutdown();
  for (auto &thread : stream_threads_) {
    thread.join();
  }
  // TODO: cleanup reservations?
}

bool Nvr::Init(std::string *error_msg) {
  std::vector<Uuid> all_reserved;
  if (!env_->mdb->ListReservedSampleFiles(&all_reserved, error_msg)) {
    return false;
  }
  for (const auto &reserved : all_reserved) {
    int ret = env_->sample_file_dir->Unlink(reserved.UnparseText().c_str());
    if (ret != 0 && ret != ENOENT) {
      LOG(WARNING) << "Unable to remove reserved sample file: "
                   << reserved.UnparseText();
    }
  }

  std::vector<ListCamerasRow> cameras;
  env_->mdb->ListCameras([&](const ListCamerasRow &row) {
    cameras.push_back(row);
    return IterationControl::kContinue;
  });
  for (size_t i = 0; i < cameras.size(); ++i) {
    int rotate_offset_sec = kRotateIntervalSec * i / cameras.size();
    auto *stream = new Stream(&signal_, env_, cameras[i], rotate_offset_sec,
                              kRotateIntervalSec);
    streams_.emplace_back(stream);
    stream_threads_.emplace_back([stream]() { stream->Run(); });
  };
  return true;
}

}  // namespace moonfire_nvr
