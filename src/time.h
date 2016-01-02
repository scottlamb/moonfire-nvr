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
// time.h: functions dealing with (wall) time.

#ifndef MOONFIRE_NVR_TIME_H
#define MOONFIRE_NVR_TIME_H

#include <math.h>
#include <time.h>

#include <mutex>

namespace moonfire_nvr {

constexpr long kNanos = 1000000000;

class WallClock {
 public:
  virtual ~WallClock() {}
  virtual struct timespec Now() const = 0;
  virtual void Sleep(struct timespec) = 0;
};

class SimulatedClock : public WallClock {
 public:
  SimulatedClock() : now_({0, 0}) {}
  struct timespec Now() const final;
  void Sleep(struct timespec req) final;

 private:
  mutable std::mutex mu_;
  struct timespec now_;
};

inline struct timespec SecToTimespec(double sec) {
  double intpart;
  double fractpart = modf(sec, &intpart);
  return {static_cast<time_t>(intpart), static_cast<long>(fractpart * kNanos)};
}

// Returns the real wall clock, which will never be deleted.
WallClock *GetRealClock();

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_TIME_H
