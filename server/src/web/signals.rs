// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! `/api/signals` handling.

use base::{bail, clock::Clocks, err};
use db::recording;
use http::{Method, Request, StatusCode};
use url::form_urlencoded;

use crate::json;

use super::{
    into_json_body, parse_json_body, plain_response, require_csrf_if_session, serve_json, Caller,
    ResponseResult, Service,
};

use std::borrow::Borrow;

impl Service {
    pub(super) async fn signals(
        &self,
        req: Request<hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        match *req.method() {
            Method::POST => self.post_signals(req, caller).await,
            Method::GET | Method::HEAD => self.get_signals(&req),
            _ => Ok(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "POST, GET, or HEAD expected",
            )),
        }
    }

    async fn post_signals(
        &self,
        req: Request<hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        if !caller.permissions.update_signals {
            bail!(PermissionDenied, msg("update_signals required"));
        }
        let (parts, b) = into_json_body(req).await?;
        let r: json::PostSignalsRequest = parse_json_body(&b)?;
        require_csrf_if_session(&caller, r.csrf)?;
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
        l.update_signals(start..end, &r.signal_ids, &r.states)?;
        serve_json(&parts, &json::PostSignalsResponse { time_90k: now })
    }

    fn get_signals(&self, req: &Request<hyper::body::Incoming>) -> ResponseResult {
        let mut time = recording::Time::MIN..recording::Time::MAX;
        if let Some(q) = req.uri().query() {
            for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                let (key, value) = (key.borrow(), value.borrow());
                match key {
                    "startTime90k" => {
                        time.start = recording::Time::parse(value)
                            .map_err(|_| err!(InvalidArgument, msg("unparseable startTime90k")))?
                    }
                    "endTime90k" => {
                        time.end = recording::Time::parse(value)
                            .map_err(|_| err!(InvalidArgument, msg("unparseable endTime90k")))?
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
}
