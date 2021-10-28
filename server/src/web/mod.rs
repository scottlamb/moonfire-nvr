// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

mod path;

use self::path::Path;
use crate::body::Body;
use crate::json;
use crate::mp4;
use base::{bail_t, ErrorKind};
use base::{clock::Clocks, format_err_t};
use core::borrow::Borrow;
use core::str::FromStr;
use db::dir::SampleFileDir;
use db::{auth, recording};
use failure::{format_err, Error};
use fnv::FnvHashMap;
use futures::stream::StreamExt;
use futures::{future::Either, sink::SinkExt};
use http::header::{self, HeaderValue};
use http::method::Method;
use http::{status::StatusCode, Request, Response};
use http_serve::dir::FsDir;
use hyper::body::Bytes;
use log::{debug, info, trace, warn};
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

/// An HTTP error response.
/// This is a thin wrapper over the hyper response type; it doesn't even verify
/// that the response actually uses a non-2xx status code. Its purpose is to
/// allow automatic conversion from `base::Error`. Rust's orphan rule prevents
/// this crate from defining a direct conversion from `base::Error` to
/// `hyper::Response`.
struct HttpError(Response<Body>);

impl From<Response<Body>> for HttpError {
    fn from(response: Response<Body>) -> Self {
        HttpError(response)
    }
}

impl From<base::Error> for HttpError {
    fn from(err: base::Error) -> Self {
        HttpError(from_base_error(err))
    }
}

fn plain_response<B: Into<Body>>(status: http::StatusCode, body: B) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))
        .body(body.into())
        .expect("hardcoded head should be valid")
}

fn not_found<B: Into<Body>>(body: B) -> HttpError {
    HttpError(plain_response(StatusCode::NOT_FOUND, body))
}

fn bad_req<B: Into<Body>>(body: B) -> HttpError {
    HttpError(plain_response(StatusCode::BAD_REQUEST, body))
}

fn internal_server_err<E: Into<Error>>(err: E) -> HttpError {
    HttpError(plain_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        err.into().to_string(),
    ))
}

fn from_base_error(err: base::Error) -> Response<Body> {
    use ErrorKind::*;
    let status_code = match err.kind() {
        Unauthenticated => StatusCode::UNAUTHORIZED,
        PermissionDenied => StatusCode::FORBIDDEN,
        InvalidArgument | FailedPrecondition => StatusCode::BAD_REQUEST,
        NotFound => StatusCode::NOT_FOUND,
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
    user: Option<json::ToplevelUser>,
}

type ResponseResult = Result<Response<Body>, HttpError>;

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
async fn extract_json_body(req: &mut Request<hyper::Body>) -> Result<Bytes, HttpError> {
    if *req.method() != Method::POST {
        return Err(plain_response(StatusCode::METHOD_NOT_ALLOWED, "POST expected").into());
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
            bail_t!(PermissionDenied, "view_video required");
        }

        let stream_id;
        let open_id;
        let (sub_tx, sub_rx) = futures::channel::mpsc::unbounded();
        {
            let mut db = self.db.lock();
            open_id = match db.open {
                None => {
                    bail_t!(
                        FailedPrecondition,
                        "database is read-only; there are no live streams"
                    );
                }
                Some(o) => o.id,
            };
            let camera = db.get_camera(uuid).ok_or_else(|| {
                plain_response(StatusCode::NOT_FOUND, format!("no such camera {}", uuid))
            })?;
            stream_id = camera.streams[stream_type.index()].ok_or_else(|| {
                format_err_t!(NotFound, "no such stream {}/{}", uuid, stream_type)
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
        sub_rx: futures::channel::mpsc::UnboundedReceiver<db::LiveSegment>,
    ) {
        let upgraded = match hyper::upgrade::on(req).await {
            Ok(u) => u,
            Err(e) => {
                warn!("Unable to upgrade stream to websocket: {}", e);
                return;
            }
        };
        let ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
            upgraded,
            tungstenite::protocol::Role::Server,
            None,
        )
        .await;

        if let Err(e) = self
            .stream_live_m4s_ws_loop(stream_id, open_id, sub_rx, ws)
            .await
        {
            info!("Dropping WebSocket after error: {}", e);
        }
    }

    /// Helper for `stream_live_m4s_ws` that returns error when the stream is dropped.
    /// The outer function logs the error.
    async fn stream_live_m4s_ws_loop(
        self: Arc<Self>,
        stream_id: i32,
        open_id: u32,
        sub_rx: futures::channel::mpsc::UnboundedReceiver<db::LiveSegment>,
        mut ws: tokio_tungstenite::WebSocketStream<hyper::upgrade::Upgraded>,
    ) -> Result<(), Error> {
        let keepalive = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(
            std::time::Duration::new(30, 0),
        ));
        let mut combo = futures::stream::select(
            sub_rx.map(Either::Left),
            keepalive.map(|_| Either::Right(())),
        );

        // On the first LiveSegment, send all the data from the previous key frame onward.
        // For LiveSegments, it's okay to send a single non-key frame at a time.
        let mut start_at_key = true;
        loop {
            let next = combo
                .next()
                .await
                .unwrap_or_else(|| unreachable!("timer stream never ends"));
            match next {
                Either::Left(live) => {
                    self.stream_live_m4s_chunk(open_id, stream_id, &mut ws, live, start_at_key)
                        .await?;
                    start_at_key = false;
                }
                Either::Right(_) => {
                    ws.send(tungstenite::Message::Ping(Vec::new())).await?;
                }
            }
        }
    }

    /// Sends a single live segment chunk of a `live.m4s` stream.
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
        match *req.method() {
            Method::POST => self.post_signals(req, caller).await,
            Method::GET | Method::HEAD => self.get_signals(&req),
            _ => Err(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "POST, GET, or HEAD expected",
            )
            .into()),
        }
    }

    /// Serves an HTTP request.
    /// Note that the `serve` wrapper handles responses the same whether they
    /// are `Ok` or `Err`. But returning `Err` here with the `?` operator is
    /// convenient for error paths.
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
            Path::User(id) => (
                CacheControl::PrivateDynamic,
                self.user(req, caller, id).await?,
            ),
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

    /// Serves an HTTP request.
    /// An error return from this method causes hyper to abruptly drop the
    /// HTTP connection rather than respond. That's not terribly useful, so this
    /// method always returns `Ok`. It delegates to a `serve_inner` which is
    /// allowed to generate `Err` results with the `?` operator, but returns
    /// them to hyper as `Ok` results.
    pub async fn serve(
        self: Arc<Self>,
        req: Request<::hyper::Body>,
    ) -> Result<Response<Body>, std::convert::Infallible> {
        let p = Path::decode(req.uri().path());
        let always_allow_unauthenticated = matches!(
            p,
            Path::NotFound | Path::Request | Path::Login | Path::Logout | Path::Static
        );
        debug!("request on: {}: {:?}", req.uri(), p);
        let caller = match self.authenticate(&req, always_allow_unauthenticated) {
            Ok(c) => c,
            Err(e) => return Ok(from_base_error(e)),
        };
        Ok(self
            .serve_inner(req, p, caller)
            .await
            .unwrap_or_else(|e| e.0))
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

        if camera_configs && !caller.permissions.read_camera_configs {
            bail_t!(PermissionDenied, "read_camera_configs required");
        }

        let db = self.db.lock();
        serve_json(
            req,
            &json::TopLevel {
                time_zone_name: &self.time_zone_name,
                server_version: env!("CARGO_PKG_VERSION"),
                cameras: (&db, days, camera_configs),
                user: caller.user,
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
                has_trailing_zero: row.has_trailing_zero,
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
            bail_t!(PermissionDenied, "view_video required");
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
                        trace!("stream_view_mp4: appending s={:?}", s);
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
                                    bail_t!(
                                        NotFound,
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
                                None => bail_t!(
                                    NotFound,
                                    "no such recording {}/{}",
                                    stream_id,
                                    s.ids.start
                                ),
                                Some(id) if r.id.recording() != id + 1 => {
                                    bail_t!(NotFound, "no such recording {}/{}", stream_id, id + 1);
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
                                trace!(
                                    "...appending recording {} with wall duration {:?} \
                                       (out of total {})",
                                    r.id,
                                    wr,
                                    wd
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
                                trace!("...skipping recording {} wall dur {}", r.id, wd);
                            }
                            cur_off += wd;
                            Ok(())
                        })?;

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
                                bail_t!(
                                    InvalidArgument,
                                    "end time {} is beyond specified recordings",
                                    end
                                );
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
            let stream_abbrev = if stream_type == db::StreamType::Main {
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

    async fn user(&self, req: Request<hyper::Body>, caller: Caller, id: i32) -> ResponseResult {
        if caller.user.map(|u| u.id) != Some(id) {
            bail_t!(Unauthenticated, "must be authenticated as supplied user");
        }
        match *req.method() {
            Method::POST => self.post_user(req, id).await,
            _ => Err(plain_response(StatusCode::METHOD_NOT_ALLOWED, "POST expected").into()),
        }
    }

    async fn post_user(&self, mut req: Request<hyper::Body>, id: i32) -> ResponseResult {
        let r = extract_json_body(&mut req).await?;
        let r: json::PostUser = serde_json::from_slice(&r).map_err(|e| bad_req(e.to_string()))?;
        let mut db = self.db.lock();
        let user = db
            .users_by_id()
            .get(&id)
            .ok_or_else(|| format_err_t!(Internal, "can't find currently authenticated user"))?;
        if let Some(precondition) = r.precondition {
            if matches!(precondition.preferences, Some(p) if p != user.config.preferences) {
                bail_t!(FailedPrecondition, "preferences mismatch");
            }
        }
        if let Some(update) = r.update {
            let mut change = user.change();
            if let Some(preferences) = update.preferences {
                change.config.preferences = preferences;
            }
            db.apply_user_change(change).map_err(internal_server_err)?;
        }
        Ok(plain_response(StatusCode::NO_CONTENT, &b""[..]))
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
                host.as_deref(),
                &authreq.addr,
                agent.as_deref(),
                self.is_secure(req)
            ),
        ))
    }

    /// Returns true iff the client is connected over `https`.
    /// Moonfire NVR currently doesn't directly serve `https`, but it supports
    /// proxies which set the `X-Forwarded-Proto` header. See `guide/secure.md`
    /// for more information.
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

        // If the request came in over https, tell the browser to only send the cookie on https
        // requests also.
        let is_secure = self.is_secure(&req);

        // Use SameSite=Lax rather than SameSite=Strict. Safari apparently doesn't send
        // SameSite=Strict cookies on WebSocket upgrade requests. There's no real security
        // difference for Moonfire NVR anyway. SameSite=Strict exists as CSRF protection for
        // sites that (unlike Moonfire NVR) don't follow best practices by (a)
        // mutating based on GET requests and (b) not using CSRF tokens.
        use auth::SessionFlag;
        let flags = (SessionFlag::HttpOnly as i32)
            | (SessionFlag::SameSite as i32)
            | if is_secure {
                SessionFlag::Secure as i32
            } else {
                0
            };
        let (sid, _) = l
            .login_by_password(authreq, &r.username, r.password, Some(domain), flags)
            .map_err(|e| plain_response(StatusCode::UNAUTHORIZED, e.to_string()))?;
        let cookie = encode_sid(sid, flags);
        Ok(Response::builder()
            .header(
                header::SET_COOKIE,
                HeaderValue::try_from(cookie).expect("cookie can't have invalid bytes"),
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
            bail_t!(PermissionDenied, "update_signals required");
        }
        let r = extract_json_body(&mut req).await?;
        let r: json::PostSignalsRequest =
            serde_json::from_slice(&r).map_err(|e| bad_req(e.to_string()))?;
        let now = recording::Time::new(self.db.clocks().realtime());
        let mut l = self.db.lock();
        let start = match r.start {
            json::PostSignalsTimeBase::Epoch(t) => t,
            json::PostSignalsTimeBase::Now(d) => now + d,
        };
        let end = match r.end {
            json::PostSignalsTimeBase::Epoch(t) => t,
            json::PostSignalsTimeBase::Now(d) => now + d,
        };
        l.update_signals(start..end, &r.signal_ids, &r.states)
            .map_err(from_base_error)?;
        serve_json(&req, &json::PostSignalsResponse { time_90k: now })
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
                signals.times_90k.push(c.when);
                signals.signal_ids.push(c.signal);
                signals.states.push(c.state);
            });
        serve_json(req, &signals)
    }

    /// Authenticates the session (if any) and returns a Caller.
    ///
    /// If there's no session,
    /// 1.  if `allow_unauthenticated_permissions` is configured, returns okay
    ///     with those permissions.
    /// 2.  if the caller specifies `unauth_path`, returns okay with no
    ///     permissions.
    /// 3.  returns `Unauthenticated` error otherwise.
    ///
    /// Does no authorization. That is, this doesn't check that the returned
    /// permissions are sufficient for whatever operation the caller is
    /// performing.
    fn authenticate(
        &self,
        req: &Request<hyper::Body>,
        unauth_path: bool,
    ) -> Result<Caller, base::Error> {
        if let Some(sid) = extract_sid(req) {
            let authreq = self.authreq(req);

            match self.db.lock().authenticate_session(authreq, &sid.hash()) {
                Ok((s, u)) => {
                    return Ok(Caller {
                        permissions: s.permissions.clone(),
                        user: Some(json::ToplevelUser {
                            id: s.user_id,
                            name: u.username.clone(),
                            preferences: u.config.preferences.clone(),
                            session: Some(json::Session { csrf: s.csrf() }),
                        }),
                    })
                }
                Err(e) if e.kind() == base::ErrorKind::Unauthenticated => {
                    // Log the specific reason this session is unauthenticated.
                    // Don't let the API client see it, as it may have a
                    // revocation reason that isn't for their eyes.
                    warn!("Session authentication failed: {:?}", &e);
                }
                Err(e) => return Err(e),
            };
        }

        if let Some(s) = self.allow_unauthenticated_permissions.as_ref() {
            return Ok(Caller {
                permissions: s.clone(),
                user: None,
            });
        }

        if unauth_path {
            return Ok(Caller {
                permissions: db::Permissions::default(),
                user: None,
            });
        }

        bail_t!(Unauthenticated, "unauthenticated");
    }
}

/// Encodes a session into `Set-Cookie` header value form.
fn encode_sid(sid: db::RawSessionId, flags: i32) -> String {
    let mut cookie = String::with_capacity(128);
    cookie.push_str("s=");
    base64::encode_config_buf(&sid, base64::STANDARD_NO_PAD, &mut cookie);
    use auth::SessionFlag;
    if (flags & SessionFlag::HttpOnly as i32) != 0 {
        cookie.push_str("; HttpOnly");
    }
    if (flags & SessionFlag::Secure as i32) != 0 {
        cookie.push_str("; Secure");
    }
    if (flags & SessionFlag::SameSiteStrict as i32) != 0 {
        cookie.push_str("; SameSite=Strict");
    } else if (flags & SessionFlag::SameSite as i32) != 0 {
        cookie.push_str("; SameSite=Lax");
    }
    cookie.push_str("; Max-Age=2147483648; Path=/");
    cookie
}

#[derive(Debug, Eq, PartialEq)]
struct StaticFileRequest<'a> {
    path: &'a str,
    immutable: bool,
    mime: &'static str,
}

impl<'a> StaticFileRequest<'a> {
    fn parse(path: &'a str) -> Option<Self> {
        if !path.starts_with('/') || path == "/index.html" {
            return None;
        }

        let (path, immutable) = match &path[1..] {
            // These well-known URLs don't have content hashes in them, and
            // thus aren't immutable.
            "" => ("index.html", false),
            "robots.txt" => ("robots.txt", false),
            "site.webmanifest" => ("site.webmanifest", false),

            // Everything else should.
            p => (p, true),
        };

        let last_dot = match path.rfind('.') {
            None => return None,
            Some(d) => d,
        };
        let ext = &path[last_dot + 1..];
        let mime = match ext {
            "css" => "text/css",
            "html" => "text/html",
            "ico" => "image/x-icon",
            "js" | "map" => "text/javascript",
            "json" => "application/json",
            "png" => "image/png",
            "svg" => "image/svg+xml",
            "txt" => "text/plain",
            "webmanifest" => "application/manifest+json",
            "woff2" => "font/woff2",
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
            self.0.clone().unwrap()
        }
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
            .get("user")
            .unwrap()
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

    #[test]
    fn encode_sid() {
        use super::encode_sid;
        use db::auth::{RawSessionId, SessionFlag};
        let s64 = "3LbrruP5vj/hpE8kvYTz/rNDg4BleRiTCHGA3Ocm91z/YrtxHDxexmrz46biZJxJ";
        let s = RawSessionId::decode_base64(s64.as_bytes()).unwrap();
        assert_eq!(
            encode_sid(
                s,
                (SessionFlag::Secure as i32)
                    | (SessionFlag::HttpOnly as i32)
                    | (SessionFlag::SameSite as i32)
                    | (SessionFlag::SameSiteStrict as i32)
            ),
            format!(
                "s={}; HttpOnly; Secure; SameSite=Strict; Max-Age=2147483648; Path=/",
                s64
            )
        );
        assert_eq!(
            encode_sid(s, SessionFlag::SameSite as i32),
            format!("s={}; SameSite=Lax; Max-Age=2147483648; Path=/", s64)
        );
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
