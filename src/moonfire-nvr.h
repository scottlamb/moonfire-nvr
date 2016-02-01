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

#include "config.pb.h"
#include "filesystem.h"
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

// Environment for streams to use. This is supplied for testability.
struct Environment {
  WallClock *clock = nullptr;
  VideoSource *video_source = nullptr;
  Filesystem *fs = nullptr;
};

// Delete old ".mp4" files within a specified directory, keeping them within a
// byte limit. In particular, "old" means "lexographically smaller filename".
// Thread-safe.
//
// On startup, FileManager reads the directory and stats every matching file.
// Afterward, it assumes that (1) it is informed of every added file and (2)
// files are deleted only through calls to Rotate.
class FileManager {
 public:
  using FileCallback = std::function<void(const std::string &filename,
                                          const struct stat &statbuf)>;

  // |short_name| will be prepended to log messages.
  FileManager(const std::string &short_name, const std::string &path,
              uint64_t byte_limit, Environment *env);
  FileManager(const FileManager &) = delete;
  FileManager &operator=(const FileManager &) = delete;

  // Initialize the FileManager by examining existing directory contents.
  // Create the directory if necessary.
  bool Init(std::string *error_message);

  // Delete files to go back within the byte limit if necessary.
  bool Rotate(std::string *error_message);

  // Note that a file has been added. This may bring the FileManager over the
  // byte limit; no files will be deleted immediately.
  bool AddFile(const std::string &filename, std::string *error_message);

  // Call |fn| for each file, while holding the lock.
  void ForEachFile(FileCallback) const;

  // Look up a file.
  // If |filename| is known to the manager, returns true and fills |statbuf|.
  // Otherwise returns false.
  bool Lookup(const std::string &filename, struct stat *statbuf) const;

  int64_t total_bytes() const {
    std::lock_guard<std::mutex> lock(mu_);
    return total_bytes_;
  }

 private:
  const std::string short_name_;
  const std::string path_;
  const uint64_t byte_limit_;
  Environment *const env_;

  mutable std::mutex mu_;
  std::map<std::string, struct stat> files_;
  uint64_t total_bytes_ = 0;  // total bytes of all |files_|.
};

// A single video stream, currently always a camera's "main" (as opposed to
// "sub") stream. Methods are thread-compatible rather than thread-safe; the
// Nvr should call Init + Run in a dedicated thread.
class Stream {
 public:
  Stream(const ShutdownSignal *signal, const moonfire_nvr::Config &config,
         Environment *const env, const moonfire_nvr::Camera &camera)
      : signal_(signal),
        env_(env),
        camera_path_(config.base_path() + "/" + camera.short_name()),
        rotate_interval_(config.rotate_sec()),
        camera_(camera),
        manager_(camera_.short_name(), camera_path_, camera.retain_bytes(),
                 env) {}
  Stream(const Stream &) = delete;
  Stream &operator=(const Stream &) = delete;

  // Call once on startup, before Run().
  bool Init(std::string *error_message);

  const std::string &camera_name() const { return camera_.short_name(); }
  const std::string &camera_description() const {
    return camera_.description();
  }

  // Call from dedicated thread. Runs until shutdown requested.
  void Run();

  // Handle HTTP requests which have been pre-determined to be for the
  // directory view of this stream or a particular file, respectively.
  // Thread-safe.
  void HttpCallbackForDirectory(evhttp_request *req);
  void HttpCallbackForFile(evhttp_request *req, const std::string &filename);

  std::vector<std::string> GetFilesForTesting();

 private:
  enum ProcessPacketsResult { kInputError, kOutputError, kStopped };

  const std::string &short_name() const { return camera_.short_name(); }

  ProcessPacketsResult ProcessPackets(std::string *error_message);
  bool OpenInput(std::string *error_message);
  void CloseOutput();
  std::string MakeOutputFilename();
  bool OpenOutput(std::string *error_message);
  bool RotateFiles();
  bool Stat(const std::string &filename, struct stat *file,
            std::string *error_message);

  const ShutdownSignal *signal_;
  const Environment *env_;
  const std::string camera_path_;
  const int32_t rotate_interval_;
  const moonfire_nvr::Camera camera_;

  FileManager manager_;               // thread-safe.
  std::unique_ptr<File> camera_dir_;  // thread-safe.

  //
  // State below is used only by the thread in Run().
  //

  std::unique_ptr<moonfire_nvr::InputVideoPacketStream> in_;
  int64_t min_next_pts_ = std::numeric_limits<int64_t>::min();
  bool seen_key_frame_ = false;

  // Current output segment.
  moonfire_nvr::OutputVideoPacketStream out_;
  time_t rotate_time_ = 0;  // rotate when frame_realtime_ >= rotate_time_.
  std::string out_file_;    // current output filename.
  int64_t start_pts_ = -1;

  // Packet-to-packet state.
  struct timespec frame_realtime_ = {0, 0};
};

// The main network video recorder, which manages a collection of streams.
class Nvr {
 public:
  Nvr();
  Nvr(const Nvr &) = delete;
  Nvr &operator=(const Nvr &) = delete;

  // Shut down, blocking for outstanding streams.
  // Caller only has to guarantee that HttpCallback is not being called / will
  // not be called again, likely by having already shut down the event loop.
  ~Nvr();

  // Initialize the NVR. Call before any other operation.
  // Verifies configuration and starts background threads to capture/rotate
  // streams.
  bool Init(const moonfire_nvr::Config &config, std::string *error_msg);

  // Handle an HTTP request.
  void HttpCallback(evhttp_request *req);

 private:
  void HttpCallbackForTopLevel(evhttp_request *req);

  Environment env_;
  moonfire_nvr::Config config_;
  std::vector<std::unique_ptr<Stream>> streams_;
  std::vector<std::thread> stream_threads_;
  ShutdownSignal signal_;
};

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_NVR_H
