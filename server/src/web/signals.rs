// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! `/api/signals` handling.

use base::{bail_t, clock::Clocks};
use db::recording;
use http::{Method, Request, StatusCode};
use url::form_urlencoded;

use crate::json;

use super::{
    bad_req, extract_json_body, from_base_error, plain_response, serve_json, Caller,
    ResponseResult, Service,
};

use std::borrow::Borrow;

impl Service {
    pub(super) async fn signals(
        &self,
        req: Request<hyper::Body>,
        caller: Caller,
    ) -> ResponseResult {
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
}
