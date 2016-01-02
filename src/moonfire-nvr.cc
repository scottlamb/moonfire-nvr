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
// moonfire-nvr.cc: implementation of moonfire-nvr.h.

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
#include "http.h"
#include "string.h"
#include "time.h"

using std::string;

namespace moonfire_nvr {
namespace {

const char kFilenameSuffix[] = ".mp4";

}  // namespace
FileManager::FileManager(const std::string &short_name, const std::string &path,
                         uint64_t byte_limit)
    : short_name_(short_name), path_(path), byte_limit_(byte_limit) {}

bool FileManager::Init(std::string *error_message) {
  // Create the directory if it doesn't exist.
  // If the path exists, assume it is a valid directory.
  if (mkdir(path_.c_str(), 0700) < 0 && errno != EEXIST) {
    int err = errno;
    *error_message = StrCat("Unable to create ", path_, ": ", strerror(err));
    return false;
  }

  bool ok = true;

  auto file_fn = [this, &ok, error_message](const dirent *ent) {
    string filename(ent->d_name);
    if (ent->d_type != DT_REG) {
      VLOG(1) << short_name_ << ": Ignoring non-plain file " << filename;
      return IterationControl::kContinue;
    }
    if (!re2::StringPiece(filename).ends_with(kFilenameSuffix)) {
      VLOG(1) << short_name_ << ": Ignoring non-matching file " << filename
              << " of size " << ent->d_reclen;
      return IterationControl::kContinue;
    }

    if (!AddFile(filename, error_message)) {
      ok = false;
      return IterationControl::kBreak;  // no point in doing more.
    }
    return IterationControl::kContinue;
  };

  if (!DirForEach(path_, file_fn, error_message)) {
    return false;
  }

  return ok;
}

bool FileManager::Rotate(std::string *error_message) {
  mu_.lock();
  while (total_bytes_ > byte_limit_) {
    CHECK(!files_.empty()) << "total_bytes_=" << total_bytes_
                           << " vs retain=" << byte_limit_;
    auto it = files_.begin();
    const string filename = it->first;
    int64_t size = it->second.st_size;

    // Release the lock while doing (potentially slow) I/O.
    // Don't mark the file as deleted yet, so that a simultaneous Rotate() call
    // won't return prematurely.
    mu_.unlock();
    string fpath = StrCat(path_, "/", filename);
    if (unlink(fpath.c_str()) == 0) {
      LOG(INFO) << short_name_ << ": Deleted " << filename << " to reclaim "
                << size << " bytes.";
    } else if (errno == ENOENT) {
      // This may have happened due to a racing Rotate() call.
      // In any case, the file is gone, so proceed to mark it as such.
      LOG(INFO) << short_name_ << ": File " << filename
                << " was already deleted.";
    } else {
      int err = errno;
      *error_message =
          StrCat("unlink failed on ", filename, ": ", strerror(err));

      return false;
    }

    // Note that the file has been deleted.
    mu_.lock();
    if (!files_.empty()) {
      it = files_.begin();
      if (it->first == filename) {
        size = it->second.st_size;
        files_.erase(it);
        CHECK_GE(total_bytes_, size);
        total_bytes_ -= size;
      }
    }
  }
  int64_t total_bytes_copy = total_bytes_;
  mu_.unlock();
  LOG(INFO) << short_name_ << ": Path " << path_ << " total size is "
            << total_bytes_copy << ", within limit of " << byte_limit_;
  return true;
}

bool FileManager::AddFile(const std::string &filename,
                          std::string *error_message) {
  struct stat buf;
  string fpath = StrCat(path_, "/", filename);
  if (lstat(fpath.c_str(), &buf) != 0) {
    int err = errno;
    *error_message = StrCat("lstat on ", fpath, " failed: ", strerror(err));
    return false;
  }
  VLOG(1) << short_name_ << ": adding file " << filename << " size "
          << buf.st_size;
  std::lock_guard<std::mutex> lock(mu_);
  CHECK_GE(buf.st_size, 0) << fpath;
  uint64_t size = buf.st_size;
  if (!files_.emplace(filename, std::move(buf)).second) {
    *error_message = StrCat("filename ", filename, " already present.");
    return false;
  }
  total_bytes_ += size;
  return true;
}

void FileManager::ForEachFile(FileManager::FileCallback fn) const {
  std::lock_guard<std::mutex> lock(mu_);
  for (const auto &f : files_) {
    fn(f.first, f.second);
  }
}

bool FileManager::Lookup(const std::string &filename,
                         struct stat *statbuf) const {
  std::lock_guard<std::mutex> lock(mu_);
  const auto it = files_.find(filename);
  if (it != files_.end()) {
    *statbuf = it->second;
    return true;
  }
  return false;
}

bool Stream::Init(std::string *error_message) {
  // Validate configuration.
  if (!IsWord(camera_.short_name())) {
    *error_message = StrCat("Camera name ", camera_.short_name(), " invalid.");
    return false;
  }
  if (rotate_interval_ <= 0) {
    *error_message = StrCat("Rotate interval for ", camera_.short_name(),
                            " must be positive.");
    return false;
  }

  return manager_.Init(error_message);
}

// Call from dedicated thread. Runs until shutdown requested.
void Stream::Run() {
  std::string error_message;

  // Do an initial rotation so that if retain_bytes has been reduced, the
  // bulk deletion happens now, rather than while an input stream is open.
  if (!manager_.Rotate(&error_message)) {
    LOG(WARNING) << short_name()
                 << ": initial rotation failed: " << error_message;
  }

  while (!signal_->ShouldShutdown()) {
    if (in_ == nullptr && !OpenInput(&error_message)) {
      LOG(WARNING) << short_name()
                   << ": Failed to open input; sleeping before retrying: "
                   << error_message;
      env_->clock->Sleep({1, 0});
      continue;
    }

    LOG(INFO) << short_name() << ": Calling ProcessPackets.";
    ProcessPacketsResult res = ProcessPackets(&error_message);
    if (res == kInputError) {
      CloseOutput();
      in_.reset();
      LOG(WARNING) << short_name()
                   << ": Input error; sleeping before retrying: "
                   << error_message;
      env_->clock->Sleep({1, 0});
      continue;
    } else if (res == kOutputError) {
      CloseOutput();
      LOG(WARNING) << short_name()
                   << ": Output error; sleeping before retrying: "
                   << error_message;
      env_->clock->Sleep({1, 0});
      continue;
    }
  }
  CloseOutput();
}

Stream::ProcessPacketsResult Stream::ProcessPackets(
    std::string *error_message) {
  moonfire_nvr::VideoPacket pkt;
  CHECK(in_ != nullptr);
  CHECK(!out_.is_open());
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

    if (out_.is_open() && frame_realtime_.tv_sec >= rotate_time_ &&
        pkt.is_key()) {
      LOG(INFO) << short_name() << ": Reached rotation time; closing "
                << out_file_ << ".";
      VLOG(2) << short_name() << ": (Rotation time=" << rotate_time_
              << " vs current time=" << frame_realtime_.tv_sec << ")";
      out_.Close();

      if (!manager_.AddFile(out_file_, error_message)) {
        return kOutputError;
      }
    } else if (out_.is_open()) {
      VLOG(2) << short_name() << ": Rotation time=" << rotate_time_
              << " vs current time=" << frame_realtime_.tv_sec;
    }

    // Discard the initial, non-key frames from the input.
    if (!seen_key_frame_ && !pkt.is_key()) {
      continue;
    } else if (!seen_key_frame_) {
      seen_key_frame_ = true;
    }

    if (!out_.is_open()) {
      start_pts_ = pkt.pts();
      if (!OpenOutput(error_message)) {
        return kOutputError;
      }
      rotate_time_ = frame_realtime_.tv_sec -
                     (frame_realtime_.tv_sec % rotate_interval_) +
                     rotate_interval_;
    }

    // In the output stream, the pts and dts should start at 0.
    pkt.pkt()->pts -= start_pts_;
    pkt.pkt()->dts -= start_pts_;

    // The input's byte position and stream index aren't relevant to the
    // output.
    pkt.pkt()->pos = -1;
    pkt.pkt()->stream_index = 0;

    if (!out_.Write(&pkt, error_message)) {
      return kOutputError;
    }
  }
  return kStopped;
}

bool Stream::OpenInput(std::string *error_message) {
  CHECK(in_ == nullptr);
  string url = StrCat("rtsp://", camera_.user(), ":", camera_.password(), "@",
                      camera_.host(), camera_.main_rtsp_path());
  string redacted_url = StrCat("rtsp://", camera_.user(), ":redacted@",
                               camera_.host(), camera_.main_rtsp_path());
  LOG(INFO) << short_name() << ": Opening input: " << redacted_url;
  in_ = env_->video_source->OpenRtsp(url, error_message);
  min_next_pts_ = std::numeric_limits<int64_t>::min();
  seen_key_frame_ = false;
  return in_ != nullptr;
}

void Stream::CloseOutput() {
  out_.Close();
  // TODO: should know if the file was written or not.
  std::string error_message;
  if (!manager_.AddFile(out_file_, &error_message)) {
    VLOG(1) << short_name() << ": AddFile on recently closed output file "
            << out_file_ << "failed; the file may never have been written: "
            << error_message;
  }
}

std::string Stream::MakeOutputFilename() {
  const size_t kTimeBufLen = sizeof("YYYYmmDDHHMMSS");
  char formatted_time[kTimeBufLen];
  struct tm mytm;
  gmtime_r(&frame_realtime_.tv_sec, &mytm);
  strftime(formatted_time, kTimeBufLen, "%Y%m%d%H%M%S", &mytm);
  return StrCat(formatted_time, "_", camera_.short_name(), kFilenameSuffix);
}

bool Stream::OpenOutput(std::string *error_message) {
  if (!manager_.Rotate(error_message)) {
    return false;
  }
  CHECK(!out_.is_open());
  string filename = MakeOutputFilename();
  if (!out_.OpenFile(StrCat(camera_path_, "/", filename), *in_,
                     error_message)) {
    return false;
  }
  LOG(INFO) << short_name() << ": Opened output " << filename
            << ", using start_pts=" << start_pts_
            << ", input timebase=" << in_->stream()->time_base.num << "/"
            << in_->stream()->time_base.den
            << ", output timebase=" << out_.time_base().num << "/"
            << out_.time_base().den;
  out_file_ = std::move(filename);
  return true;
}

void Stream::HttpCallbackForDirectory(evhttp_request *req) {
  EvBuffer buf;
  buf.AddPrintf(
      "<!DOCTYPE html>\n"
      "<html>\n"
      "<head>\n"
      "<title>%s camera recordings</title>\n"
      "<style type=\"text/css\">\n"
      "th, td { text-align: left; padding-right: 3em; }\n"
      ".filename { font: 90%% monospace; }\n"
      "</style>\n"
      "</head>\n"
      "<body>\n"
      "<h1>%s camera recordings</h1>\n"
      "<p>%s</p>\n"
      "<table>\n"
      "<tr><th>Filename</th><th>Start</th><th>End</th></tr>\n",
      // short_name passed IsWord(); there's no need to escape it.
      camera_.short_name().c_str(), camera_.short_name().c_str(),
      EscapeHtml(camera_.description()).c_str());
  manager_.ForEachFile(
      [&buf](const std::string &filename, const struct stat &statbuf) {
        // Attempt to make a pretty version of the timestamp embedded in the
        // filename: with separators and in the local time zone. If this fails,
        // just leave it blank.
        string pretty_start_time;
        struct tm mytm;
        memset(&mytm, 0, sizeof(mytm));
        const size_t kTimeBufLen = 50;
        char tmbuf[kTimeBufLen];
        static const RE2 kFilenameRe(
            //    YYYY      mm        DD        HH        MM        SS
            "^([0-9]{4})([0-9]{2})([0-9]{2})([0-9]{2})([0-9]{2})([0-9]{2})_");
        if (RE2::PartialMatch(filename, kFilenameRe, &mytm.tm_year,
                              &mytm.tm_mon, &mytm.tm_mday, &mytm.tm_hour,
                              &mytm.tm_min, &mytm.tm_sec)) {
          mytm.tm_year -= 1900;
          mytm.tm_mon--;
          time_t start = timegm(&mytm);
          localtime_r(&start, &mytm);
          strftime(tmbuf, kTimeBufLen, "%a, %d %b %Y %H:%M:%S %Z", &mytm);
          pretty_start_time = tmbuf;
        }
        string pretty_end_time;
        localtime_r(&statbuf.st_mtime, &mytm);
        strftime(tmbuf, kTimeBufLen, "%a, %d %b %Y %H:%M:%S %Z", &mytm);
        pretty_end_time = tmbuf;

        buf.AddPrintf(
            "<tr><td class=\"filename\"><a href=\"%s\">%s</td>"
            "<td>%s</td><td>%s</td></tr>\n",
            filename.c_str(), filename.c_str(),
            EscapeHtml(pretty_start_time).c_str(),
            EscapeHtml(pretty_end_time).c_str());
      });
  buf.AddPrintf("</table>\n</html>\n");
  evhttp_send_reply(req, HTTP_OK, "OK", buf.get());
}

std::vector<std::string> Stream::GetFilesForTesting() {
  std::vector<std::string> files;
  manager_.ForEachFile(
      [&files](const std::string &filename, const struct stat &statbuf) {
        files.push_back(filename);
      });
  return files;
}

void Stream::HttpCallbackForFile(evhttp_request *req, const string &filename) {
  struct stat s;
  if (!manager_.Lookup(filename, &s)) {
    return evhttp_send_error(req, HTTP_NOTFOUND, "File not found.");
  }
  HttpServeFile(req, "video/mp4", StrCat(camera_path_, "/", filename), s);
}

Nvr::Nvr() {
  env_.clock = GetRealClock();
  env_.video_source = GetRealVideoSource();
}

Nvr::~Nvr() {
  signal_.Shutdown();
  for (auto &thread : stream_threads_) {
    thread.join();
  }
}

bool Nvr::Init(const moonfire_nvr::Config &config, std::string *error_msg) {
  if (config.base_path().empty()) {
    *error_msg = "base_path must be configured.";
    return false;
  }

  for (const auto &camera : config.camera()) {
    streams_.emplace_back(new Stream(&signal_, config, &env_, camera));
    if (!streams_.back()->Init(error_msg)) {
      return false;
    }
  }
  for (auto &stream : streams_) {
    stream_threads_.emplace_back([&stream]() { stream->Run(); });
  }
  return true;
}

void Nvr::HttpCallback(evhttp_request *req) {
  if (evhttp_request_get_command(req) != EVHTTP_REQ_GET) {
    return evhttp_send_error(req, HTTP_BADMETHOD, "only GET allowed");
  }

  evhttp_uri *uri = evhttp_uri_parse(evhttp_request_get_uri(req));
  if (uri == nullptr || evhttp_uri_get_path(uri) == nullptr) {
    return evhttp_send_error(req, HTTP_INTERNAL, "Failed to parse URI.");
  }

  std::string uri_path = evhttp_uri_get_path(uri);
  evhttp_uri_free(uri);
  uri = nullptr;

  if (uri_path == "/") {
    return HttpCallbackForTopLevel(req);
  } else if (!re2::StringPiece(uri_path).starts_with("/c/")) {
    return evhttp_send_error(req, HTTP_NOTFOUND, "Not found.");
  }
  size_t camera_name_start = strlen("/c/");
  size_t next_slash = uri_path.find('/', camera_name_start);
  if (next_slash == std::string::npos) {
    CHECK_EQ(0, evhttp_add_header(evhttp_request_get_output_headers(req),
                                  "Location", StrCat(uri_path, "/").c_str()));
    return evhttp_send_reply(req, HTTP_MOVEPERM, "OK", EvBuffer().get());
  }
  re2::StringPiece camera_name =
      uri_path.substr(camera_name_start, next_slash - camera_name_start);
  for (const auto &stream : streams_) {
    if (stream->camera_name() == camera_name) {
      if (uri_path.size() == next_slash + 1) {
        return stream->HttpCallbackForDirectory(req);
      } else {
        return stream->HttpCallbackForFile(req,
                                           uri_path.substr(next_slash + 1));
      }
    }
  }
  return evhttp_send_error(req, HTTP_NOTFOUND, "No such camera.");
}

void Nvr::HttpCallbackForTopLevel(evhttp_request *req) {
  EvBuffer buf;
  buf.Add("<ul>\n");
  for (const auto &stream : streams_) {
    // Camera name passed IsWord; there's no need to escape it.
    const string &name = stream->camera_name();
    string escaped_description = EscapeHtml(stream->camera_description());
    buf.AddPrintf("<li><a href=\"/c/%s/\">%s</a>: %s</li>\n", name.c_str(),
                  name.c_str(), escaped_description.c_str());
  }
  buf.Add("</ul>\n");
  return evhttp_send_reply(req, HTTP_OK, "OK", buf.get());
}

}  // namespace moonfire_nvr
