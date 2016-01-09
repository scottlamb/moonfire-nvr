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
// filesystem.h: helpers for dealing with the local filesystem.

#ifndef MOONFIRE_NVR_FILESYSTEM_H
#define MOONFIRE_NVR_FILESYSTEM_H

#include <dirent.h>
#include <stdarg.h>
#include <sys/stat.h>
#include <sys/types.h>

#include <memory>
#include <functional>
#include <string>

#include <event2/buffer.h>
#include <event2/http.h>
#include <glog/logging.h>
#include <re2/stringpiece.h>

namespace moonfire_nvr {

// Return value for *ForEach callbacks.
enum class IterationControl {
  kContinue,  // indicates the caller should proceed with the loop.
  kBreak      // indicates the caller should terminate the loop with success.
};

// Represents an open file. All methods but Close() are thread-safe.
class File {
 public:
  // Close the file, ignoring the result.
  virtual ~File() {}

  // Close the file, returning 0 on success or errno>0 on failure.
  // Already closed is considered a success.
  virtual int Close() = 0;

  // fsync(), returning 0 on success or errno>0 on failure.
  virtual int Sync() = 0;

  // ftruncate(), returning 0 on success or errno>0 on failure.
  virtual int Truncate(off_t length) = 0;

  // read(), returning 0 on success or errno>0 on failure.
  // On success, |bytes_read| will be updated.
  virtual int Read(void *buf, size_t count, size_t *bytes_read) = 0;

  // fstat(), returning 0 on success or errno>0 on failure.
  virtual int Stat(struct stat *buf) = 0;

  // Write to the file, returning 0 on success or errno>0 on failure.
  // On success, |bytes_written| will be updated.
  virtual int Write(re2::StringPiece data, size_t *bytes_written) = 0;
};

// Interface to the local filesystem. There's typically one per program,
// but it's an abstract class for testability. Thread-safe.
class Filesystem {
 public:
  virtual ~Filesystem() {}

  // Execute |fn| for each directory entry in |dir_path|, stopping early
  // (successfully) if the callback returns IterationControl::kBreak.
  //
  // On success, returns true.
  // On failure, returns false and updates |error_msg|.
  virtual bool DirForEach(const char *dir_path,
                          std::function<IterationControl(const dirent *)> fn,
                          std::string *error_msg) = 0;

  // open() the specified path, returning 0 on success or errno>0 on failure.
  // On success, |f| is populated with an open file.
  virtual int Open(const char *path, int flags, std::unique_ptr<File> *f) = 0;
  virtual int Open(const char *path, int flags, mode_t mode,
                   std::unique_ptr<File> *f) = 0;

  // mkdir() the specified path, returning 0 on success or errno>0 on failure.
  virtual int Mkdir(const char *path, mode_t mode) = 0;

  // rmdir() the specified path, returning 0 on success or errno>0 on failure.
  virtual int Rmdir(const char *path) = 0;

  // stat() the specified path, returning 0 on success or errno>0 on failure.
  virtual int Stat(const char *path, struct stat *buf) = 0;

  // unlink() the specified file, returning 0 on success or errno>0 on failure.
  virtual int Unlink(const char *path) = 0;
};

// Get the (singleton) real filesystem, which is never deleted.
Filesystem *GetRealFilesystem();

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_FILESYSTEM_H
