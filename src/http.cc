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
// http.cc: See http.h.

#include "http.h"

#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <sys/queue.h>
#include <sys/stat.h>
#include <sys/types.h>

#include <memory>

#include <event2/buffer.h>
#include <event2/event.h>
#include <event2/keyvalq_struct.h>
#include <event2/http.h>
#include <glog/logging.h>

#include "string.h"

namespace moonfire_nvr {

namespace {

class RealFile : public VirtualFile {
 public:
  RealFile(re2::StringPiece mime_type, re2::StringPiece filename,
           const struct stat &statbuf)
      : mime_type_(mime_type.as_string()), stat_(statbuf) {
    slice_.Init(filename, ByteRange(0, statbuf.st_size));
  }

  ~RealFile() final {}

  int64_t size() const final { return stat_.st_size; }
  time_t last_modified() const final { return stat_.st_mtime; }
  std::string mime_type() const final { return mime_type_; }

  std::string etag() const final {
    return StrCat("\"", stat_.st_ino, ":", stat_.st_size, ":",
                  stat_.st_mtim.tv_sec, ":", stat_.st_mtim.tv_nsec, "\"");
  }

  // Add the given range of the file to the buffer.
  bool AddRange(ByteRange range, EvBuffer *buf,
                std::string *error_message) const final {
    return slice_.AddRange(range, buf, error_message);
  }

 private:
  RealFileSlice slice_;
  const std::string mime_type_;
  const struct stat stat_;
};

}  // namespace

namespace internal {

RangeHeaderType ParseRangeHeader(const char *inptr, int64_t size,
                                 std::vector<ByteRange> *ranges) {
  if (inptr == nullptr) {
    return RangeHeaderType::kAbsentOrInvalid;  // absent.
  }
  if (strncmp(inptr, "bytes=", strlen("bytes=")) != 0) {
    return RangeHeaderType::kAbsentOrInvalid;  // invalid syntax.
  }
  inptr += strlen("bytes=");
  ranges->clear();
  int n_ranges = 0;
  while (*inptr != 0) {  // have more byte-range-sets.
    ++n_ranges;
    ByteRange r;

    // Parse a number.
    const char *endptr;
    int64_t value;
    if (!strto64(inptr, 10, &endptr, &value)) {
      return RangeHeaderType::kAbsentOrInvalid;  // invalid syntax.
    }

    if (value < 0) {  // just parsed suffix-byte-range-spec.
      r.begin = std::max(size + value, INT64_C(0));
      r.end = size;
      if (r.begin < r.end) {  // satisfiable.
        ranges->emplace_back(std::move(r));
      }
      inptr = endptr;

    } else {  // just parsed start of byte-range-spec.
      if (*endptr != '-') {
        return RangeHeaderType::kAbsentOrInvalid;
      }
      r.begin = value;
      inptr = endptr + 1;                  // move past the '-'.
      if (*inptr == ',' || *inptr == 0) {  // no end specified; use EOF.
        r.end = size;
      } else {  // explicit range.
        if (!strto64(inptr, 10, &endptr, &value) || value < r.begin) {
          return RangeHeaderType::kAbsentOrInvalid;  // invalid syntax.
        }
        inptr = endptr;
        r.end = std::min(size, value + 1);  // note inclusive->exclusive.
      }
      if (r.begin < size) {
        ranges->emplace_back(std::move(r));  // satisfiable.
      }
    }

    if (*inptr == ',') {
      inptr++;
      if (*inptr == 0) {
        return RangeHeaderType::kAbsentOrInvalid;  // invalid syntax.
      }
    }
  }

  if (n_ranges == 0) {  // must be at least one range.
    return RangeHeaderType::kAbsentOrInvalid;
  }

  return ranges->empty() ? RangeHeaderType::kNotSatisfiable
                         : RangeHeaderType::kSatisfiable;
}

}  // namespace internal

bool EvBuffer::AddFile(int fd, ev_off_t offset, ev_off_t length,
                       std::string *error_message) {
  if (length == 0) {
    // evbuffer_add_file fails in this trivial case, at least when using mmap.
    // Just return true since there's nothing to be done.
    return true;
  }

  if (evbuffer_get_length(buf_) > 0) {
    // Work around https://github.com/libevent/libevent/issues/306 by using a
    // fresh buffer for evbuffer_add_file.
    EvBuffer fresh_buffer;
    if (!fresh_buffer.AddFile(fd, offset, length, error_message)) {
      return false;
    }

    // Crash if evbuffer_add_buffer fails, because the ownership of |fd| has
    // already been transferred, and it's too confusing to support some
    // failures in which the caller still owns |fd| and some in which it does
    // not.
    CHECK_EQ(0, evbuffer_add_buffer(buf_, fresh_buffer.buf_))
        << "evbuffer_add_buffer failed: " << strerror(errno);
    return true;
  }

  if (evbuffer_add_file(buf_, fd, offset, length) != 0) {
    int err = errno;
    *error_message = StrCat("evbuffer_add_file failed with offset ", offset,
                            ", length ", length, ": ", strerror(err));
    return false;
  }
  return true;
}

void RealFileSlice::Init(re2::StringPiece filename, ByteRange range) {
  filename_ = filename.as_string();
  range_ = range;
}

bool RealFileSlice::AddRange(ByteRange range, EvBuffer *buf,
                             std::string *error_message) const {
  int fd = open(filename_.c_str(), O_RDONLY);
  if (fd < 0) {
    int err = errno;
    *error_message = StrCat("open: ", strerror(err));
    return false;
  }
  if (!buf->AddFile(fd, range_.begin + range.begin, range.size(),
                    error_message)) {
    close(fd);
    return false;
  }
  // |buf| now owns |fd|.
  return true;
}

bool FillerFileSlice::AddRange(ByteRange range, EvBuffer *buf,
                               std::string *error_message) const {
  std::unique_ptr<std::string> s(new std::string);
  s->reserve(size_);
  if (!fn_(s.get(), error_message)) {
    return false;
  }
  if (s->size() != size_) {
    *error_message = StrCat("Expected filled slice to be ", size_,
                            " bytes; got ", s->size(), " bytes.");
    return false;
  }
  std::string *unowned_s = s.release();
  buf->AddReference(unowned_s->data() + range.begin,
                    range.size(), [](const void *, size_t, void *s) {
                      delete reinterpret_cast<std::string *>(s);
                    }, unowned_s);
  return true;
}

bool StaticStringPieceSlice::AddRange(ByteRange range, EvBuffer *buf,
                                      std::string *error_message) const {
  buf->AddReference(piece_.data() + range.begin, range.size(), nullptr,
                    nullptr);
  return true;
}

bool CopyingStringPieceSlice::AddRange(ByteRange range, EvBuffer *buf,
                                       std::string *error_message) const {
  buf->Add(re2::StringPiece(piece_.data() + range.begin, range.size()));
  return true;
}

bool FileSlices::AddRange(ByteRange range, EvBuffer *buf,
                          std::string *error_message) const {
  if (range.begin < 0 || range.begin > range.end || range.end > size_) {
    *error_message = StrCat("Range ", range.DebugString(),
                            " not valid for file of size ", size_);
    return false;
  }
  auto it = std::upper_bound(slices_.begin(), slices_.end(), range.begin,
                             [](int64_t begin, const SliceInfo &info) {
                               return begin < info.range.end;
                             });
  for (; it != slices_.end() && range.end > it->range.begin; ++it) {
    ByteRange mapped(
        std::max(INT64_C(0), range.begin - it->range.begin),
        std::min(range.end - it->range.begin, it->range.end - it->range.begin));
    if (!it->slice->AddRange(mapped, buf, error_message)) {
      return false;
    }
  }
  return true;
}

void HttpSendError(evhttp_request *req, int http_err, const std::string &prefix,
                   int posix_err) {
  evhttp_send_error(req, http_err,
                    EscapeHtml(prefix + strerror(posix_err)).c_str());
}

void HttpServe(const VirtualFile &file, evhttp_request *req) {
  // We could support HEAD, but there's probably no need.
  if (evhttp_request_get_command(req) != EVHTTP_REQ_GET) {
    return evhttp_send_error(req, HTTP_BADMETHOD, "only GET allowed");
  }

  const struct evkeyvalq *in_hdrs = evhttp_request_get_input_headers(req);
  struct evkeyvalq *out_hdrs = evhttp_request_get_output_headers(req);

  // Construct a Last-Modified: header.
  time_t last_modified = file.last_modified();
  struct tm last_modified_tm;
  if (gmtime_r(&last_modified, &last_modified_tm) == 0) {
    return HttpSendError(req, HTTP_INTERNAL, "gmtime_r failed: ", errno);
  }
  char last_modified_str[50];
  if (strftime(last_modified_str, sizeof(last_modified_str),
               "%a, %d %b %Y %H:%M:%S GMT", &last_modified_tm) == 0) {
    return HttpSendError(req, HTTP_INTERNAL, "strftime failed: ", errno);
  }
  std::string etag = file.etag();

  // Ignore the "Range:" header if "If-Range:" specifies an incorrect etag.
  const char *if_range = evhttp_find_header(in_hdrs, "If-Range");
  const char *range_hdr = evhttp_find_header(in_hdrs, "Range");
  if (if_range != nullptr && etag != if_range) {
    LOG(INFO) << "Ignoring Range: because If-Range: is stale.";
    range_hdr = nullptr;
  }

  EvBuffer buf;
  std::vector<ByteRange> ranges;
  auto range_type = internal::ParseRangeHeader(range_hdr, file.size(), &ranges);
  std::string error_message;
  int http_status;
  const char *http_status_str;
  switch (range_type) {
    case internal::RangeHeaderType::kNotSatisfiable: {
      std::string range_hdr = StrCat("bytes */", file.size());
      evhttp_add_header(out_hdrs, "Content-Range", range_hdr.c_str());
      http_status = 416;
      http_status_str = "Range Not Satisfiable";
      LOG(INFO) << "Replying to non-satisfiable range request: " << range_hdr;
      break;
    }

    case internal::RangeHeaderType::kSatisfiable:
      // We only support the simpler single-range case for now.
      if (ranges.size() == 1) {
        std::string range_hdr = StrCat("bytes ", ranges[0].begin, "-",
                                       ranges[0].end - 1, "/", file.size());
        if (!file.AddRange(ranges[0], &buf, &error_message)) {
          LOG(ERROR) << "Unable to serve range " << ranges[0] << ": "
                     << error_message;
          return evhttp_send_error(req, HTTP_INTERNAL,
                                   EscapeHtml(error_message).c_str());
        }
        evhttp_add_header(out_hdrs, "Content-Range", range_hdr.c_str());
        http_status = 206;
        http_status_str = "Partial Content";
        LOG(INFO) << "Replying to range request";
        break;
      }
    // FALLTHROUGH

    case internal::RangeHeaderType::kAbsentOrInvalid:
      if (!file.AddRange(ByteRange(0, file.size()), &buf, &error_message)) {
        LOG(ERROR) << "Unable to serve file: " << error_message;
        return evhttp_send_error(req, HTTP_INTERNAL,
                                 EscapeHtml(error_message).c_str());
      }
      LOG(INFO) << "Replying to whole-file request";
      http_status = HTTP_OK;
      http_status_str = "OK";
  }

  // Successful reply ready; add common headers and send.
  evhttp_add_header(out_hdrs, "Content-Type", file.mime_type().c_str());
  evhttp_add_header(out_hdrs, "Accept-Ranges", "bytes");
  evhttp_add_header(out_hdrs, "Last-Modified", last_modified_str);
  evhttp_add_header(out_hdrs, "ETag", etag.c_str());
  evhttp_send_reply(req, http_status, http_status_str, buf.get());
}

void HttpServeFile(evhttp_request *req, const std::string &mime_type,
                   const std::string &filename, const struct stat &statbuf) {
  return HttpServe(RealFile(mime_type, filename, statbuf), req);
}

}  // namespace moonfire_nvr
