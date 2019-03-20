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

use base::clock::Clocks;
use base::{ErrorKind, ResultExt, bail_t, strutil};
use crate::body::{Body, BoxedError};
use crate::json;
use crate::mp4;
use base64;
use bytes::{BufMut, BytesMut};
use core::borrow::Borrow;
use core::str::FromStr;
use db::{auth, recording, CameraChange};
use db::dir::SampleFileDir;
use failure::{Error, bail, format_err};
use fnv::FnvHashMap;
use futures::{Future, Stream, future};
use futures_cpupool;
use http::{Request, Response, status::StatusCode};
use http_serve;
use http::header::{self, HeaderValue};
use lazy_static::lazy_static;
use log::{debug, info, warn};
use regex::Regex;
use serde_json;
use std::collections::HashMap;
use std::cmp;
use std::fs;
use std::net::IpAddr;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use url::form_urlencoded;
use uuid::Uuid;

lazy_static! {
    /// Regex used to parse the `s` query parameter to `view.mp4`.
    /// As described in `design/api.md`, this is of the form
    /// `START_ID[-END_ID][@OPEN_ID][.[REL_START_TIME]-[REL_END_TIME]]`.
    static ref SEGMENTS_RE: Regex =
        Regex::new(r"^(\d+)(-\d+)?(@\d+)?(?:\.(\d+)?-(\d+)?)?$").unwrap();
}

#[derive(Debug, Eq, PartialEq)]
enum Path {
    TopLevel,                                         // "/api/"
    Request,                                          // "/api/request"
    InitSegment([u8; 20], bool),                      // "/api/init/<sha1>.mp4{.txt}"
    SaveCamera,                                       // "/api/cameras"
    GetCamera(Uuid),                                  // "/api/cameras/<uuid>/"
    StreamRecordings(Uuid, db::StreamType),           // "/api/cameras/<uuid>/<type>/recordings"
    StreamViewMp4(Uuid, db::StreamType, bool),        // "/api/cameras/<uuid>/<type>/view.mp4{.txt}"
    StreamViewMp4Segment(Uuid, db::StreamType, bool), // "/api/cameras/<uuid>/<type>/view.m4s{.txt}"
    StreamLiveMp4Segments(Uuid, db::StreamType),      // "/api/cameras/<uuid>/<type>/live.m4s"
    SaveSampleFileDir,                                // "/api/dirs"
    Login,                                            // "/api/login"
    Logout,                                           // "/api/logout"
    Static,                                           // (anything that doesn't start with "/api/")
    NotFound,
}

impl Path {
    fn decode(path: &str) -> Self {
        if !path.starts_with("/api/") {
            return Path::Static;
        }
        let path = &path["/api".len()..];
        if path == "/" {
            return Path::TopLevel;
        }
        match path {
            "/request" => return Path::Request,
            "/login" => return Path::Login,
            "/logout" => return Path::Logout,
            "/cameras" => return Path::SaveCamera,
            "/dirs" => return Path::SaveSampleFileDir,
            _ => {},
        };
        if path.starts_with("/init/") {
            let (debug, path) = if path.ends_with(".txt") {
                (true, &path[0 .. path.len() - 4])
            } else {
                (false, path)
            };
            if path.len() != 50 || !path.ends_with(".mp4") {
                return Path::NotFound;
            }
            if let Ok(sha1) = strutil::dehex(&path.as_bytes()[6..46]) {
                return Path::InitSegment(sha1, debug);
            }
            return Path::NotFound;
        }
        if !path.starts_with("/cameras/") {
            return Path::NotFound;
        }
        let path = &path["/cameras/".len()..];
        let slash = match path.find('/') {
            None => { return Path::NotFound; },
            Some(s) => s,
        };
        let uuid = &path[0 .. slash];
        let path = &path[slash+1 .. ];

        // TODO(slamb): require uuid to be in canonical format.
        let uuid = match Uuid::parse_str(uuid) {
            Ok(u) => u,
            Err(_) => { return Path::NotFound },
        };

        if path.is_empty() {
            return Path::GetCamera(uuid);
        }

        let slash = match path.find('/') {
            None => { return Path::NotFound; },
            Some(s) => s,
        };
        let (type_, path) = path.split_at(slash);

        let type_ = match db::StreamType::parse(type_) {
            None => { return Path::NotFound; },
            Some(t) => t,
        };
        match path {
            "/recordings" => Path::StreamRecordings(uuid, type_),
            "/view.mp4" => Path::StreamViewMp4(uuid, type_, false),
            "/view.mp4.txt" => Path::StreamViewMp4(uuid, type_, true),
            "/view.m4s" => Path::StreamViewMp4Segment(uuid, type_, false),
            "/view.m4s.txt" => Path::StreamViewMp4Segment(uuid, type_, true),
            "/live.m4s" => Path::StreamLiveMp4Segments(uuid, type_),
            _ => Path::NotFound,
        }
    }
}

fn plain_response<B: Into<Body>>(status: http::StatusCode, body: B) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))
        .body(body.into()).expect("hardcoded head should be valid")
}

fn not_found<B: Into<Body>>(body: B) -> Response<Body> {
    plain_response(StatusCode::NOT_FOUND, body)
}

fn bad_req<B: Into<Body>>(body: B) -> Response<Body> {
    plain_response(StatusCode::BAD_REQUEST, body)
}

fn internal_server_err<E: Into<Error>>(err: E) -> Response<Body> {
    plain_response(StatusCode::INTERNAL_SERVER_ERROR, err.into().to_string())
}

fn from_base_error(err: base::Error) -> Response<Body> {
    let status_code = match err.kind() {
        ErrorKind::InvalidArgument => StatusCode::BAD_REQUEST,
        ErrorKind::NotFound => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    plain_response(status_code, err.to_string())
}

#[derive(Debug, Eq, PartialEq)]
struct Segments {
    ids: Range<i32>,
    open_id: Option<u32>,
    start_time: i64,
    end_time: Option<i64>,
}

impl Segments {
    pub fn parse(input: &str) -> Result<Segments, ()> {
        let caps = SEGMENTS_RE.captures(input).ok_or(())?;
        let ids_start = i32::from_str(caps.get(1).unwrap().as_str()).map_err(|_| ())?;
        let ids_end = match caps.get(2) {
            Some(m) => i32::from_str(&m.as_str()[1..]).map_err(|_| ())?,
            None => ids_start,
        } + 1;
        let open_id = match caps.get(3) {
            Some(m) => Some(u32::from_str(&m.as_str()[1..]).map_err(|_| ())?),
            None => None,
        };
        if ids_start < 0 || ids_end <= ids_start {
            return Err(());
        }
        let start_time = caps.get(4).map_or(Ok(0), |m| i64::from_str(m.as_str())).map_err(|_| ())?;
        if start_time < 0 {
            return Err(());
        }
        let end_time = match caps.get(5) {
            Some(v) => {
                let e = i64::from_str(v.as_str()).map_err(|_| ())?;
                if e <= start_time {
                    return Err(());
                }
                Some(e)
            },
            None => None
        };
        Ok(Segments {
            ids: ids_start .. ids_end,
            open_id,
            start_time,
            end_time,
        })
    }
}

/// A user interface file (.html, .js, etc).
/// The list of files is loaded into the server at startup; this makes path canonicalization easy.
/// The files themselves are opened on every request so they can be changed during development.
#[derive(Debug)]
struct UiFile {
    mime: HeaderValue,
    path: PathBuf,
}

struct ServiceInner {
    db: Arc<db::Database>,
    dirs_by_stream_id: Arc<FnvHashMap<i32, Arc<SampleFileDir>>>,
    ui_files: HashMap<String, UiFile>,
    pool: futures_cpupool::CpuPool,
    time_zone_name: String,
    require_auth: bool,
    trust_forward_hdrs: bool,
}

type ResponseResult = Result<Response<Body>, Response<Body>>;

impl ServiceInner {
    fn top_level(&self, req: &Request<::hyper::Body>, session: Option<json::Session>)
                 -> ResponseResult {
        let mut days = false;
        if let Some(q) = req.uri().query() {
            for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                let (key, value): (_, &str) = (key.borrow(), value.borrow());
                match key {
                    "days" => days = value == "true",
                    _ => {},
                };
            }
        }

        let (mut resp, writer) = http_serve::streaming_body(&req).build();
        resp.headers_mut().insert(header::CONTENT_TYPE,
                                  HeaderValue::from_static("application/json"));
        if let Some(mut w) = writer {
            let db = self.db.lock();
            serde_json::to_writer(&mut w, &json::TopLevel {
                    time_zone_name: &self.time_zone_name,
                    cameras: (&db, days),
                    session,
            }).map_err(internal_server_err)?;
        }
        Ok(resp)
    }

    fn camera(&self, req: &Request<::hyper::Body>, uuid: Uuid) -> ResponseResult {
        let (mut resp, writer) = http_serve::streaming_body(&req).build();
        resp.headers_mut().insert(header::CONTENT_TYPE,
                                  HeaderValue::from_static("application/json"));
        if let Some(mut w) = writer {
            let db = self.db.lock();
            let camera = db.get_camera(uuid)
                           .ok_or_else(|| not_found(format!("no such camera {}", uuid)))?;
            serde_json::to_writer(
                &mut w,
                &json::Camera::wrap(camera, &db, true).map_err(internal_server_err)?
            ).map_err(internal_server_err)?
        };
        Ok(resp)
    }

    fn save_camera(&self, req: &Request<::hyper::Body>, body: serde_json::Value) -> ResponseResult {
         let (mut resp, writer) = http_serve::streaming_body(&req).build();
        resp.headers_mut().insert(header::CONTENT_TYPE,
                                  HeaderValue::from_static("application/json"));

        let c: CameraChange = serde_json::from_value(body).map_err(|_| bad_req("missing fields"))?;
        if c.streams.len() == 0 {
            *resp.status_mut() = StatusCode::BAD_REQUEST;
            return Ok(resp);
        }
        let mut db = self.db.lock();
        for stream in &c.streams {
            let id = match stream.sample_file_dir_id {
                Some(id) => {
                    id
                },
                None => {
                    continue;
                }
            };
            let dir = db.sample_file_dirs_by_id().get(&id);
            match dir {
                None => {
                    *resp.status_mut() = StatusCode::BAD_REQUEST;
                    return Ok(resp);
                },
                Some(_) => {
                    // dir exists ignore
                }
            }
        }
        db.add_camera(c).map(|camera_id| {
            *resp.status_mut() = StatusCode::OK;
            if let Some(mut w) = writer {
                serde_json::to_writer(
                    &mut w,
                    &serde_json::json!({ "camera_id": camera_id })
                ).map_err(internal_server_err)?
            }
            Ok(resp)
        }).map_err(internal_server_err)?
    }

    fn stream_recordings(&self, req: &Request<::hyper::Body>, uuid: Uuid, type_: db::StreamType)
                         -> ResponseResult {
        let (r, split) = {
            let mut time = recording::Time(i64::min_value()) .. recording::Time(i64::max_value());
            let mut split = recording::Duration(i64::max_value());
            if let Some(q) = req.uri().query() {
                for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                    let (key, value) = (key.borrow(), value.borrow());
                    match key {
                        "startTime90k" => {
                            time.start = recording::Time::parse(value)
                                .map_err(|_| bad_req("unparseable startTime90k"))?
                        },
                        "endTime90k" => {
                            time.end = recording::Time::parse(value)
                                .map_err(|_| bad_req("unparseable endTime90k"))?
                        },
                        "split90k" => {
                            split = recording::Duration(i64::from_str(value)
                                .map_err(|_| bad_req("unparseable split90k"))?)
                        },
                        _ => {},
                    }
                };
            }
            (time, split)
        };
        let mut out = json::ListRecordings{recordings: Vec::new()};
        {
            let db = self.db.lock();
            let camera = db.get_camera(uuid)
                           .ok_or_else(|| plain_response(StatusCode::NOT_FOUND,
                                                         format!("no such camera {}", uuid)))?;
            let stream_id = camera.streams[type_.index()]
                .ok_or_else(|| plain_response(StatusCode::NOT_FOUND,
                                              format!("no such stream {}/{}", uuid, type_)))?;
            db.list_aggregated_recordings(stream_id, r, split, &mut |row| {
                let end = row.ids.end - 1;  // in api, ids are inclusive.
                let vse = db.video_sample_entries_by_id().get(&row.video_sample_entry_id).unwrap();
                out.recordings.push(json::Recording {
                    start_id: row.ids.start,
                    end_id: if end == row.ids.start { None } else { Some(end) },
                    start_time_90k: row.time.start.0,
                    end_time_90k: row.time.end.0,
                    sample_file_bytes: row.sample_file_bytes,
                    open_id: row.open_id,
                    first_uncommitted: row.first_uncommitted,
                    video_samples: row.video_samples,
                    video_sample_entry_width: vse.width,
                    video_sample_entry_height: vse.height,
                    video_sample_entry_sha1: strutil::hex(&vse.sha1),
                    growing: row.growing,
                });
                Ok(())
            }).map_err(internal_server_err)?;
        }
        let (mut resp, writer) = http_serve::streaming_body(&req).build();
        resp.headers_mut().insert(header::CONTENT_TYPE,
                                  HeaderValue::from_static("application/json"));
        if let Some(mut w) = writer {
            serde_json::to_writer(&mut w, &out).map_err(internal_server_err)?
        };
        Ok(resp)
    }

    fn init_segment(&self, sha1: [u8; 20], debug: bool, req: &Request<::hyper::Body>)
                    -> ResponseResult {
        let mut builder = mp4::FileBuilder::new(mp4::Type::InitSegment);
        let db = self.db.lock();
        for ent in db.video_sample_entries_by_id().values() {
            if ent.sha1 == sha1 {
                builder.append_video_sample_entry(ent.clone());
                let mp4 = builder.build(self.db.clone(), self.dirs_by_stream_id.clone())
                    .map_err(from_base_error)?;
                if debug {
                    return Ok(plain_response(StatusCode::OK, format!("{:#?}", mp4)));
                } else {
                    return Ok(http_serve::serve(mp4, req));
                }
            }
        }
        Err(not_found("no such init segment"))
    }

    fn stream_view_mp4(&self, req: &Request<::hyper::Body>, uuid: Uuid,
                       stream_type: db::StreamType, mp4_type: mp4::Type, debug: bool)
                       -> ResponseResult {
        let stream_id = {
            let db = self.db.lock();
            let camera = db.get_camera(uuid)
                           .ok_or_else(|| plain_response(StatusCode::NOT_FOUND,
                                                         format!("no such camera {}", uuid)))?;
            camera.streams[stream_type.index()]
                .ok_or_else(|| plain_response(StatusCode::NOT_FOUND,
                                              format!("no such stream {}/{}", uuid,
                                                      stream_type)))?
        };
        let mut builder = mp4::FileBuilder::new(mp4_type);
        if let Some(q) = req.uri().query() {
            for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                let (key, value) = (key.borrow(), value.borrow());
                match key {
                    "s" => {
                        let s = Segments::parse(value).map_err(
                            |()| plain_response(StatusCode::BAD_REQUEST,
                                                format!("invalid s parameter: {}", value)))?;
                        debug!("stream_view_mp4: appending s={:?}", s);
                        let mut est_segments = (s.ids.end - s.ids.start) as usize;
                        if let Some(end) = s.end_time {
                            // There should be roughly ceil((end - start) /
                            // desired_recording_duration) recordings in the desired timespan if
                            // there are no gaps or overlap, possibly another for misalignment of
                            // the requested timespan with the rotate offset and another because
                            // rotation only happens at key frames.
                            let ceil_durations = (end - s.start_time +
                                                  recording::DESIRED_RECORDING_DURATION - 1) /
                                                 recording::DESIRED_RECORDING_DURATION;
                            est_segments = cmp::min(est_segments, (ceil_durations + 2) as usize);
                        }
                        builder.reserve(est_segments);
                        let db = self.db.lock();
                        let mut prev = None;
                        let mut cur_off = 0;
                        db.list_recordings_by_id(stream_id, s.ids.clone(), &mut |r| {
                            let recording_id = r.id.recording();

                            if let Some(o) = s.open_id {
                                if r.open_id != o {
                                    bail!("recording {} has open id {}, requested {}",
                                          r.id, r.open_id, o);
                                }
                            }

                            // Check for missing recordings.
                            match prev {
                                None if recording_id == s.ids.start => {},
                                None => bail!("no such recording {}/{}", stream_id, s.ids.start),
                                Some(id) if r.id.recording() != id + 1 => {
                                    bail!("no such recording {}/{}", stream_id, id + 1);
                                },
                                _ => {},
                            };
                            prev = Some(recording_id);

                            // Add a segment for the relevant part of the recording, if any.
                            let end_time = s.end_time.unwrap_or(i64::max_value());
                            let d = r.duration_90k as i64;
                            if s.start_time <= cur_off + d && cur_off < end_time {
                                let start = cmp::max(0, s.start_time - cur_off);
                                let end = cmp::min(d, end_time - cur_off);
                                let times = start as i32 .. end as i32;
                                debug!("...appending recording {} with times {:?} \
                                       (out of dur {})", r.id, times, d);
                                builder.append(&db, r, start as i32 .. end as i32)?;
                            } else {
                                debug!("...skipping recording {} dur {}", r.id, d);
                            }
                            cur_off += d;
                            Ok(())
                        }).map_err(internal_server_err)?;

                        // Check for missing recordings.
                        match prev {
                            Some(id) if s.ids.end != id + 1 => {
                                return Err(not_found(format!("no such recording {}/{}",
                                                             stream_id, s.ids.end - 1)));
                            },
                            None => {
                                return Err(not_found(format!("no such recording {}/{}",
                                                             stream_id, s.ids.start)));
                            },
                            _ => {},
                        };
                        if let Some(end) = s.end_time {
                            if end > cur_off {
                                return Err(plain_response(
                                        StatusCode::BAD_REQUEST,
                                        format!("end time {} is beyond specified recordings",
                                                end)));
                            }
                        }
                    },
                    "ts" => builder.include_timestamp_subtitle_track(value == "true"),
                    _ => return Err(bad_req(format!("parameter {} not understood", key))),
                }
            };
        }
        let mp4 = builder.build(self.db.clone(), self.dirs_by_stream_id.clone())
                         .map_err(from_base_error)?;
        if debug {
            return Ok(plain_response(StatusCode::OK, format!("{:#?}", mp4)));
        }
        Ok(http_serve::serve(mp4, req))
    }

    fn static_file(&self, req: &Request<::hyper::Body>, path: &str) -> ResponseResult {
        let s = self.ui_files.get(path).ok_or_else(|| not_found("no such static file"))?;
        let f = fs::File::open(&s.path).map_err(internal_server_err)?;
        let mut hdrs = http::HeaderMap::new();
        hdrs.insert(header::CONTENT_TYPE, s.mime.clone());
        let e = http_serve::ChunkedReadFile::new(f, Some(self.pool.clone()), hdrs)
            .map_err(internal_server_err)?;
        Ok(http_serve::serve(e, &req))
    }

    fn authreq(&self, req: &Request<::hyper::Body>) -> auth::Request {
        auth::Request {
            when_sec: Some(self.db.clocks().realtime().sec),
            addr: if self.trust_forward_hdrs {
                req.headers().get("X-Real-IP")
                   .and_then(|v| v.to_str().ok())
                   .and_then(|v| IpAddr::from_str(v).ok())
            } else { None },
            user_agent: req.headers().get(header::USER_AGENT).map(|ua| ua.as_bytes().to_vec()),
        }
    }

    fn request(&self, req: &Request<::hyper::Body>) -> ResponseResult {
        let authreq = self.authreq(req);
        let host = req.headers().get(header::HOST).map(|h| String::from_utf8_lossy(h.as_bytes()));
        let agent = authreq.user_agent.as_ref().map(|u| String::from_utf8_lossy(&u[..]));
        Ok(plain_response(StatusCode::OK, format!(
                    "when: {}\n\
                    host: {:?}\n\
                    addr: {:?}\n\
                    user_agent: {:?}\n\
                    secure: {:?}",
                    time::at(time::Timespec{sec: authreq.when_sec.unwrap(), nsec: 0})
                             .strftime("%FT%T")
                             .map(|f| f.to_string())
                             .unwrap_or_else(|e| e.to_string()),
                    host.as_ref().map(|h| &*h),
                    &authreq.addr,
                    agent.as_ref().map(|a| &*a),
                    self.is_secure(req))))
    }

    fn is_secure(&self, req: &Request<::hyper::Body>) -> bool {
        self.trust_forward_hdrs &&
            req.headers().get("X-Forwarded-Proto")
               .map(|v| v.as_bytes() == b"https")
               .unwrap_or(false)
    }

    fn login(&self, req: &Request<::hyper::Body>, body: hyper::Chunk) -> ResponseResult {
        let mut username = None;
        let mut password = None;
        for (key, value) in form_urlencoded::parse(&body) {
            match &*key {
                "username" => username = Some(value),
                "password" => password = Some(value),
                _ => {},
            };
        }
        let (username, password) = match (username, password) {
            (Some(u), Some(p)) => (u, p),
            _ => return Err(bad_req("expected username + password")),
        };
        let authreq = self.authreq(req);
        let host = req.headers().get(header::HOST).ok_or_else(|| bad_req("missing Host header!"))?;
        let host = host.as_bytes();
        let domain = match ::memchr::memchr(b':', host) {
            Some(colon) => &host[0..colon],
            None => host,
        }.to_owned();
        let mut l = self.db.lock();
        let is_secure = self.is_secure(req);
        let flags = (auth::SessionFlags::HttpOnly as i32) |
                    (auth::SessionFlags::SameSite as i32) |
                    (auth::SessionFlags::SameSiteStrict as i32) |
                    if is_secure { (auth::SessionFlags::Secure as i32) } else { 0 };
        let (sid, _) = l.login_by_password(authreq, &username, password.into_owned(), domain,
            flags)
            .map_err(|e| plain_response(StatusCode::UNAUTHORIZED, e.to_string()))?;
        let s_suffix = if is_secure {
            "; HttpOnly; Secure; SameSite=Strict; Max-Age=2147483648; Path=/"
        } else {
            "; HttpOnly; SameSite=Strict; Max-Age=2147483648; Path=/"
        };
        let mut encoded = [0u8; 64];
        base64::encode_config_slice(&sid, base64::STANDARD_NO_PAD, &mut encoded);
        let mut cookie = BytesMut::with_capacity("s=".len() + encoded.len() + s_suffix.len());
        cookie.put("s=");
        cookie.put(&encoded[..]);
        cookie.put(s_suffix);
        Ok(Response::builder()
            .header(header::SET_COOKIE, cookie.freeze())
            .status(StatusCode::NO_CONTENT)
            .body(b""[..].into()).unwrap())
    }

    fn logout(&self, req: &Request<hyper::Body>, body: hyper::Chunk) -> ResponseResult {
        // Parse parameters.
        let mut csrf = None;
        for (key, value) in form_urlencoded::parse(&body) {
            match &*key {
                "csrf" => csrf = Some(value),
                _ => {},
            };
        }

        let mut res = Response::new(b""[..].into());
        if let Some(sid) = extract_sid(req) {
            let authreq = self.authreq(req);
            let mut l = self.db.lock();
            let hash = sid.hash();
            let need_revoke = match l.authenticate_session(authreq.clone(), &hash) {
                Ok((s, _)) => {
                    let correct_csrf = if let Some(c) = csrf {
                        csrf_matches(&*c, s.csrf())
                    } else { false };
                    if !correct_csrf {
                        warn!("logout request with missing/incorrect csrf");
                        return Err(bad_req("logout with incorrect csrf token"));
                    }
                    info!("revoking session");
                    true
                },
                Err(e) => {
                    // TODO: distinguish "no such session", "session is no longer valid", and
                    // "user ... is disabled" (which are all client error / bad state) from database
                    // errors.
                    warn!("logout failed: {}", e);
                    false
                },
            };
            if need_revoke {
                // TODO: inline this above with non-lexical lifetimes.
                l.revoke_session(auth::RevocationReason::LoggedOut, None, authreq, &hash)
                 .map_err(internal_server_err)?;
            }

            // By now the session is invalid (whether it was valid to start with or not).
            // Clear useless cookie.
            res.headers_mut().append(header::SET_COOKIE,
                                     HeaderValue::from_str("s=; Max-Age=0; Path=/").unwrap());
        }
        *res.status_mut() = StatusCode::NO_CONTENT;
        Ok(res)
    }

    fn authenticated(&self, req: &Request<hyper::Body>) -> Result<Option<json::Session>, Error> {
        if let Some(sid) = extract_sid(req) {
            let authreq = self.authreq(req);
            match self.db.lock().authenticate_session(authreq.clone(), &sid.hash()) {
                Ok((s, u)) => {
                    return Ok(Some(json::Session {
                        username: u.username.clone(),
                        csrf: s.csrf(),
                    }))
                },
                Err(_) => {
                    // TODO: real error handling! this assumes all errors are due to lack of
                    // authentication, when they could be logic errors in SQL or such.
                    return Ok(None);
                }
            }
        }
        Ok(None)
    }
    fn save_sample_file_dir(&self, req: &Request<::hyper::Body>, body: serde_json::Value) -> ResponseResult {
        let (mut resp, writer) = http_serve::streaming_body(&req).build();
        resp.headers_mut().insert(header::CONTENT_TYPE,
                                  HeaderValue::from_static("application/json"));
        let mut db = self.db.lock();
        match body["path"].is_string() {
            true => {
                let id = db.add_sample_file_dir(body["path"].as_str().unwrap().to_string());
                match id {
                    Ok(id) => {
                        if let Some(mut w) = writer {
                            serde_json::to_writer(
                                &mut w,
                                &serde_json::json!({ "id": id })
                            ).map_err(internal_server_err)?
                        };
                    },
                    Err(e) => {
                        return Err(internal_server_err(e));
                    }
                }

            }
            _ => {
                *resp.status_mut() = StatusCode::BAD_REQUEST;
            }
        }
        Ok(resp)
    }
}

fn csrf_matches(csrf: &str, session: auth::SessionHash) -> bool {
    let mut b64 = [0u8; 32];
    session.encode_base64(&mut b64);
    ::ring::constant_time::verify_slices_are_equal(&b64[..], csrf.as_bytes()).is_ok()
}

/// Extracts `s` cookie from the HTTP request. Does not authenticate.
fn extract_sid(req: &Request<hyper::Body>) -> Option<auth::RawSessionId> {
    let hdr = match req.headers().get(header::COOKIE) {
        None => return None,
        Some(c) => c,
    };
    for mut cookie in hdr.as_bytes().split(|&b| b == b';') {
        if cookie.starts_with(b" ") {
            cookie = &cookie[1..];
        }
        if cookie.starts_with(b"s=") {
            let s = &cookie[2..];
            if let Ok(s) = auth::RawSessionId::decode_base64(s) {
                return Some(s);
            }
        }
    }
    None
}

pub struct Config<'a> {
    pub db: Arc<db::Database>,
    pub ui_dir: Option<&'a str>,
    pub require_auth: bool,
    pub trust_forward_hdrs: bool,
    pub time_zone_name: String,
}

#[derive(Clone)]
pub struct Service(Arc<ServiceInner>);

impl Service {
    pub fn new(config: Config) -> Result<Self, Error> {
        let mut ui_files = HashMap::new();
        if let Some(d) = config.ui_dir {
            Service::fill_ui_files(d, &mut ui_files);
        }
        debug!("UI files: {:#?}", ui_files);
        let dirs_by_stream_id = {
            let l = config.db.lock();
            let mut d =
                FnvHashMap::with_capacity_and_hasher(l.streams_by_id().len(), Default::default());
            for (&id, s) in l.streams_by_id().iter() {
                let dir_id = match s.sample_file_dir_id {
                    Some(d) => d,
                    None => continue,
                };
                d.insert(id, l.sample_file_dirs_by_id()
                              .get(&dir_id)
                              .unwrap()
                              .get()?);
            }
            Arc::new(d)
        };

        Ok(Service(Arc::new(ServiceInner {
            db: config.db,
            dirs_by_stream_id,
            ui_files,
            pool: futures_cpupool::Builder::new().pool_size(1).name_prefix("static").create(),
            require_auth: config.require_auth,
            trust_forward_hdrs: config.trust_forward_hdrs,
            time_zone_name: config.time_zone_name,
        })))
    }

    fn fill_ui_files(dir: &str, files: &mut HashMap<String, UiFile>) {
        let r = match fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) => {
                warn!("Unable to search --ui-dir={}; will serve no static files. Error was: {}",
                      dir, e);
                return;
            }
        };
        for e in r {
            let e = match e {
                Ok(e) => e,
                Err(e) => {
                    warn!("Error searching UI directory; may be missing files. Error was: {}", e);
                    continue;
                },
            };
            let (p, mime) = match e.file_name().to_str() {
                Some(n) if n == "index.html" => ("/".to_owned(), "text/html"),
                Some(n) if n.ends_with(".html") => (format!("/{}", n), "text/html"),
                Some(n) if n.ends_with(".ico") => (format!("/{}", n), "image/vnd.microsoft.icon"),
                Some(n) if n.ends_with(".js") => (format!("/{}", n), "text/javascript"),
                Some(n) if n.ends_with(".map") => (format!("/{}", n), "text/javascript"),
                Some(n) if n.ends_with(".png") => (format!("/{}", n), "image/png"),
                Some(n) => {
                    warn!("UI directory file {:?} has unknown extension; skipping", n);
                    continue;
                },
                None => {
                    warn!("UI directory file {:?} is not a valid UTF-8 string; skipping",
                          e.file_name());
                    continue;
                },
            };
            files.insert(p, UiFile {
                mime: HeaderValue::from_static(mime),
                path: e.path(),
            });
        }
    }

    /// Returns a future separating the request from its form body.
    ///
    /// If this is not a `POST` or the body's `Content-Type` is not
    /// `application/x-www-form-urlencoded`, returns an appropriate error response instead.
    ///
    /// Use with `and_then` to chain logic which consumes the form body.
    fn with_form_body(&self, mut req: Request<hyper::Body>)
                      -> Box<Future<Item = (Request<hyper::Body>, hyper::Chunk),
                                    Error = Response<Body>> +
                             Send + 'static> {
        if *req.method() != http::method::Method::POST {
            return Box::new(future::err(plain_response(StatusCode::METHOD_NOT_ALLOWED,
                                                       "POST expected")));
        }
        let correct_mime_type = match req.headers().get(header::CONTENT_TYPE) {
            Some(t) if t == "application/x-www-form-urlencoded" => true,
            Some(t) if t == "application/x-www-form-urlencoded; charset=UTF-8" => true,
            _ => false,
        };
        if !correct_mime_type {
            return Box::new(future::err(bad_req(
                        "expected application/x-www-form-urlencoded request body")));
        }
        let b = ::std::mem::replace(req.body_mut(), hyper::Body::empty());
        Box::new(b.concat2()
                  .map(|b| (req, b))
                  .map_err(|e| internal_server_err(format_err!("unable to read request body: {}",
                                                                 e))))
    }

    /// Returns a future separating the request from its form body.
    ///
    /// If this is not a `POST` or the body's `Content-Type` is not
    /// `application/json`, returns an appropriate error response instead.
    ///
    /// Use with `and_then` to chain logic which consumes the json body.
    fn with_json_body(&self, mut req: Request<hyper::Body>)
                      -> Box<Future<Item = (Request<hyper::Body>, serde_json::Value),
                                    Error = Response<Body>> +
                             Send + 'static> {
        if *req.method() != http::method::Method::POST {
            return Box::new(future::err(plain_response(StatusCode::METHOD_NOT_ALLOWED,
                                                       "POST expected")));
        }
        let correct_mime_type = match req.headers().get(header::CONTENT_TYPE) {
            Some(t) if t == "application/json" => true,
            _ => false,
        };
        if !correct_mime_type {
            return Box::new(future::err(bad_req(
                        "expected application/json request body")));
        }
        let b = ::std::mem::replace(req.body_mut(), hyper::Body::empty());
        Box::new(b.concat2()
                  .map_err(|e| {
                      internal_server_err(format_err!("unable to read request body: {}", e))
                  })
                  .and_then(|b| {
                      let body_vec = b.into_bytes().to_vec();
                      let json_str = String::from_utf8(body_vec).unwrap();
                      let json_obj = match serde_json::from_str(&json_str) {
                          Ok(json) => json,
                          Err(e) => return Err(bad_req(format!("error parsing json: {}", e))),
                      };
                      Ok((req, json_obj))
                  })
                  .map(|ret| {
                      ret
                  }).
                  map_err(|e| {
                      e
                  }))
    }

    fn stream_live_m4s(&self, _req: &Request<::hyper::Body>, uuid: Uuid,
                       stream_type: db::StreamType) -> ResponseResult {
        let stream_id;
        let open_id;
        let (sub_tx, sub_rx) = futures::sync::mpsc::unbounded();
        {
            let mut db = self.0.db.lock();
            open_id = match db.open {
                None => return Err(plain_response(
                        StatusCode::PRECONDITION_FAILED,
                        "database is read-only; there are no live streams")),
                Some(o) => o.id,
            };
            let camera = db.get_camera(uuid)
                           .ok_or_else(|| plain_response(StatusCode::NOT_FOUND,
                                                         format!("no such camera {}", uuid)))?;
            stream_id = camera.streams[stream_type.index()]
                .ok_or_else(|| plain_response(StatusCode::NOT_FOUND,
                                              format!("no such stream {}/{}", uuid,
                                                      stream_type)))?;
            db.watch_live(stream_id, Box::new(move |l| sub_tx.unbounded_send(l).is_ok()))
                .expect("stream_id refed by camera");
        }
        let inner = self.0.clone();
        let body: crate::body::BodyStream = Box::new(sub_rx
            .map_err(|()| unreachable!())
            .and_then(move |live| -> Result<_, base::Error> {
                let mut builder = mp4::FileBuilder::new(mp4::Type::MediaSegment);
                let mut vse_id = None;
                {
                    let db = inner.db.lock();
                    let mut rows = 0;
                    db.list_recordings_by_id(stream_id, live.recording .. live.recording+1,
                                             &mut |r| {
                        rows += 1;
                        let vse = db.video_sample_entries_by_id().get(&r.video_sample_entry_id)
                                    .unwrap();
                        vse_id = Some(strutil::hex(&vse.sha1));
                        builder.append(&db, r, live.off_90k.clone())?;
                        Ok(())
                    }).err_kind(base::ErrorKind::Unknown)?;
                    if rows != 1 {
                        bail_t!(Internal, "unable to find {:?}", live);
                    }
                }
                let vse_id = vse_id.unwrap();
                use http_serve::Entity;
                let mp4 = builder.build(inner.db.clone(), inner.dirs_by_stream_id.clone())?;
                let mut hdrs = http::header::HeaderMap::new();
                mp4.add_headers(&mut hdrs);
                //Ok(format!("{:?}\n\n", mp4).into())
                let mime_type = hdrs.get(http::header::CONTENT_TYPE).unwrap();
                let len = mp4.len();
                use futures::stream::once;
                let hdr = format!(
                    "--B\r\n\
                    Content-Length: {}\r\n\
                    Content-Type: {}\r\n\
                    X-Recording-Id: {}\r\n\
                    X-Time-Range: {}-{}\r\n\
                    X-Video-Sample-Entry-Sha1: {}\r\n\r\n",
                    len,
                    mime_type.to_str().unwrap(),
                    live.recording,
                    live.off_90k.start,
                    live.off_90k.end,
                    &vse_id);
                let v: Vec<crate::body::BodyStream> = vec![
                    Box::new(once(Ok(hdr.into()))),
                    mp4.get_range(0 .. len),
                    Box::new(once(Ok("\r\n\r\n".into())))
                ];
                Ok(futures::stream::iter_ok::<_, crate::body::BoxedError>(v))
            })
            .map_err(|e| Box::new(e.compat()))
            .flatten()
            .flatten());
        let body: Body = body.into();
        Ok(http::Response::builder()
            .header("X-Open-Id", open_id.to_string())
            .header("Content-Type", "multipart/mixed; boundary=B")
            .body(body)
            .unwrap())
    }
}

impl ::hyper::service::Service for Service {
    type ReqBody = ::hyper::Body;
    type ResBody = Body;
    type Error = BoxedError;
    type Future = Box<Future<Item = Response<Self::ResBody>, Error = Self::Error> + Send + 'static>;

    fn call(&mut self, req: Request<::hyper::Body>) -> Self::Future {
        fn wrap<R>(is_private: bool, r: R)
               -> Box<Future<Item = Response<Body>, Error = BoxedError> + Send + 'static>
        where R: Future<Item = Response<Body>, Error = Response<Body>> + Send + 'static {
            return Box::new(r.or_else(|e| Ok(e)).map(move |mut r| {
                if is_private {
                    r.headers_mut().insert("Cache-Control", HeaderValue::from_static("private"));
                }
                r
            }))
        }

        fn wrap_r(is_private: bool, r: ResponseResult)
               -> Box<Future<Item = Response<Body>, Error = BoxedError> + Send + 'static> {
            return wrap(is_private, future::result(r))
        }
        let p = Path::decode(req.uri().path());
        let require_auth = self.0.require_auth && match p {
            Path::NotFound | Path::Request | Path::Login | Path::Logout | Path::Static => false,
            _ => true,
        };
        debug!("request on: {}: {:?}, require_auth={}", req.uri(), p, require_auth);
        let session = match self.0.authenticated(&req) {
            Ok(s) => s,
            Err(e) => return Box::new(future::ok(internal_server_err(e))),
        };
        if require_auth && session.is_none() {
            return Box::new(future::ok(
                    plain_response(StatusCode::UNAUTHORIZED, "unauthorized")));
        }
        match p {
            Path::InitSegment(sha1, debug) => wrap_r(true, self.0.init_segment(sha1, debug, &req)),
            Path::TopLevel => wrap_r(true, self.0.top_level(&req, session)),
            Path::Request => wrap_r(true, self.0.request(&req)),
            Path::SaveCamera => wrap(true, self.with_json_body(req).and_then({
                let s = self.clone();
                move |(req, b)| { s.0.save_camera(&req, b) }
            })),
            Path::GetCamera(uuid) => wrap_r(true, self.0.camera(&req, uuid)),
            Path::StreamRecordings(uuid, type_) => {
                wrap_r(true, self.0.stream_recordings(&req, uuid, type_))
            },
            Path::StreamViewMp4(uuid, type_, debug) => {
                wrap_r(true, self.0.stream_view_mp4(&req, uuid, type_, mp4::Type::Normal, debug))
            },
            Path::StreamViewMp4Segment(uuid, type_, debug) => {
                wrap_r(true, self.0.stream_view_mp4(&req, uuid, type_, mp4::Type::MediaSegment,
                                                    debug))
            },
            Path::StreamLiveMp4Segments(uuid, type_) => {
                wrap_r(true, self.stream_live_m4s(&req, uuid, type_))
            },
            Path::SaveSampleFileDir => wrap(true, self.with_json_body(req).and_then({
                let s = self.clone();
                move |(req, b)| { s.0.save_sample_file_dir(&req, b) }
            })),
            Path::NotFound => wrap(true, future::err(not_found("path not understood"))),
            Path::Login => wrap(true, self.with_form_body(req).and_then({
                let s = self.clone();
                move |(req, b)| { s.0.login(&req, b) }
            })),
            Path::Logout => wrap(true, self.with_form_body(req).and_then({
                let s = self.clone();
                move |(req, b)| { s.0.logout(&req, b) }
            })),
            Path::Static => wrap_r(false, self.0.static_file(&req, req.uri().path())),
        }
    }
}

#[cfg(test)]
mod tests {
    use db::testutil::{self, TestDb};
    use futures::Future;
    use http::header;
    use log::info;
    use std::collections::HashMap;
    use std::error::Error as StdError;
    use super::Segments;

    struct Server {
        db: TestDb<base::clock::RealClocks>,
        base_url: String,
        //test_camera_uuid: Uuid,
        handle: Option<::std::thread::JoinHandle<()>>,
        shutdown_tx: Option<futures::sync::oneshot::Sender<()>>,
    }

    impl Server {
        fn new(require_auth: bool) -> Server {
            let db = TestDb::new(base::clock::RealClocks {});
            let (shutdown_tx, shutdown_rx) = futures::sync::oneshot::channel::<()>();
            let addr = "127.0.0.1:0".parse().unwrap();
            let service = super::Service::new(super::Config {
                db: db.db.clone(),
                ui_dir: None,
                require_auth,
                trust_forward_hdrs: true,
                time_zone_name: "".to_owned(),
            }).unwrap();
            let server = hyper::server::Server::bind(&addr)
                .tcp_nodelay(true)
                .serve(move || Ok::<_, Box<StdError + Send + Sync>>(service.clone()));
            let addr = server.local_addr();  // resolve port 0 to a real ephemeral port number.
            let handle = ::std::thread::spawn(move || {
                ::tokio::run(server.with_graceful_shutdown(shutdown_rx).map_err(|e| panic!(e)));
            });

            // Create a user.
            let mut c = db::UserChange::add_user("slamb".to_owned());
            c.set_password("hunter2".to_owned());
            db.db.lock().apply_user_change(c).unwrap();

            Server {
                db,
                base_url: format!("http://{}:{}", addr.ip(), addr.port()),
                handle: Some(handle),
                shutdown_tx: Some(shutdown_tx),
            }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.shutdown_tx.take().unwrap().send(()).unwrap();
            self.handle.take().unwrap().join().unwrap()
        }
    }

    #[derive(Clone, Debug, Default)]
    struct SessionCookie(Option<String>);

    impl SessionCookie {
        pub fn new(headers: &http::HeaderMap) -> Self {
            let mut c = SessionCookie::default();
            c.update(headers);
            c
        }

        pub fn update(&mut self, headers: &http::HeaderMap) {
            for set_cookie in headers.get_all(header::SET_COOKIE) {
                let mut set_cookie = set_cookie.to_str().unwrap().split("; ");
                let c = set_cookie.next().unwrap();
                let mut clear = false;
                for attr in set_cookie {
                    if attr == "Max-Age=0" {
                        clear = true;
                    }
                }
                if !c.starts_with("s=") {
                    panic!("unrecognized cookie");
                }
                self.0 = if clear { None } else { Some(c.to_owned()) };
            }
        }

        /// Produces a `Cookie` header value.
        pub fn header(&self) -> String {
            self.0.as_ref().map(|s| s.as_str()).unwrap_or("").to_owned()
        }
    }

    #[test]
    fn paths() {
        use super::Path;
        use uuid::Uuid;
        let cam_uuid = Uuid::parse_str("35144640-ff1e-4619-b0d5-4c74c185741c").unwrap();
        assert_eq!(Path::decode("/foo"), Path::Static);
        assert_eq!(Path::decode("/api/"), Path::TopLevel);
        assert_eq!(Path::decode("/api/init/07cec464126825088ea86a07eddd6a00afa71559.mp4"),
                   Path::InitSegment([0x07, 0xce, 0xc4, 0x64, 0x12, 0x68, 0x25, 0x08, 0x8e, 0xa8,
                                      0x6a, 0x07, 0xed, 0xdd, 0x6a, 0x00, 0xaf, 0xa7, 0x15, 0x59],
                                     false));
        assert_eq!(Path::decode("/api/init/07cec464126825088ea86a07eddd6a00afa71559.mp4.txt"),
                   Path::InitSegment([0x07, 0xce, 0xc4, 0x64, 0x12, 0x68, 0x25, 0x08, 0x8e, 0xa8,
                                      0x6a, 0x07, 0xed, 0xdd, 0x6a, 0x00, 0xaf, 0xa7, 0x15, 0x59],
                                     true));
        assert_eq!(Path::decode("/api/init/000000000000000000000000000000000000000x.mp4"),
                   Path::NotFound);  // non-hexadigit
        assert_eq!(Path::decode("/api/init/000000000000000000000000000000000000000.mp4"),
                   Path::NotFound);  // too short
        assert_eq!(Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/"),
                   Path::GetCamera(cam_uuid));
        assert_eq!(Path::decode("/api/cameras/asdf/"), Path::NotFound);
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/recordings"),
            Path::StreamRecordings(cam_uuid, db::StreamType::MAIN));
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/sub/recordings"),
            Path::StreamRecordings(cam_uuid, db::StreamType::SUB));
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/junk/recordings"),
            Path::NotFound);
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/view.mp4"),
            Path::StreamViewMp4(cam_uuid, db::StreamType::MAIN, false));
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/view.mp4.txt"),
            Path::StreamViewMp4(cam_uuid, db::StreamType::MAIN, true));
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/view.m4s"),
            Path::StreamViewMp4Segment(cam_uuid, db::StreamType::MAIN, false));
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/view.m4s.txt"),
            Path::StreamViewMp4Segment(cam_uuid, db::StreamType::MAIN, true));
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/live.m4s"),
            Path::StreamLiveMp4Segments(cam_uuid, db::StreamType::MAIN));
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/junk"),
            Path::NotFound);
        assert_eq!(Path::decode("/api/login"), Path::Login);
        assert_eq!(Path::decode("/api/logout"), Path::Logout);
        assert_eq!(Path::decode("/api/junk"), Path::NotFound);
    }

    #[test]
    fn test_segments() {
        testutil::init();
        assert_eq!(Segments{ids: 1..2, open_id: None, start_time: 0, end_time: None},
                   Segments::parse("1").unwrap());
        assert_eq!(Segments{ids: 1..2, open_id: Some(42), start_time: 0, end_time: None},
                   Segments::parse("1@42").unwrap());
        assert_eq!(Segments{ids: 1..2, open_id: None, start_time: 26, end_time: None},
                   Segments::parse("1.26-").unwrap());
        assert_eq!(Segments{ids: 1..2, open_id: Some(42), start_time: 26, end_time: None},
                   Segments::parse("1@42.26-").unwrap());
        assert_eq!(Segments{ids: 1..2, open_id: None, start_time: 0, end_time: Some(42)},
                   Segments::parse("1.-42").unwrap());
        assert_eq!(Segments{ids: 1..2, open_id: None, start_time: 26, end_time: Some(42)},
                   Segments::parse("1.26-42").unwrap());
        assert_eq!(Segments{ids: 1..6, open_id: None, start_time: 0, end_time: None},
                   Segments::parse("1-5").unwrap());
        assert_eq!(Segments{ids: 1..6, open_id: None, start_time: 26, end_time: None},
                   Segments::parse("1-5.26-").unwrap());
        assert_eq!(Segments{ids: 1..6, open_id: None, start_time: 0, end_time: Some(42)},
                   Segments::parse("1-5.-42").unwrap());
        assert_eq!(Segments{ids: 1..6, open_id: None, start_time: 26, end_time: Some(42)},
                   Segments::parse("1-5.26-42").unwrap());
    }

    #[test]
    fn unauthorized_without_cookie() {
        testutil::init();
        let s = Server::new(true);
        let cli = reqwest::Client::new();
        let resp = cli.get(&format!("{}/api/", &s.base_url)).send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn login() {
        testutil::init();
        let s = Server::new(true);
        let cli = reqwest::Client::new();
        let login_url = format!("{}/api/login", &s.base_url);

        let resp = cli.get(&login_url).send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::METHOD_NOT_ALLOWED);

        let resp = cli.post(&login_url).send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);

        let mut p = HashMap::new();
        p.insert("username", "slamb");
        p.insert("password", "asdf");
        let resp = cli.post(&login_url).form(&p).send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::UNAUTHORIZED);

        p.insert("password", "hunter2");
        let resp = cli.post(&login_url).form(&p).send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);
        let cookie = SessionCookie::new(resp.headers());
        info!("cookie: {:?}", cookie);
        info!("header: {}", cookie.header());

        let resp = cli.get(&format!("{}/api/", &s.base_url))
                      .header(header::COOKIE, cookie.header())
                      .send()
                      .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    }

    #[test]
    fn logout() {
        testutil::init();
        let s = Server::new(true);
        let cli = reqwest::Client::new();
        let mut p = HashMap::new();
        p.insert("username", "slamb");
        p.insert("password", "hunter2");
        let resp = cli.post(&format!("{}/api/login", &s.base_url)).form(&p).send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);
        let cookie = SessionCookie::new(resp.headers());

        // A GET shouldn't work.
        let resp = cli.get(&format!("{}/api/logout", &s.base_url))
                      .header(header::COOKIE, cookie.header())
                      .send()
                      .unwrap();
        assert_eq!(resp.status(), http::StatusCode::METHOD_NOT_ALLOWED);

        // Neither should a POST without a csrf token.
        let resp = cli.post(&format!("{}/api/logout", &s.base_url))
                      .header(header::COOKIE, cookie.header())
                      .send()
                      .unwrap();
        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);

        // But it should work with the csrf token.
        // Retrieve that from the toplevel API request.
        let toplevel: serde_json::Value = cli.post(&format!("{}/api/", &s.base_url))
                                             .header(header::COOKIE, cookie.header())
                                             .send().unwrap()
                                             .json().unwrap();
        let csrf = toplevel.get("session").unwrap().get("csrf").unwrap().as_str();
        let mut p = HashMap::new();
        p.insert("csrf", csrf);
        let resp = cli.post(&format!("{}/api/logout", &s.base_url))
                      .header(header::COOKIE, cookie.header())
                      .form(&p)
                      .send()
                      .unwrap();
        assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);
        let mut updated_cookie = cookie.clone();
        updated_cookie.update(resp.headers());

        // The cookie should be cleared client-side.
        assert!(updated_cookie.0.is_none());

        // It should also be invalidated server-side.
        let resp = cli.get(&format!("{}/api/", &s.base_url))
                      .header(header::COOKIE, cookie.header())
                      .send()
                      .unwrap();
        assert_eq!(resp.status(), http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn save_camera() {
        testutil::init();
        let s = Server::new(true);
        let cli = reqwest::Client::new();

        // first login before adding cameras
        let login_url = format!("{}/api/login", &s.base_url);
        let mut p = HashMap::new();
        p.insert("username", "slamb");
        p.insert("password", "hunter2");
        let resp = cli.post(&login_url).form(&p).send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);

        // should reutrn 405 when sending a get
        let cookie = SessionCookie::new(resp.headers());
        let resp = cli.get(&format!("{}/api/cameras", &s.base_url))
                      .header(header::COOKIE, cookie.header())
                      .send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::METHOD_NOT_ALLOWED);

        // should reutrn 400 when content-type is not application/json
        let mut resp = cli.post(&format!("{}/api/cameras", &s.base_url))
                      .header(header::CONTENT_TYPE, "NOT-application/json")
                      .header(header::COOKIE, cookie.header())
                      .body("{}").send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(&resp.text().unwrap(), "expected application/json request body");

        // should reutrn 400 when content-type is not application/json
        let resp = cli.post(&format!("{}/api/cameras", &s.base_url))
                      .header(header::CONTENT_TYPE, "application/json")
                      .header(header::COOKIE, cookie.header())
                      .body("BAD JSON").send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);

        // should reutrn 400 when required fields are not present
        let mut resp = cli.post(&format!("{}/api/cameras", &s.base_url))
                      .header(header::CONTENT_TYPE, "application/json")
                      .header(header::COOKIE, cookie.header())
                      .body("{}").send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(&resp.text().unwrap(), "missing fields");

        // should return 400 when invalid stream changes are sent
               // should return 200 when valid stream changes are sent
        let cam = db::CameraChange {
            short_name: String::from("Test Camera"),
            description: String::from("Test Camera"),
            host: String::from("testhost:443"),
            username: String::from("slamb"),
            password: String::from("hunter2"),
            streams: [db::StreamChange {
                rtsp_path: String::from("/test/path"),
                flush_if_sec: 10000,
                record: true,
                sample_file_dir_id: Some(1337) // purposely fake id
            },Default::default()]
        };

        let resp = cli.post(&format!("{}/api/cameras", &s.base_url))
                      .header(header::CONTENT_TYPE, "application/json")
                      .header(header::COOKIE, cookie.header())
                      .body(serde_json::to_string(&cam).unwrap()).send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);

        // create a sample dir
        let tmpdir = tempdir::TempDir::new("moonfire-nvr-test").unwrap();
        let json_body = serde_json::json!({ "path": tmpdir.path().to_str().unwrap() }).to_string();
        // should return 200 with id if success
        let mut resp = cli.post(&format!("{}/api/dirs", &s.base_url))
                      .header(header::COOKIE, cookie.header())
                      .header(header::CONTENT_TYPE, "application/json")
                      .body(json_body)
                      .send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let res_obj: serde_json::Value = serde_json::from_str(&resp.text().unwrap()).unwrap();
        assert_eq!(res_obj["id"].is_i64(), true);

        // should return 200 when valid stream changes are sent
        let cam = db::CameraChange {
            short_name: String::from("Test Camera"),
            description: String::from("Test Camera"),
            host: String::from("testhost:443"),
            username: String::from("slamb"),
            password: String::from("hunter2"),
            streams: [db::StreamChange {
                rtsp_path: String::from("/test/path"),
                flush_if_sec: 10000,
                record: true,
                sample_file_dir_id: Some(res_obj["id"].as_i64().unwrap() as i32)
            },Default::default()]
        };
        let mut resp = cli.post(&format!("{}/api/cameras", &s.base_url))
                      .header(header::CONTENT_TYPE, "application/json")
                      .header(header::COOKIE, cookie.header())
                      .body(serde_json::to_string(&cam).unwrap()).send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let res_obj: serde_json::Value = serde_json::from_str(&resp.text().unwrap()).unwrap();
        assert_eq!(res_obj["camera_id"].is_u64(), true);
    }


    #[test]
    fn view_without_segments() {
        testutil::init();
        let s = Server::new(false);
        let cli = reqwest::Client::new();
        let resp = cli.get(
            &format!("{}/api/cameras/{}/main/view.mp4", &s.base_url, s.db.test_camera_uuid))
            .send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
    }
    #[test]
    fn save_sample_file_dir(){
        testutil::init();
        let s = Server::new(true);
        let cli = reqwest::Client::new();

        // first login before adding cameras
        let login_url = format!("{}/api/login", &s.base_url);
        let mut p = HashMap::new();
        p.insert("username", "slamb");
        p.insert("password", "hunter2");
        let resp = cli.post(&login_url).form(&p).send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::NO_CONTENT);

        // should return a 400 if no path is sent
        let cookie = SessionCookie::new(resp.headers());
        let resp = cli.post(&format!("{}/api/dirs", &s.base_url))
                      .header(header::CONTENT_TYPE, "application/json")
                      .header(header::COOKIE, cookie.header())
                      .body("{}")
                      .send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);

        let tmpdir = tempdir::TempDir::new("moonfire-nvr-test").unwrap();
        let json_body = serde_json::json!({ "path": tmpdir.path().to_str().unwrap() }).to_string();
        // should return 200 with id if success
        let mut resp = cli.post(&format!("{}/api/dirs", &s.base_url))
                      .header(header::COOKIE, cookie.header())
                      .header(header::CONTENT_TYPE, "application/json")
                      .body(json_body)
                      .send().unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let res_obj: serde_json::Value = serde_json::from_str(&resp.text().unwrap()).unwrap();
        assert_eq!(res_obj["id"].is_u64(), true);
    }
}

#[cfg(all(test, feature="nightly"))]
mod bench {
    extern crate test;

    use db::testutil::{self, TestDb};
    use futures::Future;
    use hyper;
    use lazy_static::lazy_static;
    use std::error::Error as StdError;
    use uuid::Uuid;

    struct Server {
        base_url: String,
        test_camera_uuid: Uuid,
    }

    impl Server {
        fn new() -> Server {
            let db = TestDb::new(::base::clock::RealClocks {});
            let test_camera_uuid = db.test_camera_uuid;
            testutil::add_dummy_recordings_to_db(&db.db, 1440);
            let addr = "127.0.0.1:0".parse().unwrap();
            let require_auth = false;
            let service = super::Service::new(super::Config {
                db: db.db.clone(),
                ui_dir: None,
                require_auth,
                trust_forward_hdrs: false,
                time_zone_name: "".to_owned(),
            }).unwrap();
            let server = hyper::server::Server::bind(&addr)
                .tcp_nodelay(true)
                .serve(move || Ok::<_, Box<StdError + Send + Sync>>(service.clone()));
            let addr = server.local_addr();  // resolve port 0 to a real ephemeral port number.
            ::std::thread::spawn(move || {
                ::tokio::run(server.map_err(|e| panic!(e)));
            });
            Server {
                base_url: format!("http://{}:{}", addr.ip(), addr.port()),
                test_camera_uuid,
            }
        }
    }

    lazy_static! {
        static ref SERVER: Server = { Server::new() };
    }

    #[bench]
    fn serve_stream_recordings(b: &mut test::Bencher) {
        testutil::init();
        let server = &*SERVER;
        let url = reqwest::Url::parse(&format!("{}/api/cameras/{}/main/recordings", server.base_url,
                                               server.test_camera_uuid)).unwrap();
        let mut buf = Vec::new();
        let client = reqwest::Client::new();
        let mut f = || {
            let mut resp = client.get(url.clone()).send().unwrap();
            assert_eq!(resp.status(), reqwest::StatusCode::OK);
            buf.clear();
            use std::io::Read;
            resp.read_to_end(&mut buf).unwrap();
        };
        f();  // warm.
        b.iter(f);
    }
}
