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
// testutil.cc: implementation of testutil.h interface.

#include "testutil.h"

#include <dirent.h>
#include <errno.h>
#include <string.h>
#include <sys/types.h>

#include <fstream>

#include <glog/logging.h>

#include "filesystem.h"

namespace moonfire_nvr {

namespace {

bool DeleteChildrenRecursively(const std::string &dirname,
                               std::string *error_msg) {
  bool ok = true;
  auto fn = [&dirname, &ok, error_msg](const struct dirent *ent) {
    std::string name(ent->d_name);
    std::string path = dirname + "/" + name;
    if (name == "." || name == "..") {
      return IterationControl::kContinue;
    }
    bool is_dir = (ent->d_type == DT_DIR);
    if (ent->d_type == DT_UNKNOWN) {
      struct stat buf;
      PCHECK(stat(path.c_str(), &buf) == 0) << path;
      is_dir = S_ISDIR(buf.st_mode);
    }
    if (is_dir) {
      ok = ok && DeleteChildrenRecursively(path, error_msg);
      if (!ok) {
        return IterationControl::kBreak;
      }
      if (rmdir(path.c_str()) != 0) {
        *error_msg =
            std::string("rmdir failed on ") + path + ": " + strerror(errno);
        ok = false;
        return IterationControl::kBreak;
      }
    } else {
      if (unlink(path.c_str()) != 0) {
        *error_msg =
            std::string("unlink failed on ") + path + ": " + strerror(errno);
        ok = false;
        return IterationControl::kBreak;
      }
    }
    return IterationControl::kContinue;
  };
  if (!DirForEach(dirname, fn, error_msg)) {
    return false;
  }
  return ok;
}

}  // namespace

std::string PrepareTempDirOrDie(const std::string &test_name) {
  std::string dirname = std::string("/tmp/test.") + test_name;
  int res = mkdir(dirname.c_str(), 0700);
  if (res != 0) {
    int err = errno;
    CHECK_EQ(err, EEXIST) << "mkdir failed: " << strerror(err);
    std::string error_msg;
    CHECK(DeleteChildrenRecursively(dirname, &error_msg)) << error_msg;
  }
  return dirname;
}

void WriteFileOrDie(const std::string &path, const std::string &contents) {
  std::ofstream f(path);
  f << contents;
  f.close();
  CHECK(!f.fail()) << "failed to write: " << path;
}

}  // namespace moonfire_nvr
