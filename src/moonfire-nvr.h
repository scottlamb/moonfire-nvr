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
// moonfire-nvr.h: main digital video recorder components.

#ifndef MOONFIRE_NVR_NVR_H
#define MOONFIRE_NVR_NVR_H

#include <sys/stat.h>
#include <time.h>

#include <atomic>
#include <map>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <event2/http.h>

#include "filesystem.h"
#include "moonfire-db.h"
#include "ffmpeg.h"
#include "time.h"

namespace moonfire_nvr {

// A signal that all streams associated with an Nvr should shut down.
class ShutdownSignal {
 public:
  ShutdownSignal() {}
  ShutdownSignal(const ShutdownSignal &) = delete;
  ShutdownSignal &operator=(const ShutdownSignal &) = delete;

  void Shutdown() { shutdown_.store(true, std::memory_order_relaxed); }

  bool ShouldShutdown() const {
    return shutdown_.load(std::memory_order_relaxed);
  }

 private:
  std::atomic_bool shutdown_{false};
};

// The Nvr's environment. This is supplied for testability.
struct Environment {
  WallClock *clock = nullptr;
  VideoSource *video_source = nullptr;
  File *sample_file_dir = nullptr;
  MoonfireDatabase *mdb = nullptr;
};

// A single video stream, currently always a camera's "main" (as opposed to
// "sub") stream. Methods are thread-compatible rather than thread-safe; the
// Nvr should call Run in a dedicated thread.
class Stream {
 public:
  Stream(const ShutdownSignal *signal, Environment *const env,
         const moonfire_nvr::ListCamerasRow &row, int rotate_offset_sec,
         int rotate_interval_sec)
      : signal_(signal),
        env_(env),
        row_(row),
        rotate_offset_sec_(rotate_offset_sec),
        rotate_interval_sec_(rotate_interval_sec),
        writer_(env->sample_file_dir) {}
  Stream(const Stream &) = delete;
  Stream &operator=(const Stream &) = delete;

  // Call from dedicated thread. Runs until shutdown requested.
  void Run();

 private:
  enum ProcessPacketsResult { kInputError, kOutputError, kStopped };

  ProcessPacketsResult ProcessPackets(std::string *error_message);
  bool OpenInput(std::string *error_message);

  // |pts| should be the relative pts within this output segment if closing
  // due to normal rotation, or -1 if closing abruptly.
  void CloseOutput(int64_t pts);

  bool OpenOutput(std::string *error_message);
  bool RotateFiles(std::string *error_message);
  void TryUnlink();

  const ShutdownSignal *signal_;
  const Environment *env_;
  ListCamerasRow row_;
  const int rotate_offset_sec_;
  const int rotate_interval_sec_;

  //
  // State below is used only by the thread in Run().
  //

  std::unique_ptr<moonfire_nvr::InputVideoPacketStream> in_;
  int64_t min_next_pts_ = std::numeric_limits<int64_t>::min();
  bool seen_key_frame_ = false;

  // need_transform_ indicates if TransformSampleData will need to be called
  // on each video sample.
  bool need_transform_ = false;

  VideoSampleEntry entry_;
  std::string transform_tmp_;
  std::vector<Uuid> uuids_to_unlink_;
  std::vector<Uuid> uuids_to_mark_deleted_;

  // Current output segment.
  Recording recording_;
  moonfire_nvr::SampleFileWriter writer_;
  SampleIndexEncoder index_;
  time_t rotate_time_ = 0;  // rotate when frame_realtime_ >= rotate_time_.

  // start_pts_ is the pts of the first frame included in the current output.
  int64_t start_pts_ = -1;

  // start_localtime_90k_ is the local system's time since epoch (in 90k units)
  // to match start_pts_.
  int64_t start_localtime_90k_ = -1;

  // These fields describe a packet which has been written to the
  // sample file but (because the duration is not yet known) has not been
  // added to the index.
  int32_t prev_pkt_start_time_90k_ = -1;
  int32_t prev_pkt_bytes_ = -1;
  bool prev_pkt_key_ = false;
  struct timespec frame_realtime_ = {0, 0};
};

// The main network video recorder, which manages a collection of streams.
class Nvr {
 public:
  explicit Nvr(Environment *env) : env_(env) {}
  Nvr(const Nvr &) = delete;
  Nvr &operator=(const Nvr &) = delete;

  // Shut down, blocking for outstanding streams.
  // Caller only has to guarantee that HttpCallback is not being called / will
  // not be called again, likely by having already shut down the event loop.
  ~Nvr();

  // Initialize the NVR. Call before any other operation.
  // Verifies configuration and starts background threads to capture/rotate
  // streams.
  bool Init(std::string *error_msg);

 private:
  void HttpCallbackForTopLevel(evhttp_request *req);

  Environment *const env_;
  std::vector<std::unique_ptr<Stream>> streams_;
  std::vector<std::thread> stream_threads_;
  ShutdownSignal signal_;
};

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_NVR_H
