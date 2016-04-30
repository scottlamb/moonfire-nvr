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

namespace {

class RealFile : public File {
 public:
  RealFile(re2::StringPiece name, int fd)
      : name_(name.data(), name.size()), fd_(fd) {}
  RealFile(const RealFile &) = delete;
  void operator=(const RealFile &) = delete;

  ~RealFile() final { Close(); }

  const std::string &name() const { return name_; }

  int Access(const char *path, int mode, int flags) final {
    return faccessat(fd_, path, mode, flags) < 0 ? errno : 0;
  }

  int Close() final {
    if (fd_ < 0) {
      return 0;
    }
    int ret;
    while ((ret = close(fd_)) != 0 && errno == EINTR)
      ;
    if (ret != 0) {
      return errno;
    }
    fd_ = -1;
    return 0;
  }

  int Open(const char *path, int flags, int *fd) final {
    return Open(path, flags, 0, fd);
  }

  int Open(const char *path, int flags, std::unique_ptr<File> *f) final {
    return Open(path, flags, 0, f);
  }

  int Open(const char *path, int flags, mode_t mode, int *fd) final {
    int ret = openat(fd_, path, flags, mode);
    if (ret < 0) {
      return errno;
    }
    *fd = ret;
    return 0;
  }

  int Open(const char *path, int flags, mode_t mode,
           std::unique_ptr<File> *f) final {
    int ret = openat(fd_, path, flags, mode);
    if (ret < 0) {
      return errno;
    }
    f->reset(new RealFile(StrCat(name_, "/", path), ret));
    return 0;
  }

  int Read(void *buf, size_t size, size_t *bytes_read) final {
    ssize_t ret;
    while ((ret = read(fd_, buf, size)) == -1 && errno == EINTR)
      ;
    if (ret < 0) {
      return errno;
    }
    *bytes_read = static_cast<size_t>(ret);
    return 0;
  }

  int Stat(struct stat *buf) final { return (fstat(fd_, buf) < 0) ? errno : 0; }

  int Sync() final { return (fsync(fd_) < 0) ? errno : 0; }

  int Truncate(off_t length) final {
    return (ftruncate(fd_, length) < 0) ? errno : 0;
  }

  int Unlink(const char *pathname) {
    return unlinkat(fd_, pathname, 0) != 0 ? errno : 0;
  }

  int Write(re2::StringPiece data, size_t *bytes_written) final {
    ssize_t ret;
    while ((ret = write(fd_, data.data(), data.size())) == -1 && errno == EINTR)
      ;
    if (ret < 0) {
      return errno;
    }
    *bytes_written = static_cast<size_t>(ret);
    return 0;
  }

 private:
  std::string name_;
  int fd_ = -1;
};

class RealFilesystem : public Filesystem {
 public:
  bool DirForEach(const char *dir_path,
                  std::function<IterationControl(const dirent *)> fn,
                  std::string *error_message) final {
    DIR *owned_dir = opendir(dir_path);
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

  int Open(const char *path, int flags, std::unique_ptr<File> *f) final {
    return Open(path, flags, 0, f);
  }

  int Open(const char *path, int flags, mode_t mode,
           std::unique_ptr<File> *f) final {
    int ret = open(path, flags, mode);
    if (ret < 0) {
      return errno;
    }
    f->reset(new RealFile(path, ret));
    return 0;
  }

  int Mkdir(const char *path, mode_t mode) final {
    return (mkdir(path, mode) < 0) ? errno : 0;
  }

  int Rmdir(const char *path) final { return (rmdir(path) < 0) ? errno : 0; }

  int Stat(const char *path, struct stat *buf) final {
    return (stat(path, buf) < 0) ? errno : 0;
  }

  int Unlink(const char *path) final { return (unlink(path) < 0) ? errno : 0; }
};

}  // namespace

Filesystem *GetRealFilesystem() {
  static Filesystem *real_filesystem = new RealFilesystem;
  return real_filesystem;
}

}  // namespace moonfire_nvr
