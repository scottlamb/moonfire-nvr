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
#include <glog/logging.h>

#include "ffmpeg.h"
#include "profiler.h"
#include "moonfire-db.h"
#include "moonfire-nvr.h"
#include "sqlite.h"
#include "string.h"
#include "web.h"

using moonfire_nvr::StrCat;

DEFINE_int32(http_port, 0, "");
DEFINE_string(db_dir, "", "");
DEFINE_string(sample_file_dir, "", "");

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

// Called on SIGTERM or SIGINT.
void SignalCallback(evutil_socket_t, short, void*) {
  event_base_loopexit(base, nullptr);
}

void FlushLogsCallback(evutil_socket_t, short, void* ev) {
  google::FlushLogFiles(google::GLOG_INFO);
  CHECK_EQ(0,
           event_add(reinterpret_cast<struct event*>(ev), &kLogFlushInterval));
}

}  // namespace

// Note that main never returns; it calls exit on either success or failure.
// This avoids the need to design an orderly shutdown for all dependencies,
// instead letting the OS clean up memory allocations en masse. State may be
// allocated in whatever way is most convenient: on the stack, in a unique_ptr
// (that may never go out of scope), or as a bare pointer that is never
// deleted.
int main(int argc, char** argv) {
  google::ParseCommandLineFlags(&argc, &argv, true);
  google::InitGoogleLogging(argv[0]);
  google::InstallFailureSignalHandler();
  signal(SIGPIPE, SIG_IGN);

  if (FLAGS_sample_file_dir.empty()) {
    LOG(ERROR) << "--sample_file_dir must be specified; exiting.";
    exit(1);
  }

  if (FLAGS_db_dir.empty()) {
    LOG(ERROR) << "--db_dir must be specified; exiting.";
    exit(1);
  }

  if (FLAGS_http_port == 0) {
    LOG(ERROR) << "--http_port must be specified; exiting.";
    exit(1);
  }

  moonfire_nvr::Environment env;
  env.clock = moonfire_nvr::GetRealClock();
  env.video_source = moonfire_nvr::GetRealVideoSource();

  std::unique_ptr<moonfire_nvr::File> sample_file_dir;
  std::string sample_file_dirname = FLAGS_sample_file_dir;
  int ret = moonfire_nvr::GetRealFilesystem()->Open(
      sample_file_dirname.c_str(), O_DIRECTORY | O_RDONLY, &sample_file_dir);
  if (ret != 0) {
    LOG(ERROR) << "Unable to open --sample_file_dir=" << sample_file_dirname
               << ": " << strerror(ret) << "; exiting.";
    exit(1);
  }

  // Separately, ensure the sample file directory is writable.
  // (Opening the directory above with O_DIRECTORY|O_RDWR doesn't work even
  // when the directory is writable; it fails with EISDIR.)
  ret = sample_file_dir->Access(".", W_OK, 0);
  if (ret != 0) {
    LOG(ERROR) << "--sample_file_dir=" << sample_file_dirname
               << " is not writable: " << strerror(ret) << "; exiting.";
    exit(1);
  }

  env.sample_file_dir = sample_file_dir.release();

  moonfire_nvr::Database db;
  std::string error_msg;
  std::string db_path = StrCat(FLAGS_db_dir, "/db");
  if (!db.Open(db_path.c_str(), SQLITE_OPEN_READWRITE, &error_msg)) {
    LOG(ERROR) << error_msg << "; exiting.";
    exit(1);
  }

  moonfire_nvr::MoonfireDatabase mdb;
  CHECK(mdb.Init(&db, &error_msg)) << error_msg;
  env.mdb = &mdb;

  moonfire_nvr::WebInterface web(&env);

  event_set_log_callback(&EventLogCallback);
  LOG(INFO) << "libevent: compiled with version " << LIBEVENT_VERSION
            << ", running with version " << event_get_version();
  base = CHECK_NOTNULL(event_base_new());

  std::unique_ptr<moonfire_nvr::Nvr> nvr(new moonfire_nvr::Nvr(&env));
  if (!nvr->Init(&error_msg)) {
    LOG(ERROR) << "Unable to initialize: " << error_msg << "; exiting.";
    exit(1);
  }

  evhttp* http = CHECK_NOTNULL(evhttp_new(base));
  moonfire_nvr::RegisterProfiler(base, http);
  web.Register(http);
  if (evhttp_bind_socket(http, "0.0.0.0", FLAGS_http_port) != 0) {
    LOG(ERROR) << "Unable to bind to --http_port=" << FLAGS_http_port
               << "; exiting.";
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
