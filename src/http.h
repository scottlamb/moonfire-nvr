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
// http.h: classes for HTTP serving. In particular, there are helpers for
// serving HTTP byte range requests with libevent.

#ifndef MOONFIRE_NVR_HTTP_H
#define MOONFIRE_NVR_HTTP_H

#include <dirent.h>
#include <stdarg.h>
#include <sys/stat.h>
#include <sys/types.h>

#include <functional>
#include <iostream>
#include <string>

#include <event2/buffer.h>
#include <event2/http.h>
#include <glog/logging.h>
#include <re2/stringpiece.h>

namespace moonfire_nvr {

// Wrapped version of libevent's "struct evbuffer" which uses RAII and simply
// aborts the process if allocations fail. (Moonfire NVR is intended to run on
// Linux systems with the default vm.overcommit_memory=0, so there's probably
// no point in trying to gracefully recover from a condition that's unlikely
// to ever happen.)
class EvBuffer {
 public:
  EvBuffer() { buf_ = CHECK_NOTNULL(evbuffer_new()); }
  EvBuffer(const EvBuffer &) = delete;
  EvBuffer &operator=(const EvBuffer &) = delete;
  ~EvBuffer() { evbuffer_free(buf_); }

  struct evbuffer *get() {
    return buf_;
  }

  void Add(const re2::StringPiece &s) {
    CHECK_EQ(0, evbuffer_add(buf_, s.data(), s.size()));
  }

  void AddPrintf(const char *fmt, ...) __attribute__((format(printf, 2, 3))) {
    va_list argp;
    va_start(argp, fmt);
    CHECK_LE(0, evbuffer_add_vprintf(buf_, fmt, argp));
    va_end(argp);
  }

  // Delegates to evbuffer_add_file.
  // On success, |fd| will be closed by libevent. On failure, it remains open.
  bool AddFile(int fd, ev_off_t offset, ev_off_t length,
               std::string *error_message);

 private:
  struct evbuffer *buf_;
};

struct ByteRange {
  ByteRange() {}
  ByteRange(int64_t begin, int64_t end) : begin(begin), end(end) {}
  int64_t begin = 0;
  int64_t end = 0;  // exclusive.
  bool operator==(const ByteRange &o) const {
    return begin == o.begin && end == o.end;
  }
};

inline std::ostream &operator<<(std::ostream &out, const ByteRange &range) {
  out << "[" << range.begin << ", " << range.end << ")";
  return out;
}

// Helper for sending HTTP errors based on POSIX error returns.
void HttpSendError(evhttp_request *req, int http_err, const std::string &prefix,
                   int posix_errno);

class VirtualFile {
 public:
  virtual ~VirtualFile() {}

  // Return the given property of the file.
  virtual int64_t size() const = 0;
  virtual time_t last_modified() const = 0;
  virtual std::string etag() const = 0;
  virtual std::string mime_type() const = 0;
  virtual std::string filename() const = 0;  // for logging.

  // Add the given range of the file to the buffer.
  virtual bool AddRange(ByteRange range, EvBuffer *buf,
                        std::string *error_message) const = 0;
};

// Serve an HTTP request |req| from |file|, handling byte range and
// conditional serving. (Similar to golang's http.ServeContent.)
//
// |file| only needs to live through the call to HttpServe itself.
// This contract may change in the future; currently all the ranges are added
// at the beginning of the request, so if large memory-backed buffers (as
// opposed to file-backed buffers) are used, the program's memory usage will
// spike, even if the HTTP client aborts early in the request. If this becomes
// problematic, this interface may change to take advantage of
// evbuffer_add_cb, adding buffers incrementally, and some mechanism will be
// added to guarantee VirtualFile objects outlive the HTTP requests they serve.
void HttpServe(const VirtualFile &file, evhttp_request *req);

// Serve a file over HTTP. Expects the caller to supply a sanitized |filename|
// (rather than taking it straight from the path specified in |req|).
void HttpServeFile(evhttp_request *req, const std::string &mime_type,
                   const std::string &filename, const struct stat &statbuf);

namespace internal {

// Value to represent result of parsing HTTP 1.1 "Range:" header.
enum class RangeHeaderType {
  // Ignore the header, serving all bytes in the file.
  kAbsentOrInvalid,

  // The server SHOULD return a response with status 416 (Requested range not
  // satisfiable).
  kNotSatisfiable,

  // The server SHOULD return a response with status 406 (Partial Content).
  kSatisfiable
};

// Parse an HTTP 1.1 "Range:" header value, following RFC 2616 section 14.35.
// This function is for use by HttpServe; it is exposed for testing only.
//
// |value| on entry should be the header value (after the ": "), or nullptr.
// |size| on entry should be the number of bytes available to serve.
// On kSatisfiable return, |ranges| will be filled with the satisfiable ranges.
// Otherwise, its contents are undefined.
RangeHeaderType ParseRangeHeader(const char *value, int64_t size,
                                 std::vector<ByteRange> *ranges);

}  // namespace internal

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_HTTP_H
