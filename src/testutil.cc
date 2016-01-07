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
#include <fcntl.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>

#include <glog/logging.h>

#include "filesystem.h"
#include "string.h"

namespace moonfire_nvr {

namespace {

bool DeleteChildrenRecursively(const char *dirname, std::string *error_msg) {
  bool ok = true;
  auto fn = [&dirname, &ok, error_msg](const struct dirent *ent) {
    std::string name(ent->d_name);
    std::string path = StrCat(dirname, "/", name);
    if (name == "." || name == "..") {
      return IterationControl::kContinue;
    }
    bool is_dir = (ent->d_type == DT_DIR);
    if (ent->d_type == DT_UNKNOWN) {
      struct stat buf;
      int ret = GetRealFilesystem()->Stat(path.c_str(), &buf);
      CHECK_EQ(ret, 0) << path << ": " << strerror(ret);
      is_dir = S_ISDIR(buf.st_mode);
    }
    if (is_dir) {
      ok = ok && DeleteChildrenRecursively(path.c_str(), error_msg);
      if (!ok) {
        return IterationControl::kBreak;
      }
      int ret = GetRealFilesystem()->Rmdir(path.c_str());
      if (ret != 0) {
        *error_msg = StrCat("rmdir failed on ", path, ": ", strerror(ret));
        ok = false;
        return IterationControl::kBreak;
      }
    } else {
      int ret = GetRealFilesystem()->Unlink(path.c_str());
      if (ret != 0) {
        *error_msg = StrCat("unlink failed on ", path, ": ", strerror(ret));
        ok = false;
        return IterationControl::kBreak;
      }
    }
    return IterationControl::kContinue;
  };
  if (!GetRealFilesystem()->DirForEach(dirname, fn, error_msg)) {
    return false;
  }
  return ok;
}

}  // namespace

std::string PrepareTempDirOrDie(const std::string &test_name) {
  std::string dirname = StrCat("/tmp/test.", test_name);
  int ret = GetRealFilesystem()->Mkdir(dirname.c_str(), 0700);
  if (ret != 0) {
    CHECK_EQ(ret, EEXIST) << "mkdir failed: " << strerror(ret);
    std::string error_msg;
    CHECK(DeleteChildrenRecursively(dirname.c_str(), &error_msg)) << error_msg;
  }
  return dirname;
}

void WriteFileOrDie(const std::string &path, re2::StringPiece contents) {
  std::unique_ptr<File> f;
  int ret = GetRealFilesystem()->Open(path.c_str(),
                                      O_WRONLY | O_CREAT | O_TRUNC, 0600, &f);
  CHECK_EQ(ret, 0) << "open " << path << ": " << strerror(ret);
  while (!contents.empty()) {
    size_t written;
    ret = f->Write(contents, &written);
    CHECK_EQ(ret, 0) << "write " << path << ": " << strerror(ret);
    contents.remove_prefix(written);
  }
  ret = f->Close();
  CHECK_EQ(ret, 0) << "close " << path << ": " << strerror(ret);
}

}  // namespace moonfire_nvr
