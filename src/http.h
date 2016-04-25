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
#include <sys/queue.h>
#include <sys/stat.h>
#include <sys/types.h>

#include <functional>
#include <iostream>
#include <memory>
#include <string>

#include <event2/buffer.h>
#include <event2/keyvalq_struct.h>
#include <event2/http.h>
#include <glog/logging.h>
#include <re2/stringpiece.h>

#include "filesystem.h"
#include "string.h"

namespace moonfire_nvr {

// Single-use object to represent a set of HTTP query parameters.
class QueryParameters {
 public:
  // Parse parameters from the given URI.
  // Caller should check ok() afterward.
  QueryParameters(const char *uri) {
    TAILQ_INIT(&me_);
    ok_ = evhttp_parse_query(uri, &me_) == 0;
  }
  QueryParameters(const QueryParameters &) = delete;
  void operator=(const QueryParameters &) = delete;

  ~QueryParameters() { evhttp_clear_headers(&me_); }

  bool ok() const { return ok_; }
  const char *Get(const char *param) const {
    return evhttp_find_header(&me_, param);
  }

 private:
  struct evkeyvalq me_;
  bool ok_ = false;
};

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

  void AddReference(const void *data, size_t datlen,
                    evbuffer_ref_cleanup_cb cleanupfn, void *cleanupfn_arg) {
    CHECK_EQ(
        0, evbuffer_add_reference(buf_, data, datlen, cleanupfn, cleanupfn_arg))
        << strerror(errno);
  }

 private:
  struct evbuffer *buf_;
};

struct ByteRange {
  ByteRange() {}
  ByteRange(int64_t begin, int64_t end) : begin(begin), end(end) {}
  int64_t begin = 0;
  int64_t end = 0;  // exclusive.
  int64_t size() const { return end - begin; }
  bool operator==(const ByteRange &o) const {
    return begin == o.begin && end == o.end;
  }
  std::string DebugString() const { return StrCat("[", begin, ", ", end, ")"); }
};

inline std::ostream &operator<<(std::ostream &out, const ByteRange &range) {
  return out << range.DebugString();
}

// Helper for sending HTTP errors based on POSIX error returns.
void HttpSendError(evhttp_request *req, int http_err, const std::string &prefix,
                   int posix_errno);

class FileSlice {
 public:
  virtual ~FileSlice() {}

  virtual int64_t size() const = 0;

  // Add some to all of the given non-empty |range| to |buf|.
  // Returns the number of bytes added, or < 0 on error.
  // On error, |error_message| should be populated. (|error_message| may also be
  // populated if 0 <= return value < range.size(), such as if one of a
  // FileSlices object's failed. However, it's safe to simply retry such
  // partial failures later.)
  virtual int64_t AddRange(ByteRange range, EvBuffer *buf,
                           std::string *error_message) const = 0;
};

class VirtualFile : public FileSlice {
 public:
  virtual ~VirtualFile() {}

  // Return the given property of the file.
  virtual time_t last_modified() const = 0;
  virtual std::string etag() const = 0;
  virtual std::string mime_type() const = 0;
};

class RealFileSlice : public FileSlice {
 public:
  // |dir| must outlive the RealFileSlice.
  void Init(File *dir, re2::StringPiece filename, ByteRange range);

  int64_t size() const final { return range_.size(); }

  int64_t AddRange(ByteRange range, EvBuffer *buf,
                   std::string *error_message) const final;

 private:
  File *dir_;
  std::string filename_;
  ByteRange range_;
};

// A FileSlice of a pre-defined length which calls a function which fills the
// slice on demand. The FillerFileSlice is responsible for subsetting.
class FillerFileSlice : public FileSlice {
 public:
  using FillFunction =
      std::function<bool(std::string *slice, std::string *error_message)>;

  void Init(size_t size, FillFunction fn) {
    fn_ = fn;
    size_ = size;
  }

  int64_t size() const final { return size_; }

  int64_t AddRange(ByteRange range, EvBuffer *buf,
                   std::string *error_message) const final;

 private:
  FillFunction fn_;
  size_t size_;
};

// A FileSlice backed by in-memory data which outlives this object.
class StringPieceSlice : public FileSlice {
 public:
  StringPieceSlice() = default;
  explicit StringPieceSlice(re2::StringPiece piece) : piece_(piece) {}
  void Init(re2::StringPiece piece) { piece_ = piece; }

  int64_t size() const final { return piece_.size(); }
  int64_t AddRange(ByteRange range, EvBuffer *buf,
                   std::string *error_message) const final;

 private:
  re2::StringPiece piece_;
};

// A slice composed of other slices.
class FileSlices : public FileSlice {
 public:
  FileSlices() {}
  FileSlices(const FileSlices &) = delete;
  FileSlices &operator=(const FileSlices &) = delete;

  // |slice| must outlive the FileSlices.
  // |slice->size()| should not change after this call.
  // |flags| should be a bitmask of Flags values below.
  void Append(const FileSlice *slice, int flags = 0) {
    int64_t new_size = size_ + slice->size();
    slices_.emplace_back(ByteRange(size_, new_size), slice, flags);
    size_ = new_size;
  }

  int64_t size() const final { return size_; }
  int64_t AddRange(ByteRange range, EvBuffer *buf,
                   std::string *error_message) const final;

  enum Flags {
    // kLazy, as an argument to Append, instructs the FileSlices to append
    // this slice in AddRange only if it is the first slice in the requested
    // range. Otherwise it returns early, expecting HttpServe to call AddRange
    // again after the earlier ranges have been sent. This is useful if it is
    // expensive to have the given slice pending. In particular, it is useful
    // when serving many file slices on 32-bit machines to avoid exhausting
    // the address space with too many memory mappings.
    kLazy = 1
  };

 private:
  struct SliceInfo {
    SliceInfo(ByteRange range, const FileSlice *slice, int flags)
        : range(range), slice(slice), flags(flags) {}
    ByteRange range;
    const FileSlice *slice = nullptr;
    int flags;
  };
  int64_t size_ = 0;

  std::vector<SliceInfo> slices_;
};

// Serve an HTTP request |req| from |file|, handling byte range and
// conditional serving. (Similar to golang's http.ServeContent.)
//
// |file| will be retained as long as the request is being served.
void HttpServe(const std::shared_ptr<VirtualFile> &file, evhttp_request *req);

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
