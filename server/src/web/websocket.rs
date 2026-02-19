// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Common code for WebSockets, including the live view WebSocket and a future
//! WebSocket for watching database changes.

use std::pin::Pin;

use crate::body::Body;
use base::{bail, err};
use futures::Future;
use http::{header, Request, Response};
use tokio_tungstenite::tungstenite;
use tracing::Instrument;

pub type WebSocketStream =
    tokio_tungstenite::WebSocketStream<hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>>;

/// Upgrades to WebSocket and runs the supplied stream handler in a separate tokio task.
///
/// Fails on `Origin` mismatch with an HTTP-level error. If the handler returns
/// an error, tries to send it to the client before dropping the stream.
pub(super) fn upgrade<H>(
    req: Request<::hyper::body::Incoming>,
    handler: H,
) -> Result<Response<Body>, base::Error>
where
    for<'a> H: FnOnce(&'a mut WebSocketStream) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>
        + Send
        + 'static,
{
    // An `Origin` mismatch should be a HTTP-level error; this is likely a cross-site attack,
    // and using HTTP-level errors avoids giving any information to the Javascript running in
    // the browser.
    check_origin(req.headers())?;

    // Otherwise, upgrade and handle the rest in a separate task.
    let response = tungstenite::handshake::server::create_response_with_body(&req, Body::empty)
        .map_err(|e| err!(InvalidArgument, source(e)))?;
    let span = tracing::info_span!("websocket");
    tokio::task::Builder::new()
        .name("websocket")
        .spawn(
            async move {
                let upgraded = match hyper::upgrade::on(req).await {
                    Ok(u) => u,
                    Err(err) => {
                        tracing::error!(%err, "upgrade failed");
                        return;
                    }
                };
                let upgraded = hyper_util::rt::TokioIo::new(upgraded);
                let mut ws = WebSocketStream::from_raw_socket(
                    upgraded,
                    tungstenite::protocol::Role::Server,
                    None,
                )
                .await;
                handler(&mut ws).await;
                let _ = ws.close(None).await;
            }
            .instrument(span),
        )
        .expect("spawning should succeed");
    Ok(response)
}

/// Checks the `Host` and `Origin` headers match, if the latter is supplied.
///
/// Web browsers must supply origin, according to [RFC 6455 section
/// 4.1](https://datatracker.ietf.org/doc/html/rfc6455#section-4.1).
/// It's not required for non-browser HTTP clients.
///
/// If present, verify it. Chrome doesn't honor the `s=` cookie's
/// `SameSite=Lax` setting for WebSocket requests, so this is the sole
/// protection against [CSWSH](https://christian-schneider.net/CrossSiteWebSocketHijacking.html).
fn check_origin(headers: &header::HeaderMap) -> Result<(), base::Error> {
    let origin_hdr = match headers.get(http::header::ORIGIN) {
        None => return Ok(()),
        Some(o) => o,
    };
    let Some(host_hdr) = headers.get(header::HOST) else {
        bail!(InvalidArgument, msg("missing Host header"));
    };
    let host_str = host_hdr
        .to_str()
        .map_err(|_| err!(InvalidArgument, msg("bad Host header")))?;

    // Currently this ignores the port number. This is easiest and I think matches the browser's
    // rules for when it sends a cookie, so it probably doesn't cause great security problems.
    let host = match host_str.split_once(':') {
        Some((host, _port)) => host,
        None => host_str,
    };
    let origin_url = origin_hdr
        .to_str()
        .ok()
        .and_then(|o| url::Url::parse(o).ok())
        .ok_or_else(|| err!(InvalidArgument, msg("bad Origin header")))?;
    let origin_host = origin_url
        .host_str()
        .ok_or_else(|| err!(InvalidArgument, msg("bad Origin header")))?;
    if host != origin_host {
        bail!(
            PermissionDenied,
            msg(
                "cross-origin request forbidden (request host {host_hdr:?}, origin {origin_hdr:?})"
            ),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::convert::TryInto;

    use super::*;

    #[test]
    fn origin_port_8080_okay() {
        // By default, Moonfire binds to port 8080. Make sure that specifying a port number works.
        let mut hdrs = header::HeaderMap::new();
        hdrs.insert(header::HOST, "nvr:8080".try_into().unwrap());
        hdrs.insert(header::ORIGIN, "http://nvr:8080/".try_into().unwrap());
        assert!(check_origin(&hdrs).is_ok());
    }

    #[test]
    fn origin_missing_okay() {
        let mut hdrs = header::HeaderMap::new();
        hdrs.insert(header::HOST, "nvr".try_into().unwrap());
        assert!(check_origin(&hdrs).is_ok());
    }

    #[test]
    fn origin_mismatch_fails() {
        let mut hdrs = header::HeaderMap::new();
        hdrs.insert(header::HOST, "nvr".try_into().unwrap());
        hdrs.insert(header::ORIGIN, "http://evil/".try_into().unwrap());
        assert!(check_origin(&hdrs).is_err());
    }
}
