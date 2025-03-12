// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! `/view.mp4` and `/view.m4s` handling.

use base::{bail, err};
use db::recording::{self, rescale};
use http::{Request, StatusCode};
use nom::bytes::complete::{tag, take_while1};
use nom::combinator::{all_consuming, map, map_res, opt};
use nom::sequence::{preceded, tuple};
use nom::IResult;
use std::borrow::Borrow;
use std::cmp;
use std::convert::TryFrom;
use std::ops::Range;
use std::str::FromStr;
use tracing::trace;
use url::form_urlencoded;
use uuid::Uuid;

use crate::mp4;
use crate::web::plain_response;

use super::{Caller, ResponseResult, Service};

impl Service {
    pub(super) fn stream_view_mp4(
        &self,
        req: &Request<::hyper::body::Incoming>,
        caller: Caller,
        uuid: Uuid,
        stream_type: db::StreamType,
        mp4_type: mp4::Type,
        debug: bool,
    ) -> ResponseResult {
        if !caller.permissions.view_video {
            bail!(PermissionDenied, msg("view_video required"));
        }
        let (stream_id, camera_name);

        // False positive: on Rust 1.78.0, clippy erroneously suggests calling `clone_from` on the
        // uninitialized `camera_name`.
        // Apparently fixed in rustc 1.80.0-nightly (ada5e2c7b 2024-05-31).
        #[allow(clippy::assigning_clones)]
        {
            let db = self.db.lock();
            let camera = db
                .get_camera(uuid)
                .ok_or_else(|| err!(NotFound, msg("no such camera {uuid}")))?;
            camera_name = camera.short_name.clone();
            stream_id = camera.streams[stream_type.index()]
                .ok_or_else(|| err!(NotFound, msg("no such stream {uuid}/{stream_type}")))?;
        };
        let mut start_time_for_filename = None;
        let mut builder = mp4::FileBuilder::new(mp4_type);
        if let Some(q) = req.uri().query() {
            for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                let (key, value) = (key.borrow(), value.borrow());
                match key {
                    "s" => {
                        let s = Segments::from_str(value).map_err(|()| {
                            err!(InvalidArgument, msg("invalid s parameter: {value}"))
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
                                    bail!(
                                        NotFound,
                                        msg(
                                            "recording {} has open id {}, requested {}",
                                            r.id,
                                            r.open_id,
                                            o,
                                        ),
                                    );
                                }
                            }

                            // Check for missing recordings.
                            match prev {
                                None if recording_id == s.ids.start => {}
                                None => bail!(
                                    NotFound,
                                    msg("no such recording {}/{}", stream_id, s.ids.start),
                                ),
                                Some(id) if r.id.recording() != id + 1 => {
                                    bail!(
                                        NotFound,
                                        msg("no such recording {}/{}", stream_id, id + 1)
                                    );
                                }
                                _ => {}
                            };
                            prev = Some(recording_id);

                            // Add a segment for the relevant part of the recording, if any.
                            // Note all calculations here are in wall times / wall durations.
                            let end_time = s.end_time.unwrap_or(i64::MAX);
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
                                let mr =
                                    rescale(wr.start, r.wall_duration_90k, r.media_duration_90k)
                                        ..rescale(
                                            wr.end,
                                            r.wall_duration_90k,
                                            r.media_duration_90k,
                                        );
                                builder.append(&db, &r, mr, true)?;
                            } else {
                                trace!("...skipping recording {} wall dur {}", r.id, wd);
                            }
                            cur_off += wd;
                            Ok(())
                        })?;

                        // Check for missing recordings.
                        match prev {
                            Some(id) if s.ids.end != id + 1 => {
                                bail!(
                                    NotFound,
                                    msg("no such recording {}/{}", stream_id, s.ids.end - 1),
                                );
                            }
                            None => {
                                bail!(
                                    NotFound,
                                    msg("no such recording {}/{}", stream_id, s.ids.start),
                                );
                            }
                            _ => {}
                        };
                        if let Some(end) = s.end_time {
                            if end > cur_off {
                                bail!(
                                    InvalidArgument,
                                    msg("end time {end} is beyond specified recordings"),
                                );
                            }
                        }
                    }
                    "ts" => builder.include_timestamp_subtitle_track(value == "true")?,
                    _ => bail!(InvalidArgument, msg("parameter {key} not understood")),
                }
            }
        }
        if let Some(start) = start_time_for_filename {
            let zone = base::time::global_zone();
            let tm = jiff::Timestamp::from_second(start.unix_seconds())
                .expect("valid start")
                .to_zoned(zone);
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
            builder.set_filename(&format!(
                "{}-{}-{}.{}",
                tm.strftime("%Y%m%d%H%M%S"),
                camera_name,
                stream_abbrev,
                suffix
            ))?;
        }
        let mp4 = builder.build(self.db.clone(), self.dirs_by_stream_id.clone())?;
        if debug {
            return Ok(plain_response(StatusCode::OK, format!("{mp4:#?}")));
        }
        Ok(http_serve::serve(mp4, req))
    }
}

/// Represents a single `s=` (segments) query parameter as supplied to `/view.mp4`.
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
    /// Parses the `s` query parameter to `view.mp4` as described in `ref/api.md`.
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

#[cfg(test)]
mod tests {
    use crate::web::tests::Server;
    use db::testutil;
    use std::str::FromStr;

    use super::Segments;

    #[tokio::test]
    async fn view_without_segments() {
        testutil::init();
        let mut permissions = db::Permissions::new();
        permissions.view_video = true;
        let s = Server::new(Some(permissions));
        let cli = reqwest::Client::new();
        let resp = cli
            .get(format!(
                "{}/api/cameras/{}/main/view.mp4",
                &s.base_url, s.db.test_camera_uuid
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
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
}
