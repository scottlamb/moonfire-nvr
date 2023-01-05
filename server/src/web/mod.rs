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

use self::accept::ConnData;
use self::path::Path;
use crate::body::Body;
use crate::json;
use crate::mp4;
use base::{bail_t, clock::Clocks, ErrorKind};
use core::borrow::Borrow;
use core::str::FromStr;
use db::dir::SampleFileDir;
use db::{auth, recording};
use failure::{format_err, Error};
use fnv::FnvHashMap;
use http::header::{self, HeaderValue};
use http::{status::StatusCode, Request, Response};
use http_serve::dir::FsDir;
use hyper::body::Bytes;
use log::{debug, warn};
use std::net::IpAddr;
use std::sync::Arc;
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

type ResponseResult = Result<Response<Body>, HttpError>;

fn serve_json<T: serde::ser::Serialize>(req: &Request<hyper::Body>, out: &T) -> ResponseResult {
    let (mut resp, writer) = http_serve::streaming_body(req).build();
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
    for hdr in req.headers().get_all(header::COOKIE) {
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
    }
    None
}

/// Extracts an `application/json` POST body from a request.
///
/// This returns the request body as bytes rather than performing
/// deserialization. Keeping the bytes allows the caller to use a `Deserialize`
/// that borrows from the bytes.
async fn extract_json_body(req: &mut Request<hyper::Body>) -> Result<Bytes, HttpError> {
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

fn require_csrf_if_session(caller: &Caller, csrf: Option<&str>) -> Result<(), base::Error> {
    match (csrf, caller.user.as_ref().and_then(|u| u.session.as_ref())) {
        (None, Some(_)) => bail_t!(Unauthenticated, "csrf must be supplied"),
        (Some(csrf), Some(session)) if !csrf_matches(csrf, session.csrf) => {
            bail_t!(Unauthenticated, "incorrect csrf");
        }
        (_, _) => Ok(()),
    }
}

pub struct Config<'a> {
    pub db: Arc<db::Database>,
    pub ui_dir: Option<&'a std::path::Path>,
    pub trust_forward_hdrs: bool,
    pub time_zone_name: String,
    pub allow_unauthenticated_permissions: Option<db::Permissions>,
    pub privileged_unix_uid: Option<nix::unistd::Uid>,
}

pub struct Service {
    db: Arc<db::Database>,
    ui_dir: Option<Arc<FsDir>>,
    dirs_by_stream_id: Arc<FnvHashMap<i32, Arc<SampleFileDir>>>,
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
        let mut ui_dir = None;
        if let Some(d) = config.ui_dir {
            match FsDir::builder().for_path(&d) {
                Err(e) => {
                    warn!(
                        "Unable to load ui dir {}; will serve no static files: {}",
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
            privileged_unix_uid: config.privileged_unix_uid,
        })
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
            Path::Request => (CacheControl::PrivateDynamic, self.request(&req, caller)?),
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
                self.stream_view_mp4(
                    &req,
                    caller,
                    uuid,
                    type_,
                    mp4::Type::MediaSegment { sequence_number: 1 },
                    debug,
                )?,
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
    /// An error return from this method causes hyper to abruptly drop the
    /// HTTP connection rather than respond. That's not terribly useful, so this
    /// method always returns `Ok`. It delegates to a `serve_inner` which is
    /// allowed to generate `Err` results with the `?` operator, but returns
    /// them to hyper as `Ok` results.
    pub async fn serve(
        self: Arc<Self>,
        req: Request<::hyper::Body>,
        conn_data: ConnData,
    ) -> Result<Response<Body>, std::convert::Infallible> {
        let p = Path::decode(req.uri().path());
        let always_allow_unauthenticated = matches!(
            p,
            Path::NotFound | Path::Request | Path::Login | Path::Logout | Path::Static
        );
        debug!("request on: {}: {:?}", req.uri(), p);
        let caller = match self.authenticate(&req, &conn_data, always_allow_unauthenticated) {
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
                permissions: caller.permissions.into(),
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

    fn request(&self, req: &Request<::hyper::Body>, caller: Caller) -> ResponseResult {
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
                    secure: {:?}\n\
                    caller: {:?}\n",
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
                self.is_secure(req),
                &caller,
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
        req: &Request<hyper::Body>,
        conn_data: &ConnData,
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

        bail_t!(Unauthenticated, "unauthenticated");
    }
}

#[cfg(test)]
mod tests {
    use db::testutil::{self, TestDb};
    use futures::future::FutureExt;
    use http::{header, Request};
    use std::sync::Arc;

    pub(super) struct Server {
        pub(super) db: TestDb<base::clock::RealClocks>,
        pub(super) base_url: String,
        //test_camera_uuid: Uuid,
        handle: Option<::std::thread::JoinHandle<()>>,
        shutdown_tx: Option<futures::channel::oneshot::Sender<()>>,
    }

    impl Server {
        pub(super) fn new(allow_unauthenticated_permissions: Option<db::Permissions>) -> Server {
            let db = TestDb::new(base::clock::RealClocks {});
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
            let make_svc = hyper::service::make_service_fn(move |_conn| {
                futures::future::ok::<_, std::convert::Infallible>(hyper::service::service_fn({
                    let s = Arc::clone(&service);
                    move |req| {
                        Arc::clone(&s).serve(
                            req,
                            super::accept::ConnData {
                                client_unix_uid: None,
                                client_addr: None,
                            },
                        )
                    }
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

    #[test]
    fn test_extract_sid() {
        let req = Request::builder()
            .header(header::COOKIE, "foo=asdf; bar=asdf")
            .header(
                header::COOKIE,
                "s=OsL6Cg4ikLw6UIXOT28tI+vPez3qWACovI+nLHWyjsW1ERX83qRrOR3guKedc8IP",
            )
            .body(hyper::Body::empty())
            .unwrap();
        let sid = super::extract_sid(&req).unwrap();
        assert_eq!(sid.as_ref(), &b":\xc2\xfa\n\x0e\"\x90\xbc:P\x85\xceOo-#\xeb\xcf{=\xeaX\x00\xa8\xbc\x8f\xa7,u\xb2\x8e\xc5\xb5\x11\x15\xfc\xde\xa4k9\x1d\xe0\xb8\xa7\x9ds\xc2\x0f"[..]);
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
                    privileged_unix_uid: None,
                })
                .unwrap(),
            );
            let make_svc = hyper::service::make_service_fn(move |_conn| {
                futures::future::ok::<_, std::convert::Infallible>(hyper::service::service_fn({
                    let s = Arc::clone(&service);
                    move |req| {
                        Arc::clone(&s).serve(
                            req,
                            super::accept::ConnData {
                                client_unix_uid: None,
                                client_addr: None,
                            },
                        )
                    }
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
