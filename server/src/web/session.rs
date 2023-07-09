// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Session management: `/api/login` and `/api/logout`.

use base::{bail_t, ErrorKind, ResultExt};
use db::auth;
use http::{header, HeaderValue, Method, Request, Response, StatusCode};
use memchr::memchr;
use tracing::{info, warn};

use crate::{json, web::parse_json_body};

use super::{
    csrf_matches, extract_json_body, extract_sid, plain_response, ResponseResult, Service,
};
use std::convert::TryFrom;

impl Service {
    pub(super) async fn login(
        &self,
        mut req: Request<::hyper::Body>,
        authreq: auth::Request,
    ) -> ResponseResult {
        if *req.method() != Method::POST {
            return Ok(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "POST expected",
            ));
        }
        let r = extract_json_body(&mut req).await?;
        let r: json::LoginRequest = parse_json_body(&r)?;
        let Some(host) = req.headers().get(header::HOST) else {
            bail_t!(InvalidArgument, "missing Host header");
        };
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
            .login_by_password(authreq, r.username, r.password, Some(domain), flags)
            .err_kind(ErrorKind::Unauthenticated)?;
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

    pub(super) async fn logout(
        &self,
        mut req: Request<hyper::Body>,
        authreq: auth::Request,
    ) -> ResponseResult {
        if *req.method() != Method::POST {
            return Ok(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "POST expected",
            ));
        }
        let r = extract_json_body(&mut req).await?;
        let r: json::LogoutRequest = parse_json_body(&r)?;

        let mut res = Response::new(b""[..].into());
        if let Some(sid) = extract_sid(&req) {
            let mut l = self.db.lock();
            let hash = sid.hash();
            match l.authenticate_session(authreq.clone(), &hash) {
                Ok((s, _)) => {
                    if !csrf_matches(r.csrf, s.csrf()) {
                        bail_t!(InvalidArgument, "logout with incorret csrf token");
                    }
                    info!("revoking session");
                    l.revoke_session(auth::RevocationReason::LoggedOut, None, authreq, &hash)
                        .err_kind(ErrorKind::Internal)?;
                }
                Err(e) => {
                    // TODO: distinguish "no such session", "session is no longer valid", and
                    // "user ... is disabled" (which are all client error / bad state) from database
                    // errors.
                    warn!("logout failed: {}", e);
                }
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
}

/// Encodes a session into `Set-Cookie` header value form.
fn encode_sid(sid: db::RawSessionId, flags: i32) -> String {
    let mut cookie = String::with_capacity(128);
    cookie.push_str("s=");
    base64::encode_config_buf(sid, base64::STANDARD_NO_PAD, &mut cookie);
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

#[cfg(test)]
mod tests {
    use db::testutil;
    use fnv::FnvHashMap;
    use tracing::info;

    use crate::web::tests::Server;

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

        let mut p = FnvHashMap::default();
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
        let mut p = FnvHashMap::default();
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
        let mut p = FnvHashMap::default();
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
            format!("s={s64}; HttpOnly; Secure; SameSite=Strict; Max-Age=2147483648; Path=/")
        );
        assert_eq!(
            encode_sid(s, SessionFlag::SameSite as i32),
            format!("s={s64}; SameSite=Lax; Max-Age=2147483648; Path=/")
        );
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
}
