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
// testutil.h: utilities for testing.

#ifndef MOONFIRE_NVR_TESTUTIL_H
#define MOONFIRE_NVR_TESTUTIL_H

#include <glog/logging.h>
#include <gmock/gmock.h>
#include <re2/stringpiece.h>

#include "filesystem.h"
#include "http.h"
#include "uuid.h"

namespace moonfire_nvr {

// Create or empty the given test directory, or die.
// Returns the full path.
std::string PrepareTempDirOrDie(const std::string &test_name);

// Write the given file contents to the given path, or die.
void WriteFileOrDie(const std::string &path, re2::StringPiece contents);
void WriteFileOrDie(const std::string &path, EvBuffer *buf);

// Read the contents of the given path, or die.
std::string ReadFileOrDie(const std::string &path);

// A scoped log sink for testing that the right log messages are sent.
// Modelled after glog's "mock-log.h", which is not exported.
// Use as follows:
//
// {
//   ScopedMockLog log;
//   EXPECT_CALL(log, Log(ERROR, _, HasSubstr("blah blah")));
//   log.Start();
//   ThingThatLogs();
// }
class ScopedMockLog : public google::LogSink {
 public:
  ~ScopedMockLog() final { google::RemoveLogSink(this); }

  // Start logging to this sink.
  // This is not done at construction time so that it's possible to set
  // expectations first, which is important if some background thread is
  // already logging.
  void Start() { google::AddLogSink(this); }

  // Set expectations here.
  MOCK_METHOD3(Log, void(google::LogSeverity severity,
                         const std::string &full_filename,
                         const std::string &message));

 private:
  struct LogEntry {
    google::LogSeverity severity = -1;
    std::string full_filename;
    std::string message;
  };

  // This method is called with locks held and thus shouldn't call Log.
  // It just stashes away the log entry for later.
  void send(google::LogSeverity severity, const char *full_filename,
            const char *base_filename, int line, const tm *tm_time,
            const char *message, size_t message_len) final {
    pending_.severity = severity;
    pending_.full_filename = full_filename;
    pending_.message.assign(message, message_len);
  }

  // This method is always called after send() without locks.
  // It does the actual work of calling Log. It moves data away from
  // pending_ in case Log() logs itself (causing a nested call to send() and
  // WaitTillSent()).
  void WaitTillSent() final {
    LogEntry entry = std::move(pending_);
    Log(entry.severity, entry.full_filename, entry.message);
  }

  LogEntry pending_;
};

class MockUuidGenerator : public UuidGenerator {
 public:
  MOCK_METHOD0(Generate, Uuid());
};

class MockFile : public File {
 public:
  MOCK_CONST_METHOD0(name, const std::string &());
  MOCK_METHOD3(Access, int(const char *, int, int));
  MOCK_METHOD0(Close, int());

  // The std::unique_ptr<File> variants of Open are wrapped here because gmock's
  // SetArgPointee doesn't work well with std::unique_ptr.

  int Open(const char *path, int flags, std::unique_ptr<File> *f) final {
    File *f_tmp = nullptr;
    int ret = OpenRaw(path, flags, &f_tmp);
    f->reset(f_tmp);
    return ret;
  }

  int Open(const char *path, int flags, mode_t mode,
           std::unique_ptr<File> *f) final {
    File *f_tmp = nullptr;
    int ret = OpenRaw(path, flags, mode, &f_tmp);
    f->reset(f_tmp);
    return ret;
  }

  MOCK_METHOD3(Open, int(const char *, int, int *));
  MOCK_METHOD4(Open, int(const char *, int, mode_t, int *));
  MOCK_METHOD3(OpenRaw, int(const char *, int, File **));
  MOCK_METHOD4(OpenRaw, int(const char *, int, mode_t, File **));
  MOCK_METHOD3(Read, int(void *, size_t, size_t *));
  MOCK_METHOD1(Stat, int(struct stat *));
  MOCK_METHOD0(Sync, int());
  MOCK_METHOD1(Truncate, int(off_t));
  MOCK_METHOD1(Unlink, int(const char *));
  MOCK_METHOD2(Write, int(re2::StringPiece, size_t *));
};

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_TESTUTIL_H
