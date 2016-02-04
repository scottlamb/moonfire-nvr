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
// web.h: web (HTTP/HTML) interface to the SQLite-based recording schema.
// Currently, during the transition from the old bunch-of-.mp4-files schema to
// the SQLite-based schema, it's convenient for this to be a separate class
// that interacts with the recording system only through the SQLite database
// and filesystem. In fact, the only advantage of being in-process is that it
// shares the same database mutex and avoids hitting SQLITE_BUSY.
//
// In the future, the interface will be reworked for tighter integration to
// support more features:
//
// * including the recording currently being written in the web interface
// * subscribing to changes
// * reconfiguring the recording system, such as
//   adding/removing/starting/stopping/editing cameras
// * showing thumbnails of the latest key frame from each camera
// * ...

#ifndef MOONFIRE_NVR_WEB_H
#define MOONFIRE_NVR_WEB_H

#include <string>

#include <event2/http.h>

#include "moonfire-db.h"
#include "moonfire-nvr.h"
#include "http.h"

namespace moonfire_nvr {

class WebInterface {
 public:
  explicit WebInterface(Environment *env) : env_(env) {}
  WebInterface(const WebInterface &) = delete;
  void operator=(const WebInterface &) = delete;

  void Register(evhttp *http);

 private:
  static void HandleCameraList(evhttp_request *req, void *arg);
  static void HandleCameraDetail(evhttp_request *req, void *arg);
  static void HandleMp4View(evhttp_request *req, void *arg);

  // TODO: more nuanced error code for HTTP.
  std::shared_ptr<VirtualFile> BuildMp4(Uuid camera_uuid,
                                        int64_t start_time_90k,
                                        int64_t end_time_90k,
                                        std::string *error_message);

  Environment *const env_;
};

}  // namespace moonfire_nvr

#endif  // MOONFIRE_NVR_WEB_H
