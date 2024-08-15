// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Live video websocket handling.

use std::sync::Arc;

use base::{bail, err, Error};
use futures::SinkExt;
use http::header;
use tokio::sync::broadcast::error::RecvError;
use tokio_tungstenite::{tungstenite, WebSocketStream};
use uuid::Uuid;

use crate::mp4;

use super::{Caller, Service};

/// Interval at which to send keepalives if there are no frames.
///
/// Chrome appears to time out WebSockets after 60 seconds of inactivity.
/// If the camera is disconnected or not sending frames, we'd like to keep
/// the connection open so everything will recover when the camera comes back.
const KEEPALIVE_AFTER_IDLE: tokio::time::Duration = tokio::time::Duration::from_secs(30);

impl Service {
    pub(super) async fn stream_live_m4s(
        self: Arc<Self>,
        ws: &mut WebSocketStream<hyper::upgrade::Upgraded>,
        caller: Result<Caller, Error>,
        uuid: Uuid,
        stream_type: db::StreamType,
    ) -> Result<(), Error> {
        let caller = caller?;
        if !caller.permissions.view_video {
            bail!(PermissionDenied, msg("view_video required"));
        }

        let stream_id;
        let open_id;
        let mut sub_rx = {
            let mut db = self.db.lock();
            open_id = match db.open {
                None => {
                    bail!(
                        FailedPrecondition,
                        msg("database is read-only; there are no live streams"),
                    );
                }
                Some(o) => o.id,
            };
            let camera = db
                .get_camera(uuid)
                .ok_or_else(|| err!(NotFound, msg("no such camera {uuid}")))?;
            stream_id = camera.streams[stream_type.index()]
                .ok_or_else(|| err!(NotFound, msg("no such stream {uuid}/{stream_type}")))?;
            db.watch_live(stream_id).expect("stream_id refed by camera")
        };

        let mut keepalive = tokio::time::interval(KEEPALIVE_AFTER_IDLE);
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // On the first LiveFrame, send all the data from the previous key frame
        // onward. Afterward, send a single (often non-key) frame at a time.
        let mut start_at_key = true;
        loop {
            tokio::select! {
                biased;

                next = sub_rx.recv() => {
                    match next {
                        Ok(l) => {
                            keepalive.reset_after(KEEPALIVE_AFTER_IDLE);
                            if !self.stream_live_m4s_chunk(
                                open_id,
                                stream_id,
                                ws,
                                l,
                                start_at_key,
                            ).await? {
                                return Ok(());
                            }
                            start_at_key = false;
                        }
                        Err(RecvError::Closed) => {
                            bail!(Internal, msg("live stream closed unexpectedly"));
                        }
                        Err(RecvError::Lagged(frames)) => {
                            bail!(
                                ResourceExhausted,
                                msg("subscriber {frames} frames further behind than allowed; \
                                     this typically indicates insufficient bandwidth"),
                            )
                        }
                    }
                }

                _ = keepalive.tick() => {
                    if ws.send(tungstenite::Message::Ping(Vec::new())).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }

    /// Sends a single live segment chunk of a `live.m4s` stream, returning `Ok(false)` when
    /// the connection is lost.
    async fn stream_live_m4s_chunk(
        &self,
        open_id: u32,
        stream_id: i32,
        ws: &mut tokio_tungstenite::WebSocketStream<hyper::upgrade::Upgraded>,
        live: db::LiveFrame,
        start_at_key: bool,
    ) -> Result<bool, Error> {
        let mut builder = mp4::FileBuilder::new(mp4::Type::MediaSegment);
        let mut row = None;
        {
            let db = self.db.lock();
            let mut rows = 0;
            db.list_recordings_by_id(stream_id, live.recording..live.recording + 1, &mut |r| {
                rows += 1;
                builder.append(&db, &r, live.media_off_90k.clone(), start_at_key)?;
                row = Some(r);
                Ok(())
            })?;
        }
        let row = row.ok_or_else(|| err!(Internal, msg("unable to find {live:?}")))?;
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
        Ok(ws.send(tungstenite::Message::Binary(v)).await.is_ok())
    }
}
