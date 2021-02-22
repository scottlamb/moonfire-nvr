// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use crate::body::Body;
use crate::json;
use crate::mp4;
use base::clock::Clocks;
use base::{bail_t, ErrorKind};
use bytes::{BufMut, BytesMut};
use core::borrow::Borrow;
use core::str::FromStr;
use db::dir::SampleFileDir;
use db::{auth, recording};
use failure::{bail, format_err, Error};
use fnv::FnvHashMap;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use http::header::{self, HeaderValue};
use http::{status::StatusCode, Request, Response};
use http_serve::dir::FsDir;
use hyper::body::Bytes;
use log::{debug, info, warn};
use memchr::memchr;
use nom::bytes::complete::{tag, take_while1};
use nom::combinator::{all_consuming, map, map_res, opt};
use nom::sequence::{preceded, tuple};
use nom::IResult;
use std::cmp;
use std::convert::TryFrom;
use std::net::IpAddr;
use std::ops::Range;
use std::sync::Arc;
use tokio_tungstenite::tungstenite;
use url::form_urlencoded;
use uuid::Uuid;

#[derive(Debug, Eq, PartialEq)]
enum Path {
    TopLevel,                                         // "/api/"
    Request,                                          // "/api/request"
    InitSegment(i32, bool),                           // "/api/init/<id>.mp4{.txt}"
    Camera(Uuid),                                     // "/api/cameras/<uuid>/"
    Signals,                                          // "/api/signals"
    StreamRecordings(Uuid, db::StreamType),           // "/api/cameras/<uuid>/<type>/recordings"
    StreamViewMp4(Uuid, db::StreamType, bool),        // "/api/cameras/<uuid>/<type>/view.mp4{.txt}"
    StreamViewMp4Segment(Uuid, db::StreamType, bool), // "/api/cameras/<uuid>/<type>/view.m4s{.txt}"
    StreamLiveMp4Segments(Uuid, db::StreamType),      // "/api/cameras/<uuid>/<type>/live.m4s"
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
            "/login" => return Path::Login,
            "/logout" => return Path::Logout,
            "/request" => return Path::Request,
            "/signals" => return Path::Signals,
            _ => {}
        };
        if path.starts_with("/init/") {
            let (debug, path) = if path.ends_with(".txt") {
                (true, &path[0..path.len() - 4])
            } else {
                (false, path)
            };
            if !path.ends_with(".mp4") {
                return Path::NotFound;
            }
            let id_start = "/init/".len();
            let id_end = path.len() - ".mp4".len();
            if let Ok(id) = i32::from_str(&path[id_start..id_end]) {
                return Path::InitSegment(id, debug);
            }
            return Path::NotFound;
        }
        if !path.starts_with("/cameras/") {
            return Path::NotFound;
        }
        let path = &path["/cameras/".len()..];
        let slash = match path.find('/') {
            None => {
                return Path::NotFound;
            }
            Some(s) => s,
        };
        let uuid = &path[0..slash];
        let path = &path[slash + 1..];

        // TODO(slamb): require uuid to be in canonical format.
        let uuid = match Uuid::parse_str(uuid) {
            Ok(u) => u,
            Err(_) => return Path::NotFound,
        };

        if path.is_empty() {
            return Path::Camera(uuid);
        }

        let slash = match path.find('/') {
            None => {
                return Path::NotFound;
            }
            Some(s) => s,
        };
        let (type_, path) = path.split_at(slash);

        let type_ = match db::StreamType::parse(type_) {
            None => {
                return Path::NotFound;
            }
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
        .body(body.into())
        .expect("hardcoded head should be valid")
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
        ErrorKind::PermissionDenied | ErrorKind::Unauthenticated => StatusCode::UNAUTHORIZED,
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

fn num<'a, T: FromStr>() -> impl FnMut(&'a str) -> IResult<&'a str, T> {
    map_res(take_while1(|c: char| c.is_ascii_digit()), FromStr::from_str)
}

impl Segments {
    /// Parses the `s` query parameter to `view.mp4` as described in `design/api.md`.
    /// Doesn't do any validation.
    fn parse(i: &str) -> IResult<&str, Segments> {
        // Parse START_ID[-END_ID] into Range<i32>.
        // Note that END_ID is inclusive, but Ranges are half-open.
        let (i, ids) = map(
            tuple((num::<i32>(), opt(preceded(tag("-"), num::<i32>())))),
            |(start, end)| start..end.unwrap_or(start) + 1,
        )(i)?;

        // Parse [@OPEN_ID] into Option<u32>.
        let (i, open_id) = opt(preceded(tag("@"), num::<u32>()))(i)?;

        // Parse [.[REL_START_TIME]-[REL_END_TIME]] into (i64, Option<i64>).
        let (i, (start_time, end_time)) = map(
            opt(preceded(
                tag("."),
                tuple((opt(num::<i64>()), tag("-"), opt(num::<i64>()))),
            )),
            |t| t.map(|(s, _, e)| (s.unwrap_or(0), e)).unwrap_or((0, None)),
        )(i)?;

        Ok((
            i,
            Segments {
                ids,
                open_id,
                start_time,
                end_time,
            },
        ))
    }
}

impl FromStr for Segments {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (_, s) = all_consuming(Segments::parse)(s).map_err(|_| ())?;
        if s.ids.end <= s.ids.start {
            return Err(());
        }
        if let Some(e) = s.end_time {
            if e < s.start_time {
                return Err(());
            }
        }
        Ok(s)
    }
}

struct Caller {
    permissions: db::Permissions,
    session: Option<json::Session>,
}

type ResponseResult = Result<Response<Body>, Response<Body>>;

fn serve_json<T: serde::ser::Serialize>(req: &Request<hyper::Body>, out: &T) -> ResponseResult {
    let (mut resp, writer) = http_serve::streaming_body(&req).build();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Some(mut w) = writer {
        serde_json::to_writer(&mut w, out).map_err(internal_server_err)?;
    }
    Ok(resp)
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

/// Extracts an `application/json` POST body from a request.
///
/// This returns the request body as bytes rather than performing
/// deserialization. Keeping the bytes allows the caller to use a `Deserialize`
/// that borrows from the bytes.
async fn extract_json_body(req: &mut Request<hyper::Body>) -> Result<Bytes, Response<Body>> {
    if *req.method() != http::method::Method::POST {
        return Err(plain_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "POST expected",
        ));
    }
    let correct_mime_type = match req.headers().get(header::CONTENT_TYPE) {
        Some(t) if t == "application/json" => true,
        Some(t) if t == "application/json; charset=UTF-8" => true,
        _ => false,
    };
    if !correct_mime_type {
        return Err(bad_req("expected application/json request body"));
    }
    let b = ::std::mem::replace(req.body_mut(), hyper::Body::empty());
    hyper::body::to_bytes(b)
        .await
        .map_err(|e| internal_server_err(format_err!("unable to read request body: {}", e)))
}

pub struct Config<'a> {
    pub db: Arc<db::Database>,
    pub ui_dir: Option<&'a std::path::Path>,
    pub trust_forward_hdrs: bool,
    pub time_zone_name: String,
    pub allow_unauthenticated_permissions: Option<db::Permissions>,
}

pub struct Service {
    db: Arc<db::Database>,
    ui_dir: Option<Arc<FsDir>>,
    dirs_by_stream_id: Arc<FnvHashMap<i32, Arc<SampleFileDir>>>,
    time_zone_name: String,
    allow_unauthenticated_permissions: Option<db::Permissions>,
    trust_forward_hdrs: bool,
}

/// Useful HTTP `Cache-Control` values to set on successful (HTTP 200) API responses.
enum CacheControl {
    /// For endpoints which have private data that may change from request to request.
    PrivateDynamic,

    /// For endpoints which rarely change for a given URL.
    /// E.g., a fixed segment of video. The underlying video logically never changes; there may
    /// rarely be some software change to the actual bytes (which would result in a new etag) so
    /// (unlike the content-hashed static content) it's not entirely immutable.
    PrivateStatic,

    None,
}

impl Service {
    pub fn new(config: Config) -> Result<Self, Error> {
        let mut ui_dir = None;
        if let Some(d) = config.ui_dir {
            match FsDir::builder().for_path(&d) {
                Err(e) => {
                    warn!(
                        "Unable to load --ui-dir={}; will serve no static files: {}",
                        d.display(),
                        e
                    );
                }
                Ok(d) => ui_dir = Some(d),
            };
        }
        let dirs_by_stream_id = {
            let l = config.db.lock();
            let mut d =
                FnvHashMap::with_capacity_and_hasher(l.streams_by_id().len(), Default::default());
            for (&id, s) in l.streams_by_id().iter() {
                let dir_id = match s.sample_file_dir_id {
                    Some(d) => d,
                    None => continue,
                };
                d.insert(id, l.sample_file_dirs_by_id().get(&dir_id).unwrap().get()?);
            }
            Arc::new(d)
        };

        Ok(Service {
            db: config.db,
            dirs_by_stream_id,
            ui_dir,
            allow_unauthenticated_permissions: config.allow_unauthenticated_permissions,
            trust_forward_hdrs: config.trust_forward_hdrs,
            time_zone_name: config.time_zone_name,
        })
    }

    fn stream_live_m4s(
        self: Arc<Self>,
        req: Request<::hyper::Body>,
        caller: Caller,
        uuid: Uuid,
        stream_type: db::StreamType,
    ) -> ResponseResult {
        if !caller.permissions.view_video {
            return Err(plain_response(
                StatusCode::UNAUTHORIZED,
                "view_video required",
            ));
        }

        let stream_id;
        let open_id;
        let (sub_tx, sub_rx) = futures::channel::mpsc::unbounded();
        {
            let mut db = self.db.lock();
            open_id = match db.open {
                None => {
                    return Err(plain_response(
                        StatusCode::PRECONDITION_FAILED,
                        "database is read-only; there are no live streams",
                    ))
                }
                Some(o) => o.id,
            };
            let camera = db.get_camera(uuid).ok_or_else(|| {
                plain_response(StatusCode::NOT_FOUND, format!("no such camera {}", uuid))
            })?;
            stream_id = camera.streams[stream_type.index()].ok_or_else(|| {
                plain_response(
                    StatusCode::NOT_FOUND,
                    format!("no such stream {}/{}", uuid, stream_type),
                )
            })?;
            db.watch_live(
                stream_id,
                Box::new(move |l| sub_tx.unbounded_send(l).is_ok()),
            )
            .expect("stream_id refed by camera");
        }

        let response =
            tungstenite::handshake::server::create_response_with_body(&req, hyper::Body::empty)
                .map_err(|e| bad_req(e.to_string()))?;
        let (parts, _) = response.into_parts();

        tokio::spawn(self.stream_live_m4s_ws(stream_id, open_id, req, sub_rx));

        Ok(Response::from_parts(parts, Body::from("")))
    }

    async fn stream_live_m4s_ws(
        self: Arc<Self>,
        stream_id: i32,
        open_id: u32,
        req: hyper::Request<hyper::Body>,
        mut sub_rx: futures::channel::mpsc::UnboundedReceiver<db::LiveSegment>,
    ) {
        let upgraded = match hyper::upgrade::on(req).await {
            Ok(u) => u,
            Err(e) => {
                warn!("Unable to upgrade stream to websocket: {}", e);
                return;
            }
        };
        let mut ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
            upgraded,
            tungstenite::protocol::Role::Server,
            None,
        )
        .await;

        // Start the first segment at a key frame to reduce startup latency.
        let mut start_at_key = true;
        loop {
            let live = match sub_rx.next().await {
                Some(l) => l,
                None => return,
            };

            info!("chunk: is_key={:?}", live.is_key);
            if let Err(e) = self
                .stream_live_m4s_chunk(open_id, stream_id, &mut ws, live, start_at_key)
                .await
            {
                info!("Dropping WebSocket after error: {}", e);
                return;
            }
            start_at_key = false;
        }
    }

    async fn stream_live_m4s_chunk(
        &self,
        open_id: u32,
        stream_id: i32,
        ws: &mut tokio_tungstenite::WebSocketStream<hyper::upgrade::Upgraded>,
        live: db::LiveSegment,
        start_at_key: bool,
    ) -> Result<(), Error> {
        let mut builder = mp4::FileBuilder::new(mp4::Type::MediaSegment);
        let mut row = None;
        {
            let db = self.db.lock();
            let mut rows = 0;
            db.list_recordings_by_id(stream_id, live.recording..live.recording + 1, &mut |r| {
                rows += 1;
                row = Some(r);
                builder.append(&db, r, live.media_off_90k.clone(), start_at_key)?;
                Ok(())
            })?;
            if rows != 1 {
                bail_t!(Internal, "unable to find {:?}", live);
            }
        }
        let row = row.unwrap();
        use http_serve::Entity;
        let mp4 = builder.build(self.db.clone(), self.dirs_by_stream_id.clone())?;
        let mut hdrs = header::HeaderMap::new();
        mp4.add_headers(&mut hdrs);
        let mime_type = hdrs.get(header::CONTENT_TYPE).unwrap();
        let (prev_media_duration, prev_runs) = row.prev_media_duration_and_runs.unwrap();
        let hdr = format!(
            "Content-Type: {}\r\n\
            X-Recording-Start: {}\r\n\
            X-Recording-Id: {}.{}\r\n\
            X-Media-Time-Range: {}-{}\r\n\
            X-Prev-Media-Duration: {}\r\n\
            X-Runs: {}\r\n\
            X-Video-Sample-Entry-Id: {}\r\n\r\n",
            mime_type.to_str().unwrap(),
            row.start.0,
            open_id,
            live.recording,
            live.media_off_90k.start,
            live.media_off_90k.end,
            prev_media_duration.0,
            prev_runs + if row.run_offset == 0 { 1 } else { 0 },
            &row.video_sample_entry_id
        );
        let mut v = hdr.into_bytes();
        mp4.append_into_vec(&mut v).await?;
        ws.send(tungstenite::Message::Binary(v)).await?;
        Ok(())
    }

    async fn signals(&self, req: Request<hyper::Body>, caller: Caller) -> ResponseResult {
        use http::method::Method;
        match *req.method() {
            Method::POST => self.post_signals(req, caller).await,
            Method::GET | Method::HEAD => self.get_signals(&req),
            _ => Err(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "POST, GET, or HEAD expected",
            )),
        }
    }

    async fn serve_inner(
        self: Arc<Self>,
        req: Request<::hyper::Body>,
        p: Path,
        caller: Caller,
    ) -> ResponseResult {
        let (cache, mut response) = match p {
            Path::InitSegment(sha1, debug) => (
                CacheControl::PrivateStatic,
                self.init_segment(sha1, debug, &req)?,
            ),
            Path::TopLevel => (CacheControl::PrivateDynamic, self.top_level(&req, caller)?),
            Path::Request => (CacheControl::PrivateDynamic, self.request(&req)?),
            Path::Camera(uuid) => (CacheControl::PrivateDynamic, self.camera(&req, uuid)?),
            Path::StreamRecordings(uuid, type_) => (
                CacheControl::PrivateDynamic,
                self.stream_recordings(&req, uuid, type_)?,
            ),
            Path::StreamViewMp4(uuid, type_, debug) => (
                CacheControl::PrivateStatic,
                self.stream_view_mp4(&req, caller, uuid, type_, mp4::Type::Normal, debug)?,
            ),
            Path::StreamViewMp4Segment(uuid, type_, debug) => (
                CacheControl::PrivateStatic,
                self.stream_view_mp4(&req, caller, uuid, type_, mp4::Type::MediaSegment, debug)?,
            ),
            Path::StreamLiveMp4Segments(uuid, type_) => (
                CacheControl::PrivateDynamic,
                self.stream_live_m4s(req, caller, uuid, type_)?,
            ),
            Path::NotFound => return Err(not_found("path not understood")),
            Path::Login => (CacheControl::PrivateDynamic, self.login(req).await?),
            Path::Logout => (CacheControl::PrivateDynamic, self.logout(req).await?),
            Path::Signals => (
                CacheControl::PrivateDynamic,
                self.signals(req, caller).await?,
            ),
            Path::Static => (CacheControl::None, self.static_file(req).await?),
        };
        match cache {
            CacheControl::PrivateStatic => {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("private, max-age=3600"),
                );
            }
            CacheControl::PrivateDynamic => {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("private, no-cache"),
                );
            }
            CacheControl::None => {}
        }
        Ok(response)
    }

    pub async fn serve(
        self: Arc<Self>,
        req: Request<::hyper::Body>,
    ) -> Result<Response<Body>, std::convert::Infallible> {
        let p = Path::decode(req.uri().path());
        let always_allow_unauthenticated = match p {
            Path::NotFound | Path::Request | Path::Login | Path::Logout | Path::Static => true,
            _ => false,
        };
        debug!("request on: {}: {:?}", req.uri(), p);
        let caller = match self.authenticate(&req, always_allow_unauthenticated) {
            Ok(c) => c,
            Err(e) => return Ok(from_base_error(e)),
        };
        Ok(self.serve_inner(req, p, caller).await.unwrap_or_else(|e| e))
    }

    fn top_level(&self, req: &Request<::hyper::Body>, caller: Caller) -> ResponseResult {
        let mut days = false;
        let mut camera_configs = false;
        if let Some(q) = req.uri().query() {
            for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                let (key, value): (_, &str) = (key.borrow(), value.borrow());
                match key {
                    "days" => days = value == "true",
                    "cameraConfigs" => camera_configs = value == "true",
                    _ => {}
                };
            }
        }

        if camera_configs {
            if !caller.permissions.read_camera_configs {
                return Err(plain_response(
                    StatusCode::UNAUTHORIZED,
                    "read_camera_configs required",
                ));
            }
        }

        let db = self.db.lock();
        serve_json(
            req,
            &json::TopLevel {
                time_zone_name: &self.time_zone_name,
                cameras: (&db, days, camera_configs),
                session: caller.session,
                signals: (&db, days),
                signal_types: &db,
            },
        )
    }

    fn camera(&self, req: &Request<::hyper::Body>, uuid: Uuid) -> ResponseResult {
        let db = self.db.lock();
        let camera = db
            .get_camera(uuid)
            .ok_or_else(|| not_found(format!("no such camera {}", uuid)))?;
        serve_json(
            req,
            &json::Camera::wrap(camera, &db, true, false).map_err(internal_server_err)?,
        )
    }

    fn stream_recordings(
        &self,
        req: &Request<::hyper::Body>,
        uuid: Uuid,
        type_: db::StreamType,
    ) -> ResponseResult {
        let (r, split) = {
            let mut time = recording::Time::min_value()..recording::Time::max_value();
            let mut split = recording::Duration(i64::max_value());
            if let Some(q) = req.uri().query() {
                for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                    let (key, value) = (key.borrow(), value.borrow());
                    match key {
                        "startTime90k" => {
                            time.start = recording::Time::parse(value)
                                .map_err(|_| bad_req("unparseable startTime90k"))?
                        }
                        "endTime90k" => {
                            time.end = recording::Time::parse(value)
                                .map_err(|_| bad_req("unparseable endTime90k"))?
                        }
                        "split90k" => {
                            split = recording::Duration(
                                i64::from_str(value)
                                    .map_err(|_| bad_req("unparseable split90k"))?,
                            )
                        }
                        _ => {}
                    }
                }
            }
            (time, split)
        };
        let db = self.db.lock();
        let mut out = json::ListRecordings {
            recordings: Vec::new(),
            video_sample_entries: (&db, Vec::new()),
        };
        let camera = db.get_camera(uuid).ok_or_else(|| {
            plain_response(StatusCode::NOT_FOUND, format!("no such camera {}", uuid))
        })?;
        let stream_id = camera.streams[type_.index()].ok_or_else(|| {
            plain_response(
                StatusCode::NOT_FOUND,
                format!("no such stream {}/{}", uuid, type_),
            )
        })?;
        db.list_aggregated_recordings(stream_id, r, split, &mut |row| {
            let end = row.ids.end - 1; // in api, ids are inclusive.
            out.recordings.push(json::Recording {
                start_id: row.ids.start,
                end_id: if end == row.ids.start {
                    None
                } else {
                    Some(end)
                },
                start_time_90k: row.time.start.0,
                end_time_90k: row.time.end.0,
                sample_file_bytes: row.sample_file_bytes,
                open_id: row.open_id,
                first_uncommitted: row.first_uncommitted,
                video_samples: row.video_samples,
                video_sample_entry_id: row.video_sample_entry_id,
                growing: row.growing,
            });
            if !out
                .video_sample_entries
                .1
                .contains(&row.video_sample_entry_id)
            {
                out.video_sample_entries.1.push(row.video_sample_entry_id);
            }
            Ok(())
        })
        .map_err(internal_server_err)?;
        serve_json(req, &out)
    }

    fn init_segment(&self, id: i32, debug: bool, req: &Request<::hyper::Body>) -> ResponseResult {
        let mut builder = mp4::FileBuilder::new(mp4::Type::InitSegment);
        let db = self.db.lock();
        let ent = db
            .video_sample_entries_by_id()
            .get(&id)
            .ok_or_else(|| not_found("not such init segment"))?;
        builder.append_video_sample_entry(ent.clone());
        let mp4 = builder
            .build(self.db.clone(), self.dirs_by_stream_id.clone())
            .map_err(from_base_error)?;
        if debug {
            Ok(plain_response(StatusCode::OK, format!("{:#?}", mp4)))
        } else {
            Ok(http_serve::serve(mp4, req))
        }
    }

    fn stream_view_mp4(
        &self,
        req: &Request<::hyper::Body>,
        caller: Caller,
        uuid: Uuid,
        stream_type: db::StreamType,
        mp4_type: mp4::Type,
        debug: bool,
    ) -> ResponseResult {
        if !caller.permissions.view_video {
            return Err(plain_response(
                StatusCode::UNAUTHORIZED,
                "view_video required",
            ));
        }
        let (stream_id, camera_name);
        {
            let db = self.db.lock();
            let camera = db.get_camera(uuid).ok_or_else(|| {
                plain_response(StatusCode::NOT_FOUND, format!("no such camera {}", uuid))
            })?;
            camera_name = camera.short_name.clone();
            stream_id = camera.streams[stream_type.index()].ok_or_else(|| {
                plain_response(
                    StatusCode::NOT_FOUND,
                    format!("no such stream {}/{}", uuid, stream_type),
                )
            })?;
        };
        let mut start_time_for_filename = None;
        let mut builder = mp4::FileBuilder::new(mp4_type);
        if let Some(q) = req.uri().query() {
            for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                let (key, value) = (key.borrow(), value.borrow());
                match key {
                    "s" => {
                        let s = Segments::from_str(value).map_err(|()| {
                            plain_response(
                                StatusCode::BAD_REQUEST,
                                format!("invalid s parameter: {}", value),
                            )
                        })?;
                        debug!("stream_view_mp4: appending s={:?}", s);
                        let mut est_segments = usize::try_from(s.ids.end - s.ids.start).unwrap();
                        if let Some(end) = s.end_time {
                            // There should be roughly ceil((end - start) /
                            // desired_recording_duration) recordings in the desired timespan if
                            // there are no gaps or overlap, possibly another for misalignment of
                            // the requested timespan with the rotate offset and another because
                            // rotation only happens at key frames.
                            let ceil_durations = (end - s.start_time
                                + recording::DESIRED_RECORDING_WALL_DURATION
                                - 1)
                                / recording::DESIRED_RECORDING_WALL_DURATION;
                            est_segments = cmp::min(est_segments, (ceil_durations + 2) as usize);
                        }
                        builder.reserve(est_segments);
                        let db = self.db.lock();
                        let mut prev = None; // previous recording id
                        let mut cur_off = 0;
                        db.list_recordings_by_id(stream_id, s.ids.clone(), &mut |r| {
                            let recording_id = r.id.recording();

                            if let Some(o) = s.open_id {
                                if r.open_id != o {
                                    bail!(
                                        "recording {} has open id {}, requested {}",
                                        r.id,
                                        r.open_id,
                                        o
                                    );
                                }
                            }

                            // Check for missing recordings.
                            match prev {
                                None if recording_id == s.ids.start => {}
                                None => bail!("no such recording {}/{}", stream_id, s.ids.start),
                                Some(id) if r.id.recording() != id + 1 => {
                                    bail!("no such recording {}/{}", stream_id, id + 1);
                                }
                                _ => {}
                            };
                            prev = Some(recording_id);

                            // Add a segment for the relevant part of the recording, if any.
                            // Note all calculations here are in wall times / wall durations.
                            let end_time = s.end_time.unwrap_or(i64::max_value());
                            let wd = i64::from(r.wall_duration_90k);
                            if s.start_time <= cur_off + wd && cur_off < end_time {
                                let start = cmp::max(0, s.start_time - cur_off);
                                let end = cmp::min(wd, end_time - cur_off);
                                let wr = i32::try_from(start).unwrap()..i32::try_from(end).unwrap();
                                debug!(
                                    "...appending recording {} with wall duration {:?} \
                                       (out of total {})",
                                    r.id, wr, wd
                                );
                                if start_time_for_filename.is_none() {
                                    start_time_for_filename =
                                        Some(r.start + recording::Duration(start));
                                }
                                use recording::rescale;
                                let mr =
                                    rescale(wr.start, r.wall_duration_90k, r.media_duration_90k)
                                        ..rescale(
                                            wr.end,
                                            r.wall_duration_90k,
                                            r.media_duration_90k,
                                        );
                                builder.append(&db, r, mr, true)?;
                            } else {
                                debug!("...skipping recording {} wall dur {}", r.id, wd);
                            }
                            cur_off += wd;
                            Ok(())
                        })
                        .map_err(internal_server_err)?;

                        // Check for missing recordings.
                        match prev {
                            Some(id) if s.ids.end != id + 1 => {
                                return Err(not_found(format!(
                                    "no such recording {}/{}",
                                    stream_id,
                                    s.ids.end - 1
                                )));
                            }
                            None => {
                                return Err(not_found(format!(
                                    "no such recording {}/{}",
                                    stream_id, s.ids.start
                                )));
                            }
                            _ => {}
                        };
                        if let Some(end) = s.end_time {
                            if end > cur_off {
                                return Err(plain_response(
                                    StatusCode::BAD_REQUEST,
                                    format!("end time {} is beyond specified recordings", end),
                                ));
                            }
                        }
                    }
                    "ts" => builder
                        .include_timestamp_subtitle_track(value == "true")
                        .map_err(from_base_error)?,
                    _ => return Err(bad_req(format!("parameter {} not understood", key))),
                }
            }
        }
        if let Some(start) = start_time_for_filename {
            let tm = time::at(time::Timespec {
                sec: start.unix_seconds(),
                nsec: 0,
            });
            let stream_abbrev = if stream_type == db::StreamType::MAIN {
                "main"
            } else {
                "sub"
            };
            let suffix = if mp4_type == mp4::Type::Normal {
                "mp4"
            } else {
                "m4s"
            };
            builder
                .set_filename(&format!(
                    "{}-{}-{}.{}",
                    tm.strftime("%Y%m%d%H%M%S").unwrap(),
                    camera_name,
                    stream_abbrev,
                    suffix
                ))
                .map_err(from_base_error)?;
        }
        let mp4 = builder
            .build(self.db.clone(), self.dirs_by_stream_id.clone())
            .map_err(from_base_error)?;
        if debug {
            return Ok(plain_response(StatusCode::OK, format!("{:#?}", mp4)));
        }
        Ok(http_serve::serve(mp4, req))
    }

    async fn static_file(&self, req: Request<hyper::Body>) -> ResponseResult {
        let dir = self
            .ui_dir
            .clone()
            .ok_or_else(|| not_found("--ui-dir not configured; no static files available."))?;
        let static_req = match StaticFileRequest::parse(req.uri().path()) {
            None => return Err(not_found("static file not found")),
            Some(r) => r,
        };
        let f = dir.get(static_req.path, req.headers());
        let node = f.await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                not_found("no such static file")
            } else {
                internal_server_err(e)
            }
        })?;
        let mut hdrs = http::HeaderMap::new();
        node.add_encoding_headers(&mut hdrs);
        hdrs.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(if static_req.immutable {
                // https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Cache-Control#Caching_static_assets
                "public, max-age=604800, immutable"
            } else {
                "public"
            }),
        );
        hdrs.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(static_req.mime),
        );
        let e = node.into_file_entity(hdrs).map_err(internal_server_err)?;
        Ok(http_serve::serve(e, &req))
    }

    fn authreq(&self, req: &Request<::hyper::Body>) -> auth::Request {
        auth::Request {
            when_sec: Some(self.db.clocks().realtime().sec),
            addr: if self.trust_forward_hdrs {
                req.headers()
                    .get("X-Real-IP")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| IpAddr::from_str(v).ok())
            } else {
                None
            },
            user_agent: req
                .headers()
                .get(header::USER_AGENT)
                .map(|ua| ua.as_bytes().to_vec()),
        }
    }

    fn request(&self, req: &Request<::hyper::Body>) -> ResponseResult {
        let authreq = self.authreq(req);
        let host = req
            .headers()
            .get(header::HOST)
            .map(|h| String::from_utf8_lossy(h.as_bytes()));
        let agent = authreq
            .user_agent
            .as_ref()
            .map(|u| String::from_utf8_lossy(&u[..]));
        Ok(plain_response(
            StatusCode::OK,
            format!(
                "when: {}\n\
                    host: {:?}\n\
                    addr: {:?}\n\
                    user_agent: {:?}\n\
                    secure: {:?}",
                time::at(time::Timespec {
                    sec: authreq.when_sec.unwrap(),
                    nsec: 0
                })
                .strftime("%FT%T")
                .map(|f| f.to_string())
                .unwrap_or_else(|e| e.to_string()),
                host.as_ref().map(|h| &*h),
                &authreq.addr,
                agent.as_ref().map(|a| &*a),
                self.is_secure(req)
            ),
        ))
    }

    fn is_secure(&self, req: &Request<::hyper::Body>) -> bool {
        self.trust_forward_hdrs
            && req
                .headers()
                .get("X-Forwarded-Proto")
                .map(|v| v.as_bytes() == b"https")
                .unwrap_or(false)
    }

    async fn login(&self, mut req: Request<::hyper::Body>) -> ResponseResult {
        let r = extract_json_body(&mut req).await?;
        let r: json::LoginRequest =
            serde_json::from_slice(&r).map_err(|e| bad_req(e.to_string()))?;
        let authreq = self.authreq(&req);
        let host = req
            .headers()
            .get(header::HOST)
            .ok_or_else(|| bad_req("missing Host header!"))?;
        let host = host.as_bytes();
        let domain = match memchr(b':', host) {
            Some(colon) => &host[0..colon],
            None => host,
        }
        .to_owned();
        let mut l = self.db.lock();
        let is_secure = self.is_secure(&req);
        let flags = (auth::SessionFlag::HttpOnly as i32)
            | (auth::SessionFlag::SameSite as i32)
            | (auth::SessionFlag::SameSiteStrict as i32)
            | if is_secure {
                auth::SessionFlag::Secure as i32
            } else {
                0
            };
        let (sid, _) = l
            .login_by_password(authreq, &r.username, r.password, Some(domain), flags)
            .map_err(|e| plain_response(StatusCode::UNAUTHORIZED, e.to_string()))?;
        let s_suffix = if is_secure {
            &b"; HttpOnly; Secure; SameSite=Strict; Max-Age=2147483648; Path=/"[..]
        } else {
            &b"; HttpOnly; SameSite=Strict; Max-Age=2147483648; Path=/"[..]
        };
        let mut encoded = [0u8; 64];
        base64::encode_config_slice(&sid, base64::STANDARD_NO_PAD, &mut encoded);
        let mut cookie = BytesMut::with_capacity("s=".len() + encoded.len() + s_suffix.len());
        cookie.put(&b"s="[..]);
        cookie.put(&encoded[..]);
        cookie.put(s_suffix);
        Ok(Response::builder()
            .header(
                header::SET_COOKIE,
                HeaderValue::from_maybe_shared(cookie.freeze())
                    .expect("cookie can't have invalid bytes"),
            )
            .status(StatusCode::NO_CONTENT)
            .body(b""[..].into())
            .unwrap())
    }

    async fn logout(&self, mut req: Request<hyper::Body>) -> ResponseResult {
        let r = extract_json_body(&mut req).await?;
        let r: json::LogoutRequest =
            serde_json::from_slice(&r).map_err(|e| bad_req(e.to_string()))?;

        let mut res = Response::new(b""[..].into());
        if let Some(sid) = extract_sid(&req) {
            let authreq = self.authreq(&req);
            let mut l = self.db.lock();
            let hash = sid.hash();
            let need_revoke = match l.authenticate_session(authreq.clone(), &hash) {
                Ok((s, _)) => {
                    if !csrf_matches(r.csrf, s.csrf()) {
                        warn!("logout request with missing/incorrect csrf");
                        return Err(bad_req("logout with incorrect csrf token"));
                    }
                    info!("revoking session");
                    true
                }
                Err(e) => {
                    // TODO: distinguish "no such session", "session is no longer valid", and
                    // "user ... is disabled" (which are all client error / bad state) from database
                    // errors.
                    warn!("logout failed: {}", e);
                    false
                }
            };
            if need_revoke {
                // TODO: inline this above with non-lexical lifetimes.
                l.revoke_session(auth::RevocationReason::LoggedOut, None, authreq, &hash)
                    .map_err(internal_server_err)?;
            }

            // By now the session is invalid (whether it was valid to start with or not).
            // Clear useless cookie.
            res.headers_mut().append(
                header::SET_COOKIE,
                HeaderValue::from_str("s=; Max-Age=0; Path=/").unwrap(),
            );
        }
        *res.status_mut() = StatusCode::NO_CONTENT;
        Ok(res)
    }

    async fn post_signals(&self, mut req: Request<hyper::Body>, caller: Caller) -> ResponseResult {
        if !caller.permissions.update_signals {
            return Err(plain_response(
                StatusCode::UNAUTHORIZED,
                "update_signals required",
            ));
        }
        let r = extract_json_body(&mut req).await?;
        let r: json::PostSignalsRequest =
            serde_json::from_slice(&r).map_err(|e| bad_req(e.to_string()))?;
        let mut l = self.db.lock();
        let now = recording::Time::new(self.db.clocks().realtime());
        let start = r.start_time_90k.map(recording::Time).unwrap_or(now);
        let end = match r.end_base {
            json::PostSignalsEndBase::Epoch => {
                recording::Time(r.rel_end_time_90k.ok_or_else(|| {
                    bad_req("must specify rel_end_time_90k when end_base is epoch")
                })?)
            }
            json::PostSignalsEndBase::Now => {
                now + recording::Duration(r.rel_end_time_90k.unwrap_or(0))
            }
        };
        l.update_signals(start..end, &r.signal_ids, &r.states)
            .map_err(from_base_error)?;
        serve_json(&req, &json::PostSignalsResponse { time_90k: now.0 })
    }

    fn get_signals(&self, req: &Request<hyper::Body>) -> ResponseResult {
        let mut time = recording::Time::min_value()..recording::Time::max_value();
        if let Some(q) = req.uri().query() {
            for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                let (key, value) = (key.borrow(), value.borrow());
                match key {
                    "startTime90k" => {
                        time.start = recording::Time::parse(value)
                            .map_err(|_| bad_req("unparseable startTime90k"))?
                    }
                    "endTime90k" => {
                        time.end = recording::Time::parse(value)
                            .map_err(|_| bad_req("unparseable endTime90k"))?
                    }
                    _ => {}
                }
            }
        }

        let mut signals = json::Signals::default();
        self.db
            .lock()
            .list_changes_by_time(time, &mut |c: &db::signal::ListStateChangesRow| {
                signals.times_90k.push(c.when.0);
                signals.signal_ids.push(c.signal);
                signals.states.push(c.state);
            });
        serve_json(req, &signals)
    }

    fn authenticate(
        &self,
        req: &Request<hyper::Body>,
        unauth_path: bool,
    ) -> Result<Caller, base::Error> {
        if let Some(sid) = extract_sid(req) {
            let authreq = self.authreq(req);

            // TODO: real error handling! this assumes all errors are due to lack of
            // authentication, when they could be logic errors in SQL or such.
            if let Ok((s, u)) = self
                .db
                .lock()
                .authenticate_session(authreq.clone(), &sid.hash())
            {
                return Ok(Caller {
                    permissions: s.permissions.clone(),
                    session: Some(json::Session {
                        username: u.username.clone(),
                        csrf: s.csrf(),
                    }),
                });
            }
            info!("authenticate_session failed");
        }

        if let Some(s) = self.allow_unauthenticated_permissions.as_ref() {
            return Ok(Caller {
                permissions: s.clone(),
                session: None,
            });
        }

        if unauth_path {
            return Ok(Caller {
                permissions: db::Permissions::default(),
                session: None,
            });
        }

        bail_t!(Unauthenticated, "unauthenticated");
    }
}

#[derive(Debug, Eq, PartialEq)]
struct StaticFileRequest<'a> {
    path: &'a str,
    immutable: bool,
    mime: &'static str,
}

impl<'a> StaticFileRequest<'a> {
    fn parse(path: &'a str) -> Option<Self> {
        if !path.starts_with("/") {
            return None;
        }

        let (path, immutable) = match &path[1..] {
            "" => ("index.html", false),
            p => (p, true),
        };

        let last_dot = match path.rfind('.') {
            None => return None,
            Some(d) => d,
        };
        let ext = &path[last_dot + 1..];
        let mime = match ext {
            "html" => "text/html",
            "ico" => "image/x-icon",
            "js" | "map" => "text/javascript",
            "json" => "application/json",
            "png" => "image/png",
            "webapp" => "application/x-web-app-manifest+json",
            _ => return None,
        };

        Some(StaticFileRequest {
            path,
            immutable,
            mime,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Segments, StaticFileRequest};
    use db::testutil::{self, TestDb};
    use futures::future::FutureExt;
    use log::info;
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::Arc;

    struct Server {
        db: TestDb<base::clock::RealClocks>,
        base_url: String,
        //test_camera_uuid: Uuid,
        handle: Option<::std::thread::JoinHandle<()>>,
        shutdown_tx: Option<futures::channel::oneshot::Sender<()>>,
    }

    impl Server {
        fn new(allow_unauthenticated_permissions: Option<db::Permissions>) -> Server {
            let db = TestDb::new(base::clock::RealClocks {});
            let (shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel::<()>();
            let service = Arc::new(
                super::Service::new(super::Config {
                    db: db.db.clone(),
                    ui_dir: None,
                    allow_unauthenticated_permissions,
                    trust_forward_hdrs: true,
                    time_zone_name: "".to_owned(),
                })
                .unwrap(),
            );
            let make_svc = hyper::service::make_service_fn(move |_conn| {
                futures::future::ok::<_, std::convert::Infallible>(hyper::service::service_fn({
                    let s = Arc::clone(&service);
                    move |req| Arc::clone(&s).serve(req)
                }))
            });
            let (tx, rx) = std::sync::mpsc::channel();
            let handle = ::std::thread::spawn(move || {
                let addr = ([127, 0, 0, 1], 0).into();
                let rt = tokio::runtime::Runtime::new().unwrap();
                let srv = {
                    let _guard = rt.enter();
                    hyper::server::Server::bind(&addr)
                        .tcp_nodelay(true)
                        .serve(make_svc)
                };
                let addr = srv.local_addr(); // resolve port 0 to a real ephemeral port number.
                tx.send(addr).unwrap();
                rt.block_on(srv.with_graceful_shutdown(shutdown_rx.map(|_| ())))
                    .unwrap();
            });
            let addr = rx.recv().unwrap();

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
        pub fn new(headers: &reqwest::header::HeaderMap) -> Self {
            let mut c = SessionCookie::default();
            c.update(headers);
            c
        }

        pub fn update(&mut self, headers: &reqwest::header::HeaderMap) {
            for set_cookie in headers.get_all(reqwest::header::SET_COOKIE) {
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
        assert_eq!(
            Path::decode("/api/init/42.mp4"),
            Path::InitSegment(42, false)
        );
        assert_eq!(
            Path::decode("/api/init/42.mp4.txt"),
            Path::InitSegment(42, true)
        );
        assert_eq!(Path::decode("/api/init/x.mp4"), Path::NotFound); // non-digit
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/"),
            Path::Camera(cam_uuid)
        );
        assert_eq!(Path::decode("/api/cameras/asdf/"), Path::NotFound);
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/recordings"),
            Path::StreamRecordings(cam_uuid, db::StreamType::MAIN)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/sub/recordings"),
            Path::StreamRecordings(cam_uuid, db::StreamType::SUB)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/junk/recordings"),
            Path::NotFound
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/view.mp4"),
            Path::StreamViewMp4(cam_uuid, db::StreamType::MAIN, false)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/view.mp4.txt"),
            Path::StreamViewMp4(cam_uuid, db::StreamType::MAIN, true)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/view.m4s"),
            Path::StreamViewMp4Segment(cam_uuid, db::StreamType::MAIN, false)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/view.m4s.txt"),
            Path::StreamViewMp4Segment(cam_uuid, db::StreamType::MAIN, true)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/live.m4s"),
            Path::StreamLiveMp4Segments(cam_uuid, db::StreamType::MAIN)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/junk"),
            Path::NotFound
        );
        assert_eq!(Path::decode("/api/login"), Path::Login);
        assert_eq!(Path::decode("/api/logout"), Path::Logout);
        assert_eq!(Path::decode("/api/signals"), Path::Signals);
        assert_eq!(Path::decode("/api/junk"), Path::NotFound);
    }

    #[test]
    fn static_file() {
        testutil::init();
        let r = StaticFileRequest::parse("/jquery-ui.b6d3d46c828800e78499.js").unwrap();
        assert_eq!(
            r,
            StaticFileRequest {
                path: "jquery-ui.b6d3d46c828800e78499.js",
                mime: "text/javascript",
                immutable: true,
            }
        );

        let r = StaticFileRequest::parse("/").unwrap();
        assert_eq!(
            r,
            StaticFileRequest {
                path: "index.html",
                mime: "text/html",
                immutable: false,
            }
        );
    }

    #[test]
    #[rustfmt::skip]
    fn test_segments() {
        testutil::init();
        assert_eq!(
            Segments { ids: 1..2, open_id: None, start_time: 0, end_time: None },
            Segments::from_str("1").unwrap()
        );
        assert_eq!(
            Segments { ids: 1..2, open_id: Some(42), start_time: 0, end_time: None },
            Segments::from_str("1@42").unwrap()
        );
        assert_eq!(
            Segments { ids: 1..2, open_id: None, start_time: 26, end_time: None },
            Segments::from_str("1.26-").unwrap()
        );
        assert_eq!(
            Segments { ids: 1..2, open_id: Some(42), start_time: 26, end_time: None },
            Segments::from_str("1@42.26-").unwrap()
        );
        assert_eq!(
            Segments { ids: 1..2, open_id: None, start_time: 0, end_time: Some(42) },
            Segments::from_str("1.-42").unwrap()
        );
        assert_eq!(
            Segments { ids: 1..2, open_id: None, start_time: 26, end_time: Some(42) },
            Segments::from_str("1.26-42").unwrap()
        );
        assert_eq!(
            Segments { ids: 1..6, open_id: None, start_time: 0, end_time: None },
            Segments::from_str("1-5").unwrap()
        );
        assert_eq!(
            Segments { ids: 1..6, open_id: None, start_time: 26, end_time: None },
            Segments::from_str("1-5.26-").unwrap()
        );
        assert_eq!(
            Segments { ids: 1..6, open_id: None, start_time: 0, end_time: Some(42) },
            Segments::from_str("1-5.-42").unwrap()
        );
        assert_eq!(
            Segments { ids: 1..6, open_id: None, start_time: 26, end_time: Some(42) },
            Segments::from_str("1-5.26-42").unwrap()
        );
    }

    #[tokio::test]
    async fn unauthorized_without_cookie() {
        testutil::init();
        let s = Server::new(None);
        let cli = reqwest::Client::new();
        let resp = cli
            .get(&format!("{}/api/", &s.base_url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login() {
        testutil::init();
        let s = Server::new(None);
        let cli = reqwest::Client::new();
        let login_url = format!("{}/api/login", &s.base_url);

        let resp = cli.get(&login_url).send().await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);

        let resp = cli.post(&login_url).send().await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

        let mut p = HashMap::new();
        p.insert("username", "slamb");
        p.insert("password", "asdf");
        let resp = cli.post(&login_url).json(&p).send().await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

        p.insert("password", "hunter2");
        let resp = cli.post(&login_url).json(&p).send().await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
        let cookie = SessionCookie::new(resp.headers());
        info!("cookie: {:?}", cookie);
        info!("header: {}", cookie.header());

        let resp = cli
            .get(&format!("{}/api/", &s.base_url))
            .header(reqwest::header::COOKIE, cookie.header())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    }

    #[tokio::test]
    async fn logout() {
        testutil::init();
        let s = Server::new(None);
        let cli = reqwest::Client::new();
        let mut p = HashMap::new();
        p.insert("username", "slamb");
        p.insert("password", "hunter2");
        let resp = cli
            .post(&format!("{}/api/login", &s.base_url))
            .json(&p)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
        let cookie = SessionCookie::new(resp.headers());

        // A GET shouldn't work.
        let resp = cli
            .get(&format!("{}/api/logout", &s.base_url))
            .header(reqwest::header::COOKIE, cookie.header())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);

        // Neither should a POST without a csrf token.
        let resp = cli
            .post(&format!("{}/api/logout", &s.base_url))
            .header(reqwest::header::COOKIE, cookie.header())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

        // But it should work with the csrf token.
        // Retrieve that from the toplevel API request.
        let toplevel: serde_json::Value = cli
            .post(&format!("{}/api/", &s.base_url))
            .header(reqwest::header::COOKIE, cookie.header())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let csrf = toplevel
            .get("session")
            .unwrap()
            .get("csrf")
            .unwrap()
            .as_str();
        let mut p = HashMap::new();
        p.insert("csrf", csrf);
        let resp = cli
            .post(&format!("{}/api/logout", &s.base_url))
            .header(reqwest::header::COOKIE, cookie.header())
            .json(&p)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
        let mut updated_cookie = cookie.clone();
        updated_cookie.update(resp.headers());

        // The cookie should be cleared client-side.
        assert!(updated_cookie.0.is_none());

        // It should also be invalidated server-side.
        let resp = cli
            .get(&format!("{}/api/", &s.base_url))
            .header(reqwest::header::COOKIE, cookie.header())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn view_without_segments() {
        testutil::init();
        let mut permissions = db::Permissions::new();
        permissions.view_video = true;
        let s = Server::new(Some(permissions));
        let cli = reqwest::Client::new();
        let resp = cli
            .get(&format!(
                "{}/api/cameras/{}/main/view.mp4",
                &s.base_url, s.db.test_camera_uuid
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    }
}

#[cfg(all(test, feature = "nightly"))]
mod bench {
    extern crate test;

    use db::testutil::{self, TestDb};
    use hyper;
    use lazy_static::lazy_static;
    use std::sync::Arc;
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
            let service = Arc::new(
                super::Service::new(super::Config {
                    db: db.db.clone(),
                    ui_dir: None,
                    allow_unauthenticated_permissions: Some(db::Permissions::default()),
                    trust_forward_hdrs: false,
                    time_zone_name: "".to_owned(),
                })
                .unwrap(),
            );
            let make_svc = hyper::service::make_service_fn(move |_conn| {
                futures::future::ok::<_, std::convert::Infallible>(hyper::service::service_fn({
                    let s = Arc::clone(&service);
                    move |req| Arc::clone(&s).serve(req)
                }))
            });
            let rt = tokio::runtime::Runtime::new().unwrap();
            let srv = {
                let _guard = rt.enter();
                let addr = ([127, 0, 0, 1], 0).into();
                hyper::server::Server::bind(&addr)
                    .tcp_nodelay(true)
                    .serve(make_svc)
            };
            let addr = srv.local_addr(); // resolve port 0 to a real ephemeral port number.
            ::std::thread::spawn(move || {
                rt.block_on(srv).unwrap();
            });
            Server {
                base_url: format!("http://{}:{}", addr.ip(), addr.port()),
                test_camera_uuid,
            }
        }
    }

    lazy_static! {
        static ref SERVER: Server = Server::new();
    }

    #[bench]
    fn serve_stream_recordings(b: &mut test::Bencher) {
        testutil::init();
        let server = &*SERVER;
        let url = reqwest::Url::parse(&format!(
            "{}/api/cameras/{}/main/recordings",
            server.base_url, server.test_camera_uuid
        ))
        .unwrap();
        let client = reqwest::Client::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let f = || {
            rt.block_on(async {
                let resp = client.get(url.clone()).send().await.unwrap();
                assert_eq!(resp.status(), reqwest::StatusCode::OK);
                let _b = resp.bytes().await.unwrap();
            });
        };
        f(); // warm.
        b.iter(f);
    }
}
