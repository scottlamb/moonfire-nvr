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
#include <sys/stat.h>
#include <sys/types.h>

#include <algorithm>

#include <event2/buffer.h>
#include <event2/event.h>
#include <event2/keyvalq_struct.h>
#include <event2/http.h>
#include <glog/logging.h>

#include "string.h"

namespace moonfire_nvr {

namespace {

// An HttpServe call still in progress.
struct ServeInProgress {
  ByteRange left;
  int64_t sent_bytes = 0;
  std::shared_ptr<VirtualFile> file;
  evhttp_request *req = nullptr;
};

void ServeCloseCallback(evhttp_connection *con, void *arg) {
  std::unique_ptr<ServeInProgress> serve(
      reinterpret_cast<ServeInProgress *>(arg));
  LOG(INFO) << serve->req << ": received client abort after sending "
            << serve->sent_bytes << " bytes; there were " << serve->left.size()
            << " bytes left.";

  // The call to cancel will guarantee ServeChunkCallback is not called again.
  evhttp_cancel_request(serve->req);
}

void ServeChunkCallback(evhttp_connection *con, void *arg) {
  std::unique_ptr<ServeInProgress> serve(
      reinterpret_cast<ServeInProgress *>(arg));

  if (serve->left.size() == 0) {
    LOG(INFO) << serve->req << ": done; sent " << serve->sent_bytes
              << " bytes.";
    evhttp_connection_set_closecb(con, nullptr, nullptr);
    evhttp_send_reply_end(serve->req);
    return;
  }

  // Serve more data.
  EvBuffer buf;
  std::string error_message;
  int64_t added = serve->file->AddRange(serve->left, &buf, &error_message);
  if (added <= 0) {
    // Order is important here: evhttp_cancel_request immediately calls the
    // close callback, so remove it first to avoid double-freeing |serve|.
    evhttp_connection_set_closecb(con, nullptr, nullptr);
    evhttp_cancel_request(serve->req);
    LOG(ERROR) << serve->req << ": Failed to serve request after sending "
               << serve->sent_bytes << " bytes (" << serve->left.size()
               << " bytes left): " << error_message;
    return;
  }

  serve->sent_bytes += added;
  serve->left.begin += added;
  VLOG(1) << serve->req << ": sending " << added << " bytes (more) data; still "
          << serve->left.size() << " bytes left";
  evhttp_send_reply_chunk_with_cb(serve->req, buf.get(), &ServeChunkCallback,
                                  serve.get());
  evhttp_send_reply_chunk(serve->req, buf.get());
  serve.release();
}

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

void RealFileSlice::Init(File *dir, re2::StringPiece filename,
                         ByteRange range) {
  dir_ = dir;
  filename_ = filename.as_string();
  range_ = range;
}

int64_t RealFileSlice::AddRange(ByteRange range, EvBuffer *buf,
                                std::string *error_message) const {
  int fd;
  int ret = dir_->Open(filename_.c_str(), O_RDONLY, &fd);
  if (ret != 0) {
    *error_message = StrCat("open ", filename_, ": ", strerror(ret));
    return -1;
  }
  if (!buf->AddFile(fd, range_.begin + range.begin, range.size(),
                    error_message)) {
    close(fd);
    return -1;
  }
  // |buf| now owns |fd|.
  return range.size();
}

int64_t FillerFileSlice::AddRange(ByteRange range, EvBuffer *buf,
                                  std::string *error_message) const {
  std::unique_ptr<std::string> s(new std::string);
  s->reserve(size_);
  if (!fn_(s.get(), error_message)) {
    return 0;
  }
  if (s->size() != size_) {
    *error_message = StrCat("Expected filled slice to be ", size_,
                            " bytes; got ", s->size(), " bytes.");
    return 0;
  }
  std::string *unowned_s = s.release();
  buf->AddReference(unowned_s->data() + range.begin,
                    range.size(), [](const void *, size_t, void *s) {
                      delete reinterpret_cast<std::string *>(s);
                    }, unowned_s);
  return range.size();
}

int64_t StringPieceSlice::AddRange(ByteRange range, EvBuffer *buf,
                                   std::string *error_message) const {
  buf->AddReference(piece_.data() + range.begin, range.size(), nullptr,
                    nullptr);
  return range.size();
}

int64_t FileSlices::AddRange(ByteRange range, EvBuffer *buf,
                             std::string *error_message) const {
  if (range.begin < 0 || range.begin > range.end || range.end > size_) {
    *error_message = StrCat("Range ", range.DebugString(),
                            " not valid for file of size ", size_);
    return false;
  }
  int64_t total_bytes_added = 0;
  auto it = std::upper_bound(slices_.begin(), slices_.end(), range.begin,
                             [](int64_t begin, const SliceInfo &info) {
                               return begin < info.range.end;
                             });
  for (; it != slices_.end() && range.end > it->range.begin; ++it) {
    if (total_bytes_added > 0 && (it->flags & kLazy) != 0) {
      VLOG(1) << "early return of " << total_bytes_added << "/" << range.size()
              << " bytes from FileSlices " << this << " because slice "
              << it->slice << " is lazy.";
      break;
    }
    ByteRange mapped(
        std::max(INT64_C(0), range.begin - it->range.begin),
        std::min(range.end - it->range.begin, it->range.end - it->range.begin));
    int64_t slice_bytes_added = it->slice->AddRange(mapped, buf, error_message);
    total_bytes_added += slice_bytes_added > 0 ? slice_bytes_added : 0;
    if (slice_bytes_added < 0 && total_bytes_added == 0) {
      LOG(WARNING) << "early return of " << total_bytes_added << "/"
                   << range.size() << " bytes from FileSlices " << this
                   << " due to slice " << it->slice
                   << " returning error: " << *error_message;
      return -1;
    } else if (slice_bytes_added < mapped.size()) {
      LOG(INFO) << "early return of " << total_bytes_added << "/"
                << range.size() << " bytes from FileSlices " << this
                << " due to slice " << it->slice << " returning "
                << slice_bytes_added << "/" << mapped.size()
                << " bytes. error_message (maybe populated): "
                << *error_message;
      break;
    }
  }
  return total_bytes_added;
}

void HttpSendError(evhttp_request *req, int http_err, const std::string &prefix,
                   int posix_err) {
  evhttp_send_error(req, http_err,
                    EscapeHtml(prefix + strerror(posix_err)).c_str());
}

void HttpServe(const std::shared_ptr<VirtualFile> &file, evhttp_request *req) {
  // We could support HEAD, but there's probably no need.
  if (evhttp_request_get_command(req) != EVHTTP_REQ_GET) {
    return evhttp_send_error(req, HTTP_BADMETHOD, "only GET allowed");
  }

  const struct evkeyvalq *in_hdrs = evhttp_request_get_input_headers(req);
  struct evkeyvalq *out_hdrs = evhttp_request_get_output_headers(req);

  // Construct a Last-Modified: header.
  time_t last_modified = file->last_modified();
  struct tm last_modified_tm;
  if (gmtime_r(&last_modified, &last_modified_tm) == 0) {
    return HttpSendError(req, HTTP_INTERNAL, "gmtime_r failed: ", errno);
  }
  char last_modified_str[50];
  if (strftime(last_modified_str, sizeof(last_modified_str),
               "%a, %d %b %Y %H:%M:%S GMT", &last_modified_tm) == 0) {
    return HttpSendError(req, HTTP_INTERNAL, "strftime failed: ", errno);
  }
  std::string etag = file->etag();

  // Ignore the "Range:" header if "If-Range:" specifies an incorrect etag.
  const char *if_range = evhttp_find_header(in_hdrs, "If-Range");
  const char *range_hdr = evhttp_find_header(in_hdrs, "Range");
  if (if_range != nullptr && etag != if_range) {
    LOG(INFO) << req << ": Ignoring Range: because If-Range: is stale.";
    range_hdr = nullptr;
  }

  EvBuffer buf;
  std::vector<ByteRange> ranges;
  auto range_type =
      internal::ParseRangeHeader(range_hdr, file->size(), &ranges);
  std::string error_message;
  int http_status;
  const char *http_status_str;
  ByteRange left;
  switch (range_type) {
    case internal::RangeHeaderType::kNotSatisfiable: {
      std::string range_hdr = StrCat("bytes */", file->size());
      evhttp_add_header(out_hdrs, "Content-Range", range_hdr.c_str());
      http_status = 416;
      http_status_str = "Range Not Satisfiable";
      LOG(INFO) << req
                << ": Replying to non-satisfiable range request: " << range_hdr;
      break;
    }

    case internal::RangeHeaderType::kSatisfiable:
      // We only support the simpler single-range case for now.
      // A multi-range request just serves the whole file via the fallthrough.
      if (ranges.size() == 1) {
        std::string range_hdr = StrCat("bytes ", ranges[0].begin, "-",
                                       ranges[0].end - 1, "/", file->size());
        left = ranges[0];
        evhttp_add_header(out_hdrs, "Content-Range", range_hdr.c_str());
        http_status = 206;
        http_status_str = "Partial Content";
        LOG(INFO) << req << ": URI " << evhttp_request_get_uri(req)
                  << ": client requested byte range " << left
                  << " (total file size " << file->size() << ")";
        break;
      }
    // FALLTHROUGH

    case internal::RangeHeaderType::kAbsentOrInvalid: {
      left = ByteRange(0, file->size());
      LOG(INFO) << req << ": URI " << evhttp_request_get_uri(req)
                << ": Client requested whole file of size " << file->size();
      http_status = HTTP_OK;
      http_status_str = "OK";
    }
  }

  // Successful reply started; add common headers and send.
  evhttp_add_header(out_hdrs, "Content-Length", StrCat(left.size()).c_str());
  evhttp_add_header(out_hdrs, "Content-Type", file->mime_type().c_str());
  evhttp_add_header(out_hdrs, "Accept-Ranges", "bytes");
  evhttp_add_header(out_hdrs, "Last-Modified", last_modified_str);
  evhttp_add_header(out_hdrs, "ETag", etag.c_str());
  evhttp_send_reply_start(req, http_status, http_status_str);

  ServeInProgress *serve = new ServeInProgress;
  serve->file = file;
  serve->left = left;
  serve->req = req;
  evhttp_connection *con = evhttp_request_get_connection(req);
  evhttp_connection_set_closecb(con, &ServeCloseCallback, serve);
  return ServeChunkCallback(con, serve);
}

}  // namespace moonfire_nvr
