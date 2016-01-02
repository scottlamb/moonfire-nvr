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
// time.cc: implementation of time.h interface.

#include "time.h"

#include <errno.h>
#include <string.h>

#include <glog/logging.h>

namespace moonfire_nvr {

namespace {

class RealClock : public WallClock {
 public:
  struct timespec Now() const final {
    struct timespec now;
    CHECK_EQ(0, clock_gettime(CLOCK_REALTIME, &now)) << strerror(errno);
    return now;
  }

  void Sleep(struct timespec req) final {
    struct timespec rem;
    while (true) {
      int ret = nanosleep(&req, &rem);
      if (ret != 0 && errno != EINTR) {
        PLOG(FATAL) << "nanosleep";
      }
      if (ret == 0) {
        return;
      }
      req = rem;
    }
  }
};

}  // namespace

// Returns the real wall clock, which will never be deleted.
WallClock *GetRealClock() {
  static RealClock *real_clock = new RealClock;  // never deleted.
  return real_clock;
}

struct timespec SimulatedClock::Now() const {
  std::lock_guard<std::mutex> l(mu_);
  return now_;
}

void SimulatedClock::Sleep(struct timespec req) {
  std::lock_guard<std::mutex> l(mu_);
  now_.tv_sec += req.tv_sec;
  now_.tv_nsec += req.tv_nsec;
  if (now_.tv_nsec > kNanos) {
    now_.tv_nsec -= kNanos;
    now_.tv_sec++;
  }
}

}  // namespace moonfire_nvr
