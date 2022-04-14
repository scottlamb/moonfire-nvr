// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Live video websocket handling.

use std::sync::Arc;

use crate::body::Body;
use base::{bail_t, format_err_t};
use failure::Error;
use futures::{future::Either, SinkExt, StreamExt};
use http::{header, Request, Response, StatusCode};
use log::{info, warn};
use tokio_tungstenite::tungstenite;
use uuid::Uuid;

use crate::{mp4, web::plain_response};

use super::{bad_req, Caller, ResponseResult, Service};

/// Checks the `Host` and `Origin` headers match, if the latter is supplied.
///
/// Web browsers must supply origin, according to [RFC 6455 section
/// 4.1](https://datatracker.ietf.org/doc/html/rfc6455#section-4.1).
/// It's not required for non-browser HTTP clients.
///
/// If present, verify it. Chrome doesn't honor the `s=` cookie's
/// `SameSite=Lax` setting for WebSocket requests, so this is the sole
/// protection against [CSWSH](https://christian-schneider.net/CrossSiteWebSocketHijacking.html).
fn check_origin(headers: &header::HeaderMap) -> Result<(), super::HttpError> {
    let origin_hdr = match headers.get(http::header::ORIGIN) {
        None => return Ok(()),
        Some(o) => o,
    };
    let host_hdr = headers
        .get(header::HOST)
        .ok_or_else(|| bad_req("missing Host header"))?;
    let host_str = host_hdr.to_str().map_err(|_| bad_req("bad Host header"))?;

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
        .ok_or_else(|| bad_req("bad Origin header"))?;
    let origin_host = origin_url
        .host_str()
        .ok_or_else(|| bad_req("bad Origin header"))?;
    if host != origin_host {
        bail_t!(
            PermissionDenied,
            "cross-origin request forbidden (request host {:?}, origin {:?})",
            host_hdr,
            origin_hdr
        );
    }
    Ok(())
}

impl Service {
    pub(super) fn stream_live_m4s(
        self: Arc<Self>,
        req: Request<::hyper::Body>,
        caller: Caller,
        uuid: Uuid,
        stream_type: db::StreamType,
    ) -> ResponseResult {
        check_origin(req.headers())?;
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
