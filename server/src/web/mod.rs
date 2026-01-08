// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

pub mod accept;
mod live;
mod path;
mod session;
mod signals;
mod static_file;
mod users;
mod view;
mod websocket;

use self::accept::ConnData;
use self::path::Path;
use crate::body::Body;
use crate::json;
use crate::mp4;
use crate::web::static_file::Ui;
use base::err;
use base::Error;
use base::ResultExt;
use base::{bail, clock::Clocks, ErrorKind};
use core::borrow::Borrow;
use core::str::FromStr;
use db::{auth, recording};
use http::header::{self, HeaderValue};
use http::{status::StatusCode, Request, Response};
use hyper::body::Bytes;
use std::net::IpAddr;
use std::sync::Arc;
use tracing::warn;
use tracing::Instrument;
use url::form_urlencoded;
use uuid::Uuid;

fn plain_response<B: Into<Body>>(status: http::StatusCode, body: B) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"))
        .body(body.into())
        .expect("hardcoded head should be valid")
}

fn from_base_error(err: &base::Error) -> Response<Body> {
    use ErrorKind::*;
    let status_code = match err.kind() {
        Unauthenticated => StatusCode::UNAUTHORIZED,
        PermissionDenied => StatusCode::FORBIDDEN,
        InvalidArgument => StatusCode::BAD_REQUEST,
        FailedPrecondition => StatusCode::PRECONDITION_FAILED,
        NotFound => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    plain_response(status_code, err.to_string())
}

#[derive(Debug)]
struct Caller {
    permissions: db::Permissions,
    user: Option<json::ToplevelUser>,
}

type ResponseResult = Result<Response<Body>, base::Error>;

fn serve_json<R: http_serve::AsRequest, T: serde::ser::Serialize>(
    req: &R,
    out: &T,
) -> ResponseResult {
    let (mut resp, writer) = http_serve::streaming_body(req).build();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Some(mut w) = writer {
        serde_json::to_writer(&mut w, out).err_kind(ErrorKind::Internal)?;
    }
    Ok(resp)
}

fn csrf_matches(csrf: &str, session: auth::SessionHash) -> bool {
    let mut b64 = [0u8; 32];
    session.encode_base64(&mut b64);
    use subtle::ConstantTimeEq as _;
    b64.ct_eq(csrf.as_bytes()).into()
}

/// Extracts `s` cookie from the HTTP request headers. Does not authenticate.
fn extract_sid(req_hdrs: &http::HeaderMap) -> Option<auth::RawSessionId> {
    for hdr in req_hdrs.get_all(header::COOKIE) {
        for mut cookie in hdr.as_bytes().split(|&b| b == b';') {
            if cookie.starts_with(b" ") {
                cookie = &cookie[1..];
            }
            if let Some(s) = cookie.strip_prefix(b"s=") {
                if let Ok(s) = auth::RawSessionId::decode_base64(s) {
                    return Some(s);
                }
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
async fn into_json_body(
    req: Request<hyper::body::Incoming>,
) -> Result<(http::request::Parts, Bytes), base::Error> {
    let correct_mime_type = match req.headers().get(header::CONTENT_TYPE) {
        Some(t) if t == "application/json" => true,
        Some(t) if t == "application/json; charset=UTF-8" => true,
        _ => false,
    };
    if !correct_mime_type {
        bail!(
            InvalidArgument,
            msg("expected application/json request body")
        );
    }
    let (parts, b) = req.into_parts();
    let b = http_body_util::BodyExt::collect(b)
        .await
        .map_err(|e| err!(Unavailable, msg("unable to read request body"), source(e)))?
        .to_bytes();
    Ok((parts, b))
}

fn parse_json_body<'a, T: serde::Deserialize<'a>>(body: &'a [u8]) -> Result<T, base::Error> {
    serde_json::from_slice(body)
        .map_err(|e| err!(InvalidArgument, msg("bad request body"), source(e)).build())
}

fn require_csrf_if_session(caller: &Caller, csrf: Option<&str>) -> Result<(), base::Error> {
    match (csrf, caller.user.as_ref().and_then(|u| u.session.as_ref())) {
        (None, Some(_)) => bail!(Unauthenticated, msg("csrf must be supplied")),
        (Some(csrf), Some(session)) if !csrf_matches(csrf, session.csrf) => {
            bail!(Unauthenticated, msg("incorrect csrf"));
        }
        (_, _) => Ok(()),
    }
}

pub struct Config<'a> {
    pub db: Arc<db::Database>,
    pub ui_dir: Option<&'a crate::cmds::run::config::UiDir>,
    pub trust_forward_hdrs: bool,
    pub time_zone_name: String,
    pub allow_unauthenticated_permissions: Option<db::Permissions>,
    pub privileged_unix_uid: Option<nix::unistd::Uid>,
}

pub struct Service {
    db: Arc<db::Database>,
    sample_entries: db::sample_entries::Handle,
    ui: Ui,
    time_zone_name: String,
    allow_unauthenticated_permissions: Option<db::Permissions>,
    trust_forward_hdrs: bool,
    privileged_unix_uid: Option<nix::unistd::Uid>,
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
        let ui_dir = config.ui_dir.map(Ui::from).unwrap_or(Ui::None);
        let sample_entries = config.db.lock().sample_entries().clone();
        Ok(Service {
            db: config.db,
            sample_entries,
            ui: ui_dir,
            allow_unauthenticated_permissions: config.allow_unauthenticated_permissions,
            trust_forward_hdrs: config.trust_forward_hdrs,
            time_zone_name: config.time_zone_name,
            privileged_unix_uid: config.privileged_unix_uid,
        })
    }

    /// Serves an HTTP request.
    ///
    /// The `Err` return path will cause the `serve` wrapper to log the error,
    /// as well as returning it to the HTTP client.
    async fn serve_inner(
        self: Arc<Self>,
        req: Request<::hyper::body::Incoming>,
        authreq: auth::Request,
        conn_data: ConnData,
    ) -> ResponseResult {
        let path = Path::decode(req.uri().path());
        tracing::trace!(?path, "path");
        let always_allow_unauthenticated = matches!(
            path,
            Path::NotFound | Path::Request | Path::Login | Path::Logout | Path::Static
        );
        let caller = self.authenticate(&req, &authreq, &conn_data, always_allow_unauthenticated);
        if let Some(username) = caller
            .as_ref()
            .ok()
            .and_then(|c| c.user.as_ref())
            .map(|u| &u.name)
        {
            tracing::Span::current().record("enduser.id", tracing::field::display(username));
        }

        // WebSocket stuff is handled separately, because most authentication
        // errors are returned as text messages over the protocol, rather than
        // HTTP-level errors.
        if let Path::StreamLiveMp4Segments(uuid, type_) = path {
            return websocket::upgrade(req, move |ws| {
                Box::pin(self.stream_live_m4s(ws, caller, uuid, type_))
            });
        }

        let caller = caller?;
        let (cache, mut response) = match path {
            Path::InitSegment(sha1, debug) => (
                CacheControl::PrivateStatic,
                self.init_segment(sha1, debug, &req)?,
            ),
            Path::TopLevel => (CacheControl::PrivateDynamic, self.top_level(&req, caller)?),
            Path::Request => (
                CacheControl::PrivateDynamic,
                self.request(&req, &authreq, caller)?,
            ),
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
            Path::StreamLiveMp4Segments(..) => {
                unreachable!("StreamLiveMp4Segments should have already been handled")
            }
            Path::NotFound => bail!(NotFound, msg("path not understood")),
            Path::Login => (
                CacheControl::PrivateDynamic,
                self.login(req, authreq).await?,
            ),
            Path::Logout => (
                CacheControl::PrivateDynamic,
                self.logout(req, authreq).await?,
            ),
            Path::Signals => (
                CacheControl::PrivateDynamic,
                self.signals(req, caller).await?,
            ),
            Path::Static => (CacheControl::None, self.static_file(req).await?),
            Path::Users => (CacheControl::PrivateDynamic, self.users(req, caller).await?),
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
    ///
    /// An error return from this method causes hyper to abruptly drop the
    /// HTTP connection rather than respond. That's not terribly useful, so this
    /// method always returns `Ok`. It delegates to a `serve_inner` which is
    /// allowed to generate `Err` results with the `?` operator, but returns
    /// them to hyper as `Ok` results.
    pub async fn serve(
        self: Arc<Self>,
        req: Request<::hyper::body::Incoming>,
        conn_data: ConnData,
    ) -> Result<Response<Body>, std::convert::Infallible> {
        let request_id = uuid::Uuid::now_v7();
        let authreq = auth::Request {
            when_sec: Some(self.db.clocks().realtime().as_secs()),
            addr: if self.trust_forward_hdrs {
                req.headers()
                    .get("X-Real-IP")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| IpAddr::from_str(v).ok())
            } else {
                conn_data.client_addr.map(|a| a.ip())
            },
            user_agent: req
                .headers()
                .get(header::USER_AGENT)
                .map(|ua| ua.as_bytes().to_vec()),
        };
        let start = std::time::Instant::now();

        // https://opentelemetry.io/docs/reference/specification/trace/semantic_conventions/http/
        let span = tracing::info_span!(
            "request",
            request_id = %data_encoding::BASE32_NOPAD.encode_display(request_id.as_bytes()),
            net.sock.peer.uid = conn_data.client_unix_uid.map(tracing::field::display),
            http.client_ip = authreq.addr.map(tracing::field::display),
            http.method = %req.method(),
            http.target = %req.uri(),
            http.status_code = tracing::field::Empty,
            enduser.id = tracing::field::Empty,
        );
        tracing::debug!(parent: &span, "received request headers");
        let response = self
            .serve_inner(req, authreq, conn_data)
            .instrument(span.clone())
            .await;
        let (response, error) = match response {
            Ok(r) => (r, None),
            Err(e) => (from_base_error(&e), Some(e)),
        };
        span.record("http.status_code", response.status().as_u16());
        let latency = std::time::Instant::now().duration_since(start);
        if response.status().is_server_error() {
            tracing::error!(
                parent: &span,
                latency = latency.as_secs_f32(),
                error = error.map(tracing::field::display),
                "sending response headers",
            );
        } else if response.status().is_client_error() {
            tracing::warn!(
                parent: &span,
                latency = latency.as_secs_f32(),
                error = error.map(tracing::field::display),
                "sending response headers",
            );
        } else {
            tracing::info!(
                parent: &span,
                latency = latency.as_secs_f32(),
                error = error.map(tracing::field::display),
                "sending response headers",
            );
        }
        Ok(response)
    }

    fn top_level(&self, req: &Request<::hyper::body::Incoming>, caller: Caller) -> ResponseResult {
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
            bail!(PermissionDenied, msg("read_camera_configs required"));
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
                permissions: caller.permissions.into(),
            },
        )
    }

    fn camera(&self, req: &Request<::hyper::body::Incoming>, uuid: Uuid) -> ResponseResult {
        let db = self.db.lock();
        let camera = db
            .get_camera(uuid)
            .ok_or_else(|| err!(NotFound, msg("no such camera {uuid}")))?;
        serve_json(
            req,
            &json::Camera::wrap(camera, &db, true, false).err_kind(ErrorKind::Internal)?,
        )
    }

    fn stream_recordings(
        &self,
        req: &Request<::hyper::body::Incoming>,
        uuid: Uuid,
        type_: db::StreamType,
    ) -> ResponseResult {
        let (r, split) = {
            let mut time = recording::Time::MIN..recording::Time::MAX;
            let mut split = recording::Duration(i64::MAX);
            if let Some(q) = req.uri().query() {
                for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                    let (key, value) = (key.borrow(), value.borrow());
                    match key {
                        "startTime90k" => {
                            time.start = recording::Time::parse(value).map_err(|_| {
                                err!(InvalidArgument, msg("unparseable startTime90k"))
                            })?
                        }
                        "endTime90k" => {
                            time.end = recording::Time::parse(value)
                                .map_err(|_| err!(InvalidArgument, msg("unparseable endTime90k")))?
                        }
                        "split90k" => {
                            split =
                                recording::Duration(i64::from_str(value).map_err(|_| {
                                    err!(InvalidArgument, msg("unparseable split90k"))
                                })?)
                        }
                        _ => {}
                    }
                }
            }
            (time, split)
        };
        let db = self.db.lock();
        let mut recordings = Vec::new();
        let mut vse_ids = Vec::new();
        let Some(camera) = db.get_camera(uuid) else {
            bail!(NotFound, msg("no such camera {uuid}"));
        };
        let Some(stream_id) = camera.streams[type_.index()] else {
            bail!(NotFound, msg("no such stream {uuid}/{type_}"));
        };
        db.list_aggregated_recordings(stream_id, r, split, &mut |row| {
            let end = row.ids.end - 1; // in api, ids are inclusive.
            recordings.push(json::Recording {
                start_id: row.ids.start,
                end_id: if end == row.ids.start {
                    None
                } else {
                    Some(end)
                },
                run_start_id: row.run_start_id,
                start_time_90k: row.time.start.0,
                end_time_90k: row.time.end.0,
                sample_file_bytes: row.sample_file_bytes,
                open_id: row.open_id,
                first_uncommitted: row.first_uncommitted,
                video_samples: row.video_samples,
                video_sample_entry_id: row.video_sample_entry_id,
                growing: row.growing,
                has_trailing_zero: row.has_trailing_zero,
                end_reason: row.end_reason.clone(),
            });
            if !vse_ids.contains(&row.video_sample_entry_id) {
                vse_ids.push(row.video_sample_entry_id);
            }
            Ok(())
        })
        .err_kind(ErrorKind::Internal)?;
        let video_sample_entries: Vec<_> = {
            let sample_entries = db.sample_entries().lock();
            vse_ids
                .iter()
                .map(|id| {
                    sample_entries
                        .get_video(*id)
                        .expect("row.video_sample_entry_id should exist")
                })
                .collect()
        };
        drop(db);
        serve_json(
            req,
            &json::ListRecordings {
                recordings,
                video_sample_entries,
            },
        )
    }

    fn init_segment(
        &self,
        id: i32,
        debug: bool,
        req: &Request<::hyper::body::Incoming>,
    ) -> ResponseResult {
        let mut builder = mp4::FileBuilder::new(mp4::Type::InitSegment);
        let Some(ent) = self.sample_entries.lock().get_video(id) else {
            bail!(NotFound, msg("no such init segment"));
        };
        builder.append_video_sample_entry(ent);
        let mp4 = builder
            .build(self.db.clone())
            .err_kind(ErrorKind::Internal)?;
        if debug {
            Ok(plain_response(StatusCode::OK, format!("{mp4:#?}")))
        } else {
            Ok(http_serve::serve(mp4, req))
        }
    }

    fn request(
        &self,
        req: &Request<::hyper::body::Incoming>,
        authreq: &auth::Request,
        caller: Caller,
    ) -> ResponseResult {
        let host = req
            .headers()
            .get(header::HOST)
            .map(|h| String::from_utf8_lossy(h.as_bytes()));
        let agent = authreq
            .user_agent
            .as_ref()
            .map(|u| String::from_utf8_lossy(&u[..]));
        let when = authreq.when_sec.map(|sec| {
            jiff::Timestamp::from_second(sec)
                .expect("valid time")
                .to_zoned(base::time::global_zone())
                .strftime("%FT%T%:z")
        });
        Ok(plain_response(
            StatusCode::OK,
            format!(
                "when: {:?}\n\
                    host: {:?}\n\
                    addr: {:?}\n\
                    user_agent: {:?}\n\
                    secure: {:?}\n\
                    caller: {:?}\n",
                when,
                host.as_deref(),
                &authreq.addr,
                agent.as_deref(),
                self.is_secure(req.headers()),
                &caller,
            ),
        ))
    }

    /// Returns true iff the client is connected over `https`.
    /// Moonfire NVR currently doesn't directly serve `https`, but it supports
    /// proxies which set the `X-Forwarded-Proto` header. See `guide/secure.md`
    /// for more information.
    fn is_secure(&self, hdrs: &http::HeaderMap) -> bool {
        self.trust_forward_hdrs
            && hdrs
                .get("X-Forwarded-Proto")
                .map(|v| v.as_bytes() == b"https")
                .unwrap_or(false)
    }

    /// Authenticates the session (if any) and returns a Caller.
    ///
    /// If there's no session,
    /// 1.  if connected via Unix domain socket from the same effective uid
    ///     as Moonfire NVR itself, return with all privileges.
    /// 2.  if `allow_unauthenticated_permissions` is configured, returns okay
    ///     with those permissions.
    /// 3.  if the caller specifies `unauth_path`, returns okay with no
    ///     permissions.
    /// 4.  returns `Unauthenticated` error otherwise.
    ///
    /// Does no authorization. That is, this doesn't check that the returned
    /// permissions are sufficient for whatever operation the caller is
    /// performing.
    fn authenticate(
        &self,
        req: &Request<hyper::body::Incoming>,
        authreq: &auth::Request,
        conn_data: &ConnData,
        unauth_path: bool,
    ) -> Result<Caller, base::Error> {
        if let Some(sid) = extract_sid(req.headers()) {
            match self
                .db
                .lock()
                .authenticate_session(authreq.clone(), &sid.hash())
            {
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
                Err(err) if err.kind() == base::ErrorKind::Unauthenticated => {
                    // Log the specific reason this session is unauthenticated.
                    // Don't let the API client see it, as it may have a
                    // revocation reason that isn't for their eyes.
                    warn!(err = %err.chain(), "session authentication failed");
                }
                Err(err) => return Err(err),
            };
        }

        if matches!(conn_data.client_unix_uid, Some(uid) if Some(uid) == self.privileged_unix_uid) {
            return Ok(Caller {
                permissions: db::Permissions {
                    view_video: true,
                    read_camera_configs: true,
                    update_signals: true,
                    admin_users: true,
                    ..Default::default()
                },
                user: None,
            });
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

        bail!(Unauthenticated);
    }
}

#[cfg(test)]
mod tests {
    use db::testutil::{self, TestDb};
    // use futures::future::FutureExt;
    // use http::{header, Request};
    use http::header;
    use std::sync::Arc;

    pub(super) struct Server {
        pub(super) db: TestDb<base::clock::RealClocks>,
        pub(super) base_url: String,
        //test_camera_uuid: Uuid,
        handle: Option<::std::thread::JoinHandle<()>>,
        shutdown_tx: Option<futures::channel::oneshot::Sender<()>>,
    }

    impl Server {
        pub(super) async fn new(
            allow_unauthenticated_permissions: Option<db::Permissions>,
        ) -> Server {
            let db = TestDb::new(base::clock::RealClocks {}).await;
            let (shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel::<()>();
            let service = Arc::new(
                super::Service::new(super::Config {
                    db: db.db.clone(),
                    ui_dir: None,
                    allow_unauthenticated_permissions,
                    trust_forward_hdrs: true,
                    time_zone_name: "".to_owned(),
                    privileged_unix_uid: None,
                })
                .unwrap(),
            );
            let (addr_tx, addr_rx) = std::sync::mpsc::channel();
            let handle = ::std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let service = Arc::clone(&service);
                rt.block_on(async move {
                    let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0));
                    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
                    let addr = listener.local_addr().unwrap();
                    let mut shutdown_rx = std::pin::pin!(shutdown_rx);
                    addr_tx.send(addr).unwrap();
                    loop {
                        let (tcp, _) = tokio::select! {
                            r = listener.accept() => r.unwrap(),
                            _ = shutdown_rx.as_mut() => return,
                        };
                        tcp.set_nodelay(true).unwrap();
                        let io = hyper_util::rt::TokioIo::new(tcp);
                        let service = Arc::clone(&service);
                        let serve = move |req| {
                            Arc::clone(&service).serve(
                                req,
                                super::accept::ConnData {
                                    client_unix_uid: None,
                                    client_addr: None,
                                },
                            )
                        };
                        tokio::task::spawn(async move {
                            hyper::server::conn::http1::Builder::new()
                                .serve_connection(io, hyper::service::service_fn(serve))
                                .await
                                .unwrap();
                        });
                    }
                });
            });
            let addr = addr_rx.recv().unwrap();

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
            let _ = self.shutdown_tx.take().unwrap().send(());
            self.handle.take().unwrap().join().unwrap()
        }
    }

    #[tokio::test]
    async fn unauthorized_without_cookie() {
        testutil::init();
        let s = Server::new(None).await;
        let cli = reqwest::Client::new();
        let resp = cli
            .get(format!("{}/api/", &s.base_url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_extract_sid() {
        let mut hdrs = http::HeaderMap::new();
        hdrs.append(header::COOKIE, "foo=asdf; bar=asdf".parse().unwrap());
        hdrs.append(
            header::COOKIE,
            "s=OsL6Cg4ikLw6UIXOT28tI+vPez3qWACovI+nLHWyjsW1ERX83qRrOR3guKedc8IP"
                .parse()
                .unwrap(),
        );
        let sid = super::extract_sid(&hdrs).unwrap();
        assert_eq!(sid.as_ref(), &b":\xc2\xfa\n\x0e\"\x90\xbc:P\x85\xceOo-#\xeb\xcf{=\xeaX\x00\xa8\xbc\x8f\xa7,u\xb2\x8e\xc5\xb5\x11\x15\xfc\xde\xa4k9\x1d\xe0\xb8\xa7\x9ds\xc2\x0f"[..]);
    }
}

#[cfg(all(test, feature = "nightly"))]
mod bench {
    extern crate test;

    use db::testutil::{self, TestDb};
    use hyper::{self, service::service_fn};
    use std::{
        net::SocketAddr,
        sync::{Arc, OnceLock},
    };
    use uuid::Uuid;

    struct Server {
        base_url: String,
        test_camera_uuid: Uuid,
    }

    impl Server {
        fn new() -> Server {
            let (uuid_tx, uuid_rx) = std::sync::mpsc::sync_channel(1);
            let addr: SocketAddr = ([127, 0, 0, 1], 0).into();
            let listener = std::net::TcpListener::bind(addr).unwrap();
            listener.set_nonblocking(true).unwrap();
            let addr = listener.local_addr().unwrap(); // resolve port 0 to a real ephemeral port number.
            let srv = async move {
                let db = TestDb::new(::base::clock::RealClocks {}).await;
                uuid_tx.try_send(db.test_camera_uuid).unwrap();
                testutil::add_dummy_recordings_to_db(&db.db, 1440);
                let service = Arc::new(
                    super::Service::new(super::Config {
                        db: db.db.clone(),
                        ui_dir: None,
                        allow_unauthenticated_permissions: Some(db::Permissions::default()),
                        trust_forward_hdrs: false,
                        time_zone_name: "".to_owned(),
                        privileged_unix_uid: None,
                    })
                    .unwrap(),
                );
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                loop {
                    let (conn, _remote_addr) = listener.accept().await.unwrap();
                    conn.set_nodelay(true).unwrap();
                    let io = hyper_util::rt::TokioIo::new(conn);
                    let service = Arc::clone(&service);
                    let svc_fn = service_fn(move |req| {
                        Arc::clone(&service).serve(
                            req,
                            super::accept::ConnData {
                                client_unix_uid: None,
                                client_addr: None,
                            },
                        )
                    });
                    tokio::spawn(
                        hyper::server::conn::http1::Builder::new().serve_connection(io, svc_fn),
                    );
                }
            };
            std::thread::Builder::new()
                .name("bench-server".to_owned())
                .spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    rt.block_on(srv)
                })
                .unwrap();
            Server {
                base_url: format!("http://{}:{}", addr.ip(), addr.port()),
                test_camera_uuid: uuid_rx.recv().unwrap(),
            }
        }
    }

    static SERVER: OnceLock<Server> = OnceLock::new();

    #[bench]
    fn serve_stream_recordings(b: &mut test::Bencher) {
        testutil::init();
        let server = SERVER.get_or_init(Server::new);
        let url = reqwest::Url::parse(&format!(
            "{}/api/cameras/{}/main/recordings",
            server.base_url, server.test_camera_uuid
        ))
        .unwrap();
        let client = reqwest::Client::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let f = || {
            for _i in 0..100 {
                rt.block_on(async {
                    let resp = client.get(url.clone()).send().await.unwrap();
                    assert_eq!(resp.status(), reqwest::StatusCode::OK);
                    let _b = resp.bytes().await.unwrap();
                });
            }
        };
        f(); // warm.
        b.iter(f);
    }
}
