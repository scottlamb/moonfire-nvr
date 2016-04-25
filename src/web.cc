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
#include <json/value.h>
#include <json/writer.h>
#include <re2/re2.h>

#include "recording.h"
#include "string.h"

namespace moonfire_nvr {

namespace {

static const char kJsonMimeType[] = "application/json";

void ReplyWithJson(evhttp_request *req, const Json::Value &value) {
  EvBuffer buf;
  buf.Add(Json::writeString(Json::StreamWriterBuilder(), value));
  evhttp_add_header(evhttp_request_get_output_headers(req), "Content-Type",
                    kJsonMimeType);
  evhttp_send_reply(req, HTTP_OK, "OK", buf.get());
}

// RE2::Arg::Parser for uuids.
bool ParseUuid(const char *str, int n, void *dest) {
  auto *uuid = reinterpret_cast<Uuid *>(dest);
  return uuid->ParseText(re2::StringPiece(str, n));
}

}  // namespace

void WebInterface::Register(evhttp *http) {
  evhttp_set_gencb(http, &WebInterface::DispatchHttpRequest, this);
}

void WebInterface::DispatchHttpRequest(evhttp_request *req, void *arg) {
  static const RE2 kCameraUri("/cameras/([^/]+)/");
  static const RE2 kCameraRecordingsUri("/cameras/([^/]+)/recordings");
  static const RE2 kCameraViewUri("/cameras/([^/]+)/view.mp4");

  re2::StringPiece accept =
      evhttp_find_header(evhttp_request_get_input_headers(req), "Accept");
  bool json = accept == kJsonMimeType;

  auto *this_ = reinterpret_cast<WebInterface *>(arg);
  const evhttp_uri *uri = evhttp_request_get_evhttp_uri(req);
  re2::StringPiece path = evhttp_uri_get_path(uri);
  Uuid camera_uuid;
  RE2::Arg camera_uuid_arg(&camera_uuid, &ParseUuid);
  if (path == "/" || path == "/cameras/") {
    if (json) {
      this_->HandleJsonCameraList(req);
    } else {
      this_->HandleHtmlCameraList(req);
    }
  } else if (RE2::FullMatch(path, kCameraUri, camera_uuid_arg)) {
    if (json) {
      this_->HandleJsonCameraDetail(req, camera_uuid);
    } else {
      this_->HandleHtmlCameraDetail(req, camera_uuid);
    }
  } else if (RE2::FullMatch(path, kCameraRecordingsUri, camera_uuid_arg)) {
    // The HTML version includes this in the top-level camera view.
    // So only support JSON at this URI.
    this_->HandleJsonCameraRecordings(req, camera_uuid);
  } else if (RE2::FullMatch(path, kCameraViewUri, camera_uuid_arg)) {
    this_->HandleMp4View(req, camera_uuid);
  } else {
    evhttp_send_error(req, HTTP_NOTFOUND, "path not understood");
  }
}

void WebInterface::HandleHtmlCameraList(evhttp_request *req) {
  EvBuffer buf;
  buf.Add(
      "<!DOCTYPE html>\n"
      "<html>\n"
      "<head>\n"
      "<title>Camera list</title>\n"
      "<meta http-equiv=\"Content-Language\" content=\"en\">\n"
      "<style type=\"text/css\">\n"
      ".header { background-color: #ddd; }\n"
      "td { padding-right: 3em; }\n"
      "</style>\n"
      "</head>\n"
      "<body>\n"
      "<table>\n");
  auto row_cb = [&](const ListCamerasRow &row) {
    auto seconds = row.total_duration_90k / kTimeUnitsPerSecond;
    std::string min_start_time_90k =
        row.min_start_time_90k == -1 ? std::string("n/a")
                                     : PrettyTimestamp(row.min_start_time_90k);
    std::string max_end_time_90k = row.max_end_time_90k == -1
                                       ? std::string("n/a")
                                       : PrettyTimestamp(row.max_end_time_90k);
    buf.AddPrintf(
        "<tr class=header><td colspan=2><a href=\"/cameras/%s/\">%s</a>"
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
  env_->mdb->ListCameras(row_cb);
  buf.Add(
      "</table>\n"
      "</body>\n"
      "<html>\n");
  evhttp_send_reply(req, HTTP_OK, "OK", buf.get());
}

void WebInterface::HandleJsonCameraList(evhttp_request *req) {
  Json::Value cameras(Json::arrayValue);
  auto row_cb = [&](const ListCamerasRow &row) {
    Json::Value camera(Json::objectValue);
    camera["uuid"] = row.uuid.UnparseText();
    camera["short_name"] = row.short_name;
    camera["description"] = row.description;
    camera["retain_bytes"] = static_cast<Json::Int64>(row.retain_bytes);
    camera["total_duration_90k"] =
        static_cast<Json::Int64>(row.total_duration_90k);
    camera["total_sample_file_bytes"] =
        static_cast<Json::Int64>(row.total_sample_file_bytes);
    if (row.min_start_time_90k != -1) {
      camera["min_start_time_90k"] =
          static_cast<Json::Int64>(row.min_start_time_90k);
    }
    if (row.max_end_time_90k != -1) {
      camera["max_end_time_90k"] =
          static_cast<Json::Int64>(row.max_end_time_90k);
    }
    cameras.append(camera);
    return IterationControl::kContinue;
  };
  env_->mdb->ListCameras(row_cb);
  ReplyWithJson(req, cameras);
}

void WebInterface::HandleHtmlCameraDetail(evhttp_request *req,
                                          Uuid camera_uuid) {
  GetCameraRow camera_row;
  if (!env_->mdb->GetCamera(camera_uuid, &camera_row)) {
    return evhttp_send_error(req, HTTP_NOTFOUND, "no such camera");
  }

  EvBuffer buf;
  buf.AddPrintf(
      "<!DOCTYPE html>\n"
      "<html>\n"
      "<head>\n"
      "<title>%s recordings</title>\n"
      "<meta http-equiv=\"Content-Language\" content=\"en\">\n"
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
        "<tr><td><a href=\"view.mp4?start_time_90k=%" PRId64
        "&end_time_90k=%" PRId64
        "\">%s</a></td><td>%s</td><td>%dx%d</td>"
        "<td>%.0f</td><td>%s</td><td>%s</td></tr>\n",
        aggregated.start_time_90k, aggregated.end_time_90k,
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
  if (!env_->mdb->ListCameraRecordings(camera_uuid, start_time_90k,
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

void WebInterface::HandleJsonCameraDetail(evhttp_request *req,
                                          Uuid camera_uuid) {
  GetCameraRow camera_row;
  if (!env_->mdb->GetCamera(camera_uuid, &camera_row)) {
    return evhttp_send_error(req, HTTP_NOTFOUND, "no such camera");
  }

  Json::Value camera(Json::objectValue);
  camera["short_name"] = camera_row.short_name;
  camera["description"] = camera_row.description;
  camera["retain_bytes"] = static_cast<Json::Int64>(camera_row.retain_bytes);
  camera["total_duration_90k"] =
      static_cast<Json::Int64>(camera_row.total_duration_90k);
  camera["total_sample_file_bytes"] =
      static_cast<Json::Int64>(camera_row.total_sample_file_bytes);
  if (camera_row.min_start_time_90k != -1) {
    camera["min_start_time_90k"] =
        static_cast<Json::Int64>(camera_row.min_start_time_90k);
  }
  if (camera_row.max_end_time_90k != -1) {
    camera["max_end_time_90k"] =
        static_cast<Json::Int64>(camera_row.max_end_time_90k);
  }

  // TODO(slamb): include list of calendar days with data.
  ReplyWithJson(req, camera);
}

void WebInterface::HandleJsonCameraRecordings(evhttp_request *req,
                                              Uuid camera_uuid) {
  GetCameraRow camera_row;
  if (!env_->mdb->GetCamera(camera_uuid, &camera_row)) {
    return evhttp_send_error(req, HTTP_NOTFOUND, "no such camera");
  }

  // TODO(slamb): paging support.

  Json::Value recordings(Json::arrayValue);
  auto handle_row = [&](const ListCameraRecordingsRow &row) {
    Json::Value recording(Json::objectValue);
    recording["end_time_90k"] = static_cast<Json::Int64>(row.end_time_90k);
    recording["start_time_90k"] = static_cast<Json::Int64>(row.start_time_90k);
    recording["video_samples"] = static_cast<Json::Int64>(row.video_samples);
    recording["sample_file_bytes"] =
        static_cast<Json::Int64>(row.sample_file_bytes);
    recording["video_sample_entry_sha1"] = ToHex(row.video_sample_entry_sha1);
    recording["video_sample_entry_width"] = row.width;
    recording["video_sample_entry_height"] = row.height;
    recordings.append(recording);
    return IterationControl::kContinue;
  };
  int64_t start_time_90k = 0;
  int64_t end_time_90k = std::numeric_limits<int64_t>::max();
  std::string error_message;
  if (!env_->mdb->ListCameraRecordings(camera_uuid, start_time_90k,
                                       end_time_90k, handle_row,
                                       &error_message)) {
    return evhttp_send_error(
        req, HTTP_INTERNAL,
        StrCat("sqlite query failed: ", EscapeHtml(error_message)).c_str());
  }

  Json::Value response(Json::objectValue);
  response["recordings"] = recordings;
  ReplyWithJson(req, response);
}

void WebInterface::HandleMp4View(evhttp_request *req, Uuid camera_uuid) {
  int64_t start_time_90k;
  int64_t end_time_90k;
  QueryParameters params(evhttp_request_get_uri(req));
  if (!params.ok() ||
      !Atoi64(params.Get("start_time_90k"), 10, &start_time_90k) ||
      !Atoi64(params.Get("end_time_90k"), 10, &end_time_90k) ||
      start_time_90k < 0 || start_time_90k >= end_time_90k) {
    return evhttp_send_error(req, HTTP_BADREQUEST, "bad query parameters");
  }
  bool include_ts = re2::StringPiece(params.Get("ts")) == "true";

  std::string error_message;
  auto file = BuildMp4(camera_uuid, start_time_90k, end_time_90k, include_ts,
                       &error_message);
  if (file == nullptr) {
    // TODO: more nuanced HTTP status codes.
    LOG(WARNING) << "BuildMp4 failed: " << error_message;
    return evhttp_send_error(req, HTTP_INTERNAL,
                             EscapeHtml(error_message).c_str());
  }

  return HttpServe(file, req);
}

std::shared_ptr<VirtualFile> WebInterface::BuildMp4(
    Uuid camera_uuid, int64_t start_time_90k, int64_t end_time_90k,
    bool include_ts, std::string *error_message) {
  LOG(INFO) << "Building mp4 for camera: " << camera_uuid.UnparseText()
            << ", start_time_90k: " << start_time_90k
            << ", end_time_90k: " << end_time_90k;

  Mp4FileBuilder builder(env_->sample_file_dir);
  int64_t next_row_start_time_90k = start_time_90k;
  int64_t rows = 0;
  bool ok = true;
  auto row_cb = [&](Recording &recording,
                    const VideoSampleEntry &sample_entry) {
    if (rows == 0 && recording.start_time_90k != next_row_start_time_90k) {
      *error_message = StrCat(
          "recording starts late: ", PrettyTimestamp(recording.start_time_90k),
          " (", recording.start_time_90k, ") rather than requested: ",
          PrettyTimestamp(start_time_90k), " (", start_time_90k, ")");
      ok = false;
      return IterationControl::kBreak;
    } else if (recording.start_time_90k != next_row_start_time_90k) {
      *error_message = StrCat("gap/overlap in recording: ",
                              PrettyTimestamp(next_row_start_time_90k), " (",
                              next_row_start_time_90k, ") to: ",
                              PrettyTimestamp(recording.start_time_90k), " (",
                              recording.start_time_90k, ") before row ", rows);
      ok = false;
      return IterationControl::kBreak;
    }

    next_row_start_time_90k = recording.end_time_90k;

    if (rows > 0 && recording.video_sample_entry_id != sample_entry.id) {
      *error_message =
          StrCat("inconsistent video sample entries: this recording has id ",
                 recording.video_sample_entry_id, " previous had ",
                 sample_entry.id, " (sha1 ", ToHex(sample_entry.sha1), ")");
      ok = false;
      return IterationControl::kBreak;
    } else if (rows == 0) {
      builder.SetSampleEntry(sample_entry);
    }

    // TODO: correct bounds within recording.
    // Currently this can return too much data.
    builder.Append(std::move(recording), 0,
                   std::numeric_limits<int32_t>::max());
    ++rows;
    return IterationControl::kContinue;
  };
  if (!env_->mdb->ListMp4Recordings(camera_uuid, start_time_90k, end_time_90k,
                                    row_cb, error_message) ||
      !ok) {
    return false;
  }
  if (rows == 0) {
    *error_message = StrCat("no recordings in range");
    return false;
  }
  if (next_row_start_time_90k != end_time_90k) {
    *error_message = StrCat("recording ends early: ",
                            PrettyTimestamp(next_row_start_time_90k), " (",
                            next_row_start_time_90k, "), not requested: ",
                            PrettyTimestamp(end_time_90k), " (", end_time_90k,
                            ") after ", rows, " rows");
    return false;
  }

  builder.include_timestamp_subtitle_track(include_ts);

  VLOG(1) << "...(3/4) building VirtualFile from " << rows << " recordings.";
  auto file = builder.Build(error_message);
  if (file == nullptr) {
    return false;
  }

  VLOG(1) << "...(4/4) success, " << file->size() << " bytes, etag "
          << file->etag();
  return file;
}

}  // namespace moonfire_nvr
