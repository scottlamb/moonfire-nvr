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
// moonfire-nvr-main.cc: main program. This should be kept as short as
// practical, so that individual parts of the program can be tested with the
// googletest framework.

#include <fcntl.h>
#include <signal.h>
#include <sys/types.h>
#include <unistd.h>

#include <string>

#include <event2/buffer.h>
#include <event2/event.h>
#include <event2/event_struct.h>
#include <event2/http.h>
#include <gflags/gflags.h>
#include <google/protobuf/io/zero_copy_stream_impl.h>
#include <google/protobuf/text_format.h>
#include <glog/logging.h>

#include "config.pb.h"
#include "ffmpeg.h"
#include "profiler.h"
#include "moonfire-nvr.h"

DEFINE_string(config, "/etc/moonfire-nvr.conf", "Path to configuration file.");

namespace {

const struct timeval kLogFlushInterval = {1, 0};

struct event_base* base;

void EventLogCallback(int severity, const char* msg) {
  int vlog_level = 0;
  google::LogSeverity glog_level;
  if (severity <= EVENT_LOG_DEBUG) {
    vlog_level = 1;
    glog_level = google::GLOG_INFO;
  } else if (severity <= EVENT_LOG_MSG) {
    glog_level = google::GLOG_INFO;
  } else if (severity <= EVENT_LOG_WARN) {
    glog_level = google::GLOG_WARNING;
  } else {
    glog_level = google::GLOG_ERROR;
  }

  if (vlog_level > 0 && !VLOG_IS_ON(vlog_level)) {
    return;
  }
  google::LogMessage("libevent", 0, glog_level).stream() << msg;
}

bool LoadConfiguration(const std::string& filename,
                       moonfire_nvr::Config* config) {
  int fd = open(filename.c_str(), O_RDONLY);
  if (fd == -1) {
    PLOG(ERROR) << "can't open " << filename;
    return false;
  }
  google::protobuf::io::FileInputStream file(fd);
  file.SetCloseOnDelete(true);
  // TODO(slamb): report more specific errors via an ErrorCollector.
  if (!google::protobuf::TextFormat::Parse(&file, config)) {
    LOG(ERROR) << "can't parse " << filename;
  }
  return true;
}

// Called on SIGTERM or SIGINT.
void SignalCallback(evutil_socket_t, short, void*) {
  event_base_loopexit(base, nullptr);
}

void FlushLogsCallback(evutil_socket_t, short, void* ev) {
  google::FlushLogFiles(google::GLOG_INFO);
  CHECK_EQ(0,
           event_add(reinterpret_cast<struct event*>(ev), &kLogFlushInterval));
}

void HttpCallback(evhttp_request* req, void* arg) {
  auto* nvr = reinterpret_cast<moonfire_nvr::Nvr*>(arg);
  nvr->HttpCallback(req);
}

}  // namespace

int main(int argc, char** argv) {
  GOOGLE_PROTOBUF_VERIFY_VERSION;
  google::ParseCommandLineFlags(&argc, &argv, true);
  google::InitGoogleLogging(argv[0]);
  google::InstallFailureSignalHandler();
  signal(SIGPIPE, SIG_IGN);

  moonfire_nvr::Config config;
  if (!LoadConfiguration(FLAGS_config, &config)) {
    exit(1);
  }

  event_set_log_callback(&EventLogCallback);
  LOG(INFO) << "libevent: compiled with version " << LIBEVENT_VERSION
            << ", running with version " << event_get_version();
  base = CHECK_NOTNULL(event_base_new());

  std::unique_ptr<moonfire_nvr::Nvr> nvr(new moonfire_nvr::Nvr);
  std::string error_msg;
  if (!nvr->Init(config, &error_msg)) {
    LOG(ERROR) << "Unable to initialize: " << error_msg;
    exit(1);
  }

  evhttp* http = CHECK_NOTNULL(evhttp_new(base));
  moonfire_nvr::RegisterProfiler(base, http);
  evhttp_set_gencb(http, &HttpCallback, nvr.get());
  if (evhttp_bind_socket(http, "0.0.0.0", config.http_port()) != 0) {
    LOG(ERROR) << "Unable to bind to port " << config.http_port();
    exit(1);
  }

  // Register for termination signals.
  struct event ev_sigterm;
  struct event ev_sigint;
  CHECK_EQ(0, event_assign(&ev_sigterm, base, SIGTERM, EV_SIGNAL | EV_PERSIST,
                           &SignalCallback, nullptr));
  CHECK_EQ(0, event_assign(&ev_sigint, base, SIGINT, EV_SIGNAL | EV_PERSIST,
                           &SignalCallback, nullptr));
  CHECK_EQ(0, event_add(&ev_sigterm, nullptr));
  CHECK_EQ(0, event_add(&ev_sigint, nullptr));

  // Flush the logfiles regularly for debuggability.
  struct event ev_flushlogs;
  CHECK_EQ(0, event_assign(&ev_flushlogs, base, 0, 0, &FlushLogsCallback,
                           &ev_flushlogs));
  CHECK_EQ(0, event_add(&ev_flushlogs, &kLogFlushInterval));

  // Wait for events.
  LOG(INFO) << "Main thread entering event loop.";
  CHECK_EQ(0, event_base_loop(base, 0));

  LOG(INFO) << "Shutting down.";
  google::FlushLogFiles(google::GLOG_INFO);
  nvr.reset();
  LOG(INFO) << "Done.";
  google::ShutdownGoogleLogging();
  exit(0);
}
