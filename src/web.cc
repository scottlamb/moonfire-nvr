// This file is part of Moonfire NVR, a security camera network video recorder.
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
// web.cc: implementation of web.h interface.

#include "web.h"

#include <glog/logging.h>

#include "recording.h"
#include "string.h"

namespace moonfire_nvr {

void WebInterface::Register(evhttp *http) {
  evhttp_set_cb(http, "/", &WebInterface::HandleCameraList, this);
  evhttp_set_cb(http, "/camera", &WebInterface::HandleCameraDetail, this);
  evhttp_set_cb(http, "/view.mp4", &WebInterface::HandleMp4View, this);
}

void WebInterface::HandleCameraList(evhttp_request *req, void *arg) {
  auto *this_ = reinterpret_cast<WebInterface *>(arg);
  EvBuffer buf;
  buf.Add(
      "<!DOCTYPE html>\n"
      "<html>\n"
      "<head>\n"
      "<title>Camera list</title>\n"
      "<style type=\"text/css\">\n"
      ".header { background-color: #ddd; }\n"
      "td { padding-right: 3em; }\n"
      "</style>\n"
      "</head>\n"
      "<body>\n"
      "<table>\n");
  auto row_cb = [&](const ListCamerasRow &row) {
    auto seconds =
        (row.max_end_time_90k - row.min_start_time_90k) / kTimeUnitsPerSecond;
    std::string min_start_time_90k =
        row.min_start_time_90k == -1 ? std::string("n/a")
                                     : PrettyTimestamp(row.min_start_time_90k);
    std::string max_end_time_90k = row.max_end_time_90k == -1
                                       ? std::string("n/a")
                                       : PrettyTimestamp(row.max_end_time_90k);
    buf.AddPrintf(
        "<tr class=header><td colspan=2><a href=\"/camera?uuid=%s\">%s</a>"
        "</td></tr>\n"
        "<tr><td>description</td><td>%s</td></tr>\n"
        "<tr><td>space</td><td>%s / %s (%.1f%%)</td></tr>\n"
        "<tr><td>uuid</td><td>%s</td></tr>\n"
        "<tr><td>oldest recording</td><td>%s</td></tr>\n"
        "<tr><td>newest recording</td><td>%s</td></tr>\n"
        "<tr><td>total duration</td><td>%s</td></tr>\n",
        row.uuid.UnparseText().c_str(), EscapeHtml(row.short_name).c_str(),
        EscapeHtml(row.description).c_str(),
        EscapeHtml(HumanizeWithBinaryPrefix(row.total_sample_file_bytes, "B"))
            .c_str(),
        EscapeHtml(HumanizeWithBinaryPrefix(row.retain_bytes, "B")).c_str(),
        100.f * row.total_sample_file_bytes / row.retain_bytes,
        EscapeHtml(row.uuid.UnparseText()).c_str(),
        EscapeHtml(min_start_time_90k).c_str(),
        EscapeHtml(max_end_time_90k).c_str(),
        EscapeHtml(HumanizeDuration(seconds)).c_str());
    return IterationControl::kContinue;
  };
  this_->mdb_->ListCameras(row_cb);
  buf.Add(
      "</table>\n"
      "</body>\n"
      "<html>\n");
  evhttp_send_reply(req, HTTP_OK, "OK", buf.get());
}

void WebInterface::HandleCameraDetail(evhttp_request *req, void *arg) {
  auto *this_ = reinterpret_cast<WebInterface *>(arg);

  Uuid camera_uuid;
  QueryParameters params(evhttp_request_get_uri(req));
  if (!params.ok() || !camera_uuid.ParseText(params.Get("uuid"))) {
    return evhttp_send_error(req, HTTP_BADREQUEST, "bad query parameters");
  }

  GetCameraRow camera_row;
  if (!this_->mdb_->GetCamera(camera_uuid, &camera_row)) {
    return evhttp_send_error(req, HTTP_NOTFOUND, "no such camera");
  }

  EvBuffer buf;
  buf.AddPrintf(
      "<!DOCTYPE html>\n"
      "<html>\n"
      "<head>\n"
      "<title>%s recordings</title>\n"
      "<style type=\"text/css\">\n"
      "tr:not(:first-child):hover { background-color: #ddd; }\n"
      "th, td { padding: 0.5ex 1.5em; text-align: right; }\n"
      "</style>\n"
      "</head>\n"
      "<body>\n"
      "<h1>%s</h1>\n"
      "<p>%s</p>\n"
      "<table>\n"
      "<tr><th>start</th><th>end</th><th>resolution</th>"
      "<th>fps</th><th>size</th><th>bitrate</th>"
      "</tr>\n",
      EscapeHtml(camera_row.short_name).c_str(),
      EscapeHtml(camera_row.short_name).c_str(),
      EscapeHtml(camera_row.description).c_str());

  // Rather than listing each 60-second recording, generate a HTML row for
  // aggregated .mp4 files of up to kForceSplitDuration90k each, provided
  // there is no gap or change in video parameters between recordings.
  static const int64_t kForceSplitDuration90k = 60 * 60 * kTimeUnitsPerSecond;
  ListCameraRecordingsRow aggregated;
  auto maybe_finish_html_row = [&]() {
    if (aggregated.start_time_90k == -1) {
      return;  // there is no row to finish.
    }
    auto seconds = static_cast<float>(aggregated.end_time_90k -
                                      aggregated.start_time_90k) /
                   kTimeUnitsPerSecond;
    buf.AddPrintf(
        "<tr><td><a href=\"/view.mp4?camera_uuid=%s&start_time_90k=%" PRId64
        "&end_time_90k=%" PRId64
        "\">%s</a></td><td>%s</td><td>%dx%d</td>"
        "<td>%.0f</td><td>%s</td><td>%s</td></tr>\n",
        camera_uuid.UnparseText().c_str(), aggregated.start_time_90k,
        aggregated.end_time_90k,
        PrettyTimestamp(aggregated.start_time_90k).c_str(),
        PrettyTimestamp(aggregated.end_time_90k).c_str(),
        static_cast<int>(aggregated.width), static_cast<int>(aggregated.height),
        static_cast<float>(aggregated.video_samples) / seconds,
        HumanizeWithBinaryPrefix(aggregated.sample_file_bytes, "B").c_str(),
        HumanizeWithDecimalPrefix(
            static_cast<float>(aggregated.sample_file_bytes) * 8 / seconds,
            "bps")
            .c_str());
  };
  auto handle_sql_row = [&](const ListCameraRecordingsRow &row) {
    auto new_duration_90k = aggregated.end_time_90k - row.start_time_90k;
    if (row.video_sample_entry_sha1 == aggregated.video_sample_entry_sha1 &&
        row.end_time_90k == aggregated.start_time_90k &&
        new_duration_90k < kForceSplitDuration90k) {
      // Append to current .mp4.
      aggregated.start_time_90k = row.start_time_90k;
      aggregated.video_samples += row.video_samples;
      aggregated.sample_file_bytes += row.sample_file_bytes;
    } else {
      // Start a new .mp4.
      maybe_finish_html_row();
      aggregated = row;
    }
    return IterationControl::kContinue;
  };
  int64_t start_time_90k = 0;
  int64_t end_time_90k = std::numeric_limits<int64_t>::max();
  std::string error_message;
  if (!this_->mdb_->ListCameraRecordings(camera_uuid, start_time_90k,
                                         end_time_90k, handle_sql_row,
                                         &error_message)) {
    return evhttp_send_error(
        req, HTTP_INTERNAL,
        StrCat("sqlite query failed: ", EscapeHtml(error_message)).c_str());
  }
  maybe_finish_html_row();
  buf.Add(
      "</table>\n"
      "</html>\n");
  evhttp_send_reply(req, HTTP_OK, "OK", buf.get());
}

void WebInterface::HandleMp4View(evhttp_request *req, void *arg) {
  auto *this_ = reinterpret_cast<WebInterface *>(arg);

  Uuid camera_uuid;
  int64_t start_time_90k;
  int64_t end_time_90k;
  QueryParameters params(evhttp_request_get_uri(req));
  if (!params.ok() || !camera_uuid.ParseText(params.Get("camera_uuid")) ||
      !Atoi64(params.Get("start_time_90k"), 10, &start_time_90k) ||
      !Atoi64(params.Get("end_time_90k"), 10, &end_time_90k) ||
      start_time_90k < 0 || start_time_90k >= end_time_90k) {
    return evhttp_send_error(req, HTTP_BADREQUEST, "bad query parameters");
  }

  std::string error_message;
  auto file = this_->mdb_->BuildMp4(camera_uuid, start_time_90k, end_time_90k,
                                    &error_message);
  if (file == nullptr) {
    // TODO: more nuanced HTTP status codes.
    return evhttp_send_error(req, HTTP_INTERNAL,
                             EscapeHtml(error_message).c_str());
  }

  return HttpServe(file, req);
}

}  // namespace moonfire_nvr
