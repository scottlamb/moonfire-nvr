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
// profiler.cc: See profiler.h.

#include "profiler.h"

#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#include <cstdlib>
#include <memory>

#include <event2/buffer.h>
#include <event2/event.h>
#include <event2/http.h>
#include <gperftools/profiler.h>
#include <glog/logging.h>

#include "http.h"
#include "string.h"

namespace moonfire_nvr {

namespace {

const int kDefaultProfileSeconds = 30;

// Only a single CPU profile may be active at once. Track if it is active now.
bool profiling;

struct ProfileRequestContext {
#define TEMPLATE "/tmp/moonfire-nvr.profile.XXXXXX"
  char filename[sizeof(TEMPLATE)] = TEMPLATE;
#undef TEMPLATE
  evhttp_request *req = nullptr;
  event *timer = nullptr;
  int fd = -1;
};

// End a CPU profile. Serve the result from the temporary file and delete it.
void EndProfileCallback(evutil_socket_t, short, void *arg) {
  CHECK(profiling);
  ProfilerStop();
  profiling = false;
  std::unique_ptr<ProfileRequestContext> ctx(
      reinterpret_cast<ProfileRequestContext *>(arg));
  if (unlink(ctx->filename) < 0) {
    int err = errno;
    LOG(WARNING) << "Unable to unlink temporary profile file: " << ctx->filename
                 << ": " << strerror(err);
  }
  event_free(ctx->timer);
  struct stat statbuf;
  if (fstat(ctx->fd, &statbuf) < 0) {
    close(ctx->fd);
    return HttpSendError(ctx->req, HTTP_INTERNAL, "fstat: ", errno);
  }
  EvBuffer buf;
  std::string error_message;
  if (!buf.AddFile(ctx->fd, 0, statbuf.st_size, &error_message)) {
    evhttp_send_error(ctx->req, HTTP_INTERNAL,
                      EscapeHtml(error_message).c_str());
    close(ctx->fd);
    return;
  }
  evhttp_send_reply(ctx->req, HTTP_OK, "OK", buf.get());
}

// Start a CPU profile. Creates a temporary file for the profiler library
// to use and schedules a call to EndProfileCallback.
void StartProfileCallback(struct evhttp_request *req, void *arg) {
  auto *base = reinterpret_cast<event_base *>(arg);
  if (evhttp_request_get_command(req) != EVHTTP_REQ_GET) {
    return evhttp_send_error(req, HTTP_BADMETHOD, "only GET allowed");
  }
  if (profiling) {
    return evhttp_send_error(req, HTTP_SERVUNAVAIL,
                             "Profiling already in progress");
  }
  struct timeval timeout = {0, 0};
  QueryParameters params(evhttp_request_get_uri(req));
  const char *seconds_value = params.Get("seconds");
  timeout.tv_sec =
      seconds_value == nullptr ? kDefaultProfileSeconds : atoi(seconds_value);
  if (timeout.tv_sec <= 0) {
    return evhttp_send_error(req, HTTP_BADREQUEST, "invalid seconds");
  }

  auto *ctx = new ProfileRequestContext;
  ctx->fd = mkstemp(ctx->filename);
  if (ctx->fd < 0) {
    delete ctx;
    return HttpSendError(req, HTTP_INTERNAL, "mkstemp: ", errno);
  }

  if (ProfilerStart(ctx->filename) == 0) {
    delete ctx;
    return evhttp_send_error(req, HTTP_INTERNAL, "ProfilerStart failed");
  }
  profiling = true;
  ctx->req = req;
  ctx->timer = evtimer_new(base, &EndProfileCallback, ctx);
  evtimer_add(ctx->timer, &timeout);
}

}  // namespace

void RegisterProfiler(event_base *base, evhttp *http) {
  evhttp_set_cb(http, "/pprof/profile", &StartProfileCallback, base);
}

}  // namespace moonfire_nvr
