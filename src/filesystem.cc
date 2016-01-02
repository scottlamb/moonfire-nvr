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
// filesystem.cc: See filesystem.h.

#include "filesystem.h"

#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/queue.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#include <cstdlib>
#include <memory>

#include <event2/buffer.h>
#include <event2/event.h>
#include <event2/keyvalq_struct.h>
#include <event2/http.h>
#include <gperftools/profiler.h>
#include <glog/logging.h>

#include "string.h"

namespace moonfire_nvr {

bool DirForEach(const std::string &dir_path,
                std::function<IterationControl(const dirent *)> fn,
                std::string *error_message) {
  DIR *owned_dir = opendir(dir_path.c_str());
  if (owned_dir == nullptr) {
    int err = errno;
    *error_message =
        StrCat("Unable to examine ", dir_path, ": ", strerror(err));
    return false;
  }
  struct dirent *ent;
  while (errno = 0, (ent = readdir(owned_dir)) != nullptr) {
    if (fn(ent) == IterationControl::kBreak) {
      closedir(owned_dir);
      return true;
    }
  }
  int err = errno;
  closedir(owned_dir);
  if (err != 0) {
    *error_message = StrCat("readdir failed: ", strerror(err));
    return false;
  }
  return true;
}

}  // namespace moonfire_nvr
