// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2016 Scott Lamb <slamb@slamb.org>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In addition, as a special exception, the copyright holders give
// permission to link the code of portions of this program with the
// OpenSSL library under certain conditions as described in each
// individual source file, and distribute linked combinations including
// the two.
//
// You must obey the GNU General Public License in all respects for all
// of the code used other than OpenSSL. If you modify file(s) with this
// exception, you may extend this exception to your version of the
// file(s), but you are not obligated to do so. If you do not wish to do
// so, delete this exception statement from your version. If you delete
// this exception statement from all source files in the program, then
// also delete it here.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

extern crate hyper;

use core::borrow::Borrow;
use core::str::FromStr;
use db;
use dir::SampleFileDir;
use error::Error;
use futures::{future, stream};
use json;
use http_entity;
use hyper::header;
use hyper::server::{self, Request, Response};
use mime;
use mp4;
use parking_lot::MutexGuard;
use recording;
use reffers::ARefs;
use regex::Regex;
use serde_json;
use slices;
use std::cmp;
use std::fmt;
use std::io::Write;
use std::ops::Range;
use std::sync::Arc;
use strutil;
use time;
use url::form_urlencoded;
use uuid::Uuid;

const BINARY_PREFIXES: &'static [&'static str] = &[" ", " Ki", " Mi", " Gi", " Ti", " Pi", " Ei"];
const DECIMAL_PREFIXES: &'static [&'static str] =&[" ", " k", " M", " G", " T", " P", " E"];

lazy_static! {
    /// Regex used to parse the `s` query parameter to `view.mp4`.
    /// As described in `design/api.md`, this is of the form
    /// `START_ID[-END_ID][.[REL_START_TIME]-[REL_END_TIME]]`.
    static ref SEGMENTS_RE: Regex = Regex::new(r"^(\d+)(-\d+)?(?:\.(\d+)?-(\d+)?)?$").unwrap();
}

enum Path {
    CamerasList,                 // "/" or "/cameras/"
    InitSegment([u8; 20]),       // "/init/<sha1>.mp4"
    Camera(Uuid),                // "/cameras/<uuid>/"
    CameraRecordings(Uuid),      // "/cameras/<uuid>/recordings"
    CameraViewMp4(Uuid),         // "/cameras/<uuid>/view.mp4"
    CameraViewMp4Segment(Uuid),  // "/cameras/<uuid>/view.m4s"
    NotFound,
}

fn decode_path(path: &str) -> Path {
    if path == "/" {
        return Path::CamerasList;
    }
    if path.starts_with("/init/") {
        if path.len() != 50 || !path.ends_with(".mp4") {
            return Path::NotFound;
        }
        if let Ok(sha1) = strutil::dehex(&path.as_bytes()[6..46]) {
            return Path::InitSegment(sha1);
        }
        return Path::NotFound;
    }
    if !path.starts_with("/cameras/") {
        return Path::NotFound;
    }
    let path = &path["/cameras/".len()..];
    if path == "" {
        return Path::CamerasList;
    }
    let slash = match path.find('/') {
        None => { return Path::NotFound; },
        Some(s) => s,
    };
    let (uuid, path) = path.split_at(slash);

    // TODO(slamb): require uuid to be in canonical format.
    let uuid = match Uuid::parse_str(uuid) {
        Ok(u) => u,
        Err(_) => { return Path::NotFound },
    };
    match path {
        "/" => Path::Camera(uuid),
        "/recordings" => Path::CameraRecordings(uuid),
        "/view.mp4" => Path::CameraViewMp4(uuid),
        "/view.m4s" => Path::CameraViewMp4Segment(uuid),
        _ => Path::NotFound,
    }
}

fn is_json(req: &Request) -> bool {
    if let Some(accept) = req.headers().get::<header::Accept>() {
        return accept.len() == 1 && accept[0].item == mime::APPLICATION_JSON &&
               accept[0].quality == header::q(1000);
    }
    false
}

pub struct HtmlEscaped<'a>(&'a str);

impl<'a> fmt::Display for HtmlEscaped<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut last_end = 0;
        for (start, part) in self.0.match_indices(|c| c == '<' || c == '&') {
            f.write_str(&self.0[last_end..start])?;
            f.write_str(if part == "<" { "&lt;" } else { "&amp;" })?;
            last_end = start + 1;
        }
        f.write_str(&self.0[last_end..])
    }
}

pub struct Humanized(i64);

impl Humanized {
    fn do_fmt(&self, base: f32, prefixes: &[&str], f: &mut fmt::Formatter) -> fmt::Result {
        let mut n = self.0 as f32;
        let mut i = 0;
        loop {
            if n < base || i >= prefixes.len() - 1 {
                break;
            }
            n /= base;
            i += 1;
        }
        write!(f, "{:.1}{}", n, prefixes[i])
    }
}

impl fmt::Display for Humanized {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.do_fmt(1000., DECIMAL_PREFIXES, f)
    }
}

impl fmt::Binary for Humanized {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.do_fmt(1024., BINARY_PREFIXES, f)
    }
}

pub struct HumanizedTimestamp(Option<recording::Time>);

impl fmt::Display for HumanizedTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.0 {
            None => f.write_str("n/a"),
            Some(t) => {
                let tm = time::at(time::Timespec{sec: t.unix_seconds(), nsec: 0});
                write!(f, "{}",
                       tm.strftime("%a, %d %b %Y %H:%M:%S %Z").or_else(|_| Err(fmt::Error))?)
            }
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Segments {
    ids: Range<i32>,
    start_time: i64,
    end_time: Option<i64>,
}

impl Segments {
    pub fn parse(input: &str) -> Result<Segments, ()> {
        let caps = SEGMENTS_RE.captures(input).ok_or(())?;
        let ids_start = i32::from_str(caps.get(1).unwrap().as_str()).map_err(|_| ())?;
        let ids_end = match caps.get(2) {
            Some(e) => i32::from_str(&e.as_str()[1..]).map_err(|_| ())?,
            None => ids_start,
        } + 1;
        if ids_start < 0 || ids_end <= ids_start {
            return Err(());
        }
        let start_time = caps.get(3).map_or(Ok(0), |m| i64::from_str(m.as_str())).map_err(|_| ())?;
        if start_time < 0 {
            return Err(());
        }
        let end_time = match caps.get(4) {
            Some(v) => {
                let e = i64::from_str(v.as_str()).map_err(|_| ())?;
                if e <= start_time {
                    return Err(());
                }
                Some(e)
            },
            None => None
        };
        Ok(Segments{
            ids: ids_start .. ids_end,
            start_time: start_time,
            end_time: end_time,
        })
    }
}

pub struct Service {
    db: Arc<db::Database>,
    dir: Arc<SampleFileDir>,
}

impl Service {
    pub fn new(db: Arc<db::Database>, dir: Arc<SampleFileDir>) -> Self {
        Service{db: db, dir: dir}
    }

    fn not_found(&self) -> Result<Response<slices::Body>, Error> {
        let body: slices::Body = Box::new(stream::once(Ok(ARefs::new(&b"not found"[..]))));
        Ok(Response::new()
            .with_status(hyper::StatusCode::NotFound)
            .with_header(header::ContentType(mime::TEXT_PLAIN))
            .with_body(body))
    }

    fn list_cameras(&self, req: &Request) -> Result<Response<slices::Body>, Error> {
        let json = is_json(req);
        let buf = {
            let db = self.db.lock();
            if json {
                serde_json::to_vec(&json::ListCameras{cameras: db.cameras_by_id()})?
            } else {
                self.list_cameras_html(db)?
            }
        };
        let len = buf.len();
        let body: slices::Body = Box::new(stream::once(Ok(ARefs::new(buf))));
        Ok(Response::new()
           .with_header(header::ContentType(if json { mime::APPLICATION_JSON }
                                            else { mime::TEXT_HTML }))
           .with_header(header::ContentLength(len as u64))
           .with_body(body))
    }

    fn list_cameras_html(&self, db: MutexGuard<db::LockedDatabase>) -> Result<Vec<u8>, Error> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"\
            <!DOCTYPE html>\n\
            <html>\n\
            <head>\n\
            <title>Camera list</title>\n\
            <meta http-equiv=\"Content-Language\" content=\"en\">\n\
            <style type=\"text/css\">\n\
            .header { background-color: #ddd; }\n\
            td { padding-right: 3em; }\n\
            </style>\n\
            </head>\n\
            <body>\n\
            <table>\n");
        for row in db.cameras_by_id().values() {
            write!(&mut buf, "\
                <tr class=header><td colspan=2><a href=\"/cameras/{}/\">{}</a></td></tr>\n\
                <tr><td>description</td><td>{}</td></tr>\n\
                <tr><td>space</td><td>{:b}B / {:b}B ({:.1}%)</td></tr>\n\
                <tr><td>uuid</td><td>{}</td></tr>\n\
                <tr><td>oldest recording</td><td>{}</td></tr>\n\
                <tr><td>newest recording</td><td>{}</td></tr>\n\
                <tr><td>total duration</td><td>{}</td></tr>\n",
                row.uuid, HtmlEscaped(&row.short_name), HtmlEscaped(&row.description),
                Humanized(row.sample_file_bytes), Humanized(row.retain_bytes),
                100. * row.sample_file_bytes as f32 / row.retain_bytes as f32,
                row.uuid, HumanizedTimestamp(row.range.as_ref().map(|r| r.start)),
                HumanizedTimestamp(row.range.as_ref().map(|r| r.end)),
                row.duration)?;
        }
        Ok(buf)
    }

    fn camera(&self, uuid: Uuid, query: Option<&str>, req: &Request)
              -> Result<Response<slices::Body>, Error> {
        let json = is_json(req);
        let buf = {
            let db = self.db.lock();
            if json {
                let camera = db.get_camera(uuid)
                               .ok_or_else(|| Error::new("no such camera".to_owned()))?;
                serde_json::to_vec(&json::Camera::new(camera, true))?
            } else {
                self.camera_html(db, query, uuid)?
            }
        };
        let len = buf.len();
        let body: slices::Body = Box::new(stream::once(Ok(ARefs::new(buf))));
        Ok(Response::new()
           .with_header(header::ContentType(if json { mime::APPLICATION_JSON }
                                            else { mime::TEXT_HTML }))
           .with_header(header::ContentLength(len as u64))
           .with_body(body))
    }

    fn camera_html(&self, db: MutexGuard<db::LockedDatabase>, query: Option<&str>,
                   uuid: Uuid) -> Result<Vec<u8>, Error> {
        let (r, trim) = {
            let mut time = recording::Time(i64::min_value()) .. recording::Time(i64::max_value());
            let mut trim = false;
            if let Some(q) = query {
                for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                    let (key, value) = (key.borrow(), value.borrow());
                    match key {
                        "start_time" => time.start = recording::Time::parse(value)?,
                        "end_time" => time.end = recording::Time::parse(value)?,
                        "trim" if value == "true" => trim = true,
                        _ => {},
                    }
                };
            }
            (time, trim)
        };
        let camera = db.get_camera(uuid)
                       .ok_or_else(|| Error::new("no such camera".to_owned()))?;
        let mut buf = Vec::new();
        write!(&mut buf, "\
            <!DOCTYPE html>\n\
            <html>\n\
            <head>\n\
            <title>{0} recordings</title>\n\
            <meta http-equiv=\"Content-Language\" content=\"en\">\n\
            <style type=\"text/css\">\n\
            tr:not(:first-child):hover {{ background-color: #ddd; }}\n\
            th, td {{ padding: 0.5ex 1.5em; text-align: right; }}\n\
            </style>\n\
            </head>\n\
            <body>\n\
            <h1>{0}</h1>\n\
            <p>{1}</p>\n\
            <table>\n\
            <tr><th>start</th><th>end</th><th>resolution</th>\
            <th>fps</th><th>size</th><th>bitrate</th>\
            </tr>\n",
            HtmlEscaped(&camera.short_name), HtmlEscaped(&camera.description))?;

        // Rather than listing each 60-second recording, generate a HTML row for aggregated .mp4
        // files of up to FORCE_SPLIT_DURATION each, provided there is no gap or change in video
        // parameters between recordings.
        static FORCE_SPLIT_DURATION: recording::Duration =
            recording::Duration(60 * 60 * recording::TIME_UNITS_PER_SEC);
        let mut rows = Vec::new();
        db.list_aggregated_recordings(camera.id, r.clone(), FORCE_SPLIT_DURATION, |row| {
            rows.push(row.clone());
            Ok(())
        })?;

        // Display newest recording first.
        rows.sort_by(|r1, r2| r2.ids.start.cmp(&r1.ids.start));

        for row in &rows {
            let seconds = (row.time.end.0 - row.time.start.0) / recording::TIME_UNITS_PER_SEC;
            let url = {
                let mut url = String::with_capacity(64);
                use std::fmt::Write;
                write!(&mut url, "view.mp4?s={}", row.ids.start)?;
                if row.ids.end != row.ids.start + 1 {
                    write!(&mut url, "-{}", row.ids.end - 1)?;
                }
                if trim {
                    let rel_start = if row.time.start < r.start { Some(r.start - row.time.start) }
                                    else { None };
                    let rel_end = if row.time.end > r.end { Some(r.end - row.time.start) }
                                  else { None };
                    if rel_start.is_some() || rel_end.is_some() {
                        url.push('.');
                        if let Some(s) = rel_start { write!(&mut url, "{}", s.0)?; }
                        url.push('-');
                        if let Some(e) = rel_end { write!(&mut url, "{}", e.0)?; }
                    }
                }
                url
            };
            let start = if trim && row.time.start < r.start { r.start } else { row.time.start };
            let end = if trim && row.time.end > r.end { r.end } else { row.time.end };
            write!(&mut buf, "\
                <tr><td><a href=\"{}\">{}</a></td>\
                <td>{}</td><td>{}x{}</td><td>{:.0}</td><td>{:b}B</td><td>{}bps</td></tr>\n",
                url, HumanizedTimestamp(Some(start)), HumanizedTimestamp(Some(end)),
                row.video_sample_entry.width, row.video_sample_entry.height,
                if seconds == 0 { 0. } else { row.video_samples as f32 / seconds as f32 },
                Humanized(row.sample_file_bytes),
                Humanized(if seconds == 0 { 0 } else { row.sample_file_bytes * 8 / seconds }))?;
        };
        buf.extend_from_slice(b"</table>\n</html>\n");
        Ok(buf)
    }

    fn camera_recordings(&self, uuid: Uuid, query: Option<&str>, req: &Request)
                         -> Result<Response<slices::Body>, Error> {
        let r = Service::get_optional_range(query)?;
        if !is_json(req) {
            let body: slices::Body = Box::new(stream::once(
                Ok(ARefs::new(&b"only available for JSON requests"[..]))));
            return Ok(Response::new()
                      .with_status(hyper::StatusCode::NotAcceptable)
                      .with_body(body));
        }
        let mut out = json::ListRecordings{recordings: Vec::new()};
        {
            let db = self.db.lock();
            let camera = db.get_camera(uuid)
                           .ok_or_else(|| Error::new("no such camera".to_owned()))?;
            db.list_aggregated_recordings(camera.id, r, recording::Duration(i64::max_value()),
                                          |row| {
                out.recordings.push(json::Recording{
                    start_time_90k: row.time.start.0,
                    end_time_90k: row.time.end.0,
                    sample_file_bytes: row.sample_file_bytes,
                    video_samples: row.video_samples,
                    video_sample_entry_width: row.video_sample_entry.width,
                    video_sample_entry_height: row.video_sample_entry.height,
                    video_sample_entry_sha1: strutil::hex(&row.video_sample_entry.sha1),
                });
                Ok(())
            })?;
        }
        let buf = serde_json::to_vec(&out)?;
        let len = buf.len();
        let body: slices::Body = Box::new(stream::once(Ok(ARefs::new(buf))));
        Ok(Response::new()
           .with_header(header::ContentType(mime::APPLICATION_JSON))
           .with_header(header::ContentLength(len as u64))
           .with_body(body))
    }

    fn init_segment(&self, sha1: [u8; 20], req: &Request) -> Result<Response<slices::Body>, Error> {
        let mut builder = mp4::FileBuilder::new(mp4::Type::InitSegment);
        let db = self.db.lock();
        for ent in db.video_sample_entries() {
            if ent.sha1 == sha1 {
                builder.append_video_sample_entry(ent.clone());
                let mp4 = builder.build(self.db.clone(), self.dir.clone())?;
                return Ok(http_entity::serve(mp4, req));
            }
        }
        self.not_found()
    }

    fn camera_view_mp4(&self, uuid: Uuid, type_: mp4::Type, query: Option<&str>, req: &Request)
                       -> Result<Response<slices::Body>, Error> {
        let camera_id = {
            let db = self.db.lock();
            let camera = db.get_camera(uuid)
                           .ok_or_else(|| Error::new("no such camera".to_owned()))?;
            camera.id
        };
        let mut builder = mp4::FileBuilder::new(type_);
        if let Some(q) = query {
            for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                let (key, value) = (key.borrow(), value.borrow());
                match key {
                    "s" => {
                        let s = Segments::parse(value).map_err(
                            |_| Error::new(format!("invalid s parameter: {}", value)))?;
                        debug!("camera_view_mp4: appending s={:?}", s);
                        let mut est_segments = (s.ids.end - s.ids.start) as usize;
                        if let Some(end) = s.end_time {
                            // There should be roughly ceil((end - start) /
                            // desired_recording_duration) recordings in the desired timespan if
                            // there are no gaps or overlap, possibly another for misalignment of
                            // the requested timespan with the rotate offset and another because
                            // rotation only happens at key frames.
                            let ceil_durations = (end - s.start_time +
                                                  recording::DESIRED_RECORDING_DURATION - 1) /
                                                 recording::DESIRED_RECORDING_DURATION;
                            est_segments = cmp::min(est_segments, (ceil_durations + 2) as usize);
                        }
                        builder.reserve(est_segments);
                        let db = self.db.lock();
                        let mut prev = None;
                        let mut cur_off = 0;
                        db.list_recordings_by_id(camera_id, s.ids.clone(), |r| {
                            // Check for missing recordings.
                            match prev {
                                None if r.id == s.ids.start => {},
                                None => return Err(Error::new(format!("no such recording {}/{}",
                                                                      camera_id, s.ids.start))),
                                Some(id) if r.id != id + 1 => {
                                    return Err(Error::new(format!("no such recording {}/{}",
                                                                  camera_id, id + 1)));
                                },
                                _ => {},
                            };
                            prev = Some(r.id);

                            // Add a segment for the relevant part of the recording, if any.
                            let end_time = s.end_time.unwrap_or(i64::max_value());
                            let d = r.duration_90k as i64;
                            if s.start_time <= cur_off + d && cur_off < end_time {
                                let start = cmp::max(0, s.start_time - cur_off);
                                let end = cmp::min(d, end_time - cur_off);
                                let times = start as i32 .. end as i32;
                                debug!("...appending recording {}/{} with times {:?} (out of dur {})",
                                       r.camera_id, r.id, times, d);
                                builder.append(&db, r, start as i32 .. end as i32)?;
                            } else {
                                debug!("...skipping recording {}/{} dur {}", r.camera_id, r.id, d);
                            }
                            cur_off += d;
                            Ok(())
                        })?;

                        // Check for missing recordings.
                        match prev {
                            Some(id) if s.ids.end != id + 1 => {
                                return Err(Error::new(format!("no such recording {}/{}",
                                                              camera_id, s.ids.end - 1)));
                            },
                            None => {
                                return Err(Error::new(format!("no such recording {}/{}",
                                                              camera_id, s.ids.start)));
                            },
                            _ => {},
                        };
                        if let Some(end) = s.end_time {
                            if end > cur_off {
                                return Err(Error::new(
                                        format!("end time {} is beyond specified recordings", end)));
                            }
                        }
                    },
                    "ts" => builder.include_timestamp_subtitle_track(value == "true"),
                    _ => return Err(Error::new(format!("parameter {} not understood", key))),
                }
            };
        }
        let mp4 = builder.build(self.db.clone(), self.dir.clone())?;
        Ok(http_entity::serve(mp4, req))
    }

    /// Parses optional `start_time_90k` and `end_time_90k` query parameters, defaulting to the
    /// full range of possible values.
    fn get_optional_range(query: Option<&str>) -> Result<Range<recording::Time>, Error> {
        let mut start = i64::min_value();
        let mut end = i64::max_value();
        if let Some(q) = query {
            for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                let (key, value) = (key.borrow(), value.borrow());
                match key {
                    "start_time_90k" => start = i64::from_str(value)?,
                    "end_time_90k" => end = i64::from_str(value)?,
                    _ => {},
                }
            };
        }
        Ok(recording::Time(start) .. recording::Time(end))
    }
}

impl server::Service for Service {
    type Request = Request;
    type Response = Response<slices::Body>;
    type Error = hyper::Error;
    type Future = future::FutureResult<Self::Response, Self::Error>;

    fn call(&self, req: Request) -> Self::Future {
        debug!("request on: {}", req.uri());
        let res = match decode_path(req.uri().path()) {
            Path::InitSegment(sha1) => self.init_segment(sha1, &req),
            Path::CamerasList => self.list_cameras(&req),
            Path::Camera(uuid) => self.camera(uuid, req.uri().query(), &req),
            Path::CameraRecordings(uuid) => self.camera_recordings(uuid, req.uri().query(), &req),
            Path::CameraViewMp4(uuid) => {
                self.camera_view_mp4(uuid, mp4::Type::Normal, req.uri().query(), &req)
            },
            Path::CameraViewMp4Segment(uuid) => {
                self.camera_view_mp4(uuid, mp4::Type::MediaSegment, req.uri().query(), &req)
            },
            Path::NotFound => self.not_found(),
        };
        future::result(res.map_err(|e| {
            error!("error: {}", e);
            hyper::Error::Incomplete
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{HtmlEscaped, Humanized, Segments};
    use testutil;

    #[test]
    fn test_humanize() {
        testutil::init();
        assert_eq!("1.0 B",    format!("{:b}B", Humanized(1)));
        assert_eq!("1.0 EiB",  format!("{:b}B", Humanized(1i64 << 60)));
        assert_eq!("1.5 EiB",  format!("{:b}B", Humanized((1i64 << 60) + (1i64 << 59))));
        assert_eq!("8.0 EiB", format!("{:b}B", Humanized(i64::max_value())));
        assert_eq!("1.0 Mbps", format!("{}bps", Humanized(1_000_000)));
    }

    #[test]
    fn test_html_escaped() {
        testutil::init();
        assert_eq!("", format!("{}", HtmlEscaped("")));
        assert_eq!("no special chars", format!("{}", HtmlEscaped("no special chars")));
        assert_eq!("a &lt;tag> &amp; text", format!("{}", HtmlEscaped("a <tag> & text")));
    }

    #[test]
    fn test_segments() {
        testutil::init();
        assert_eq!(Segments{ids: 1..2, start_time: 0, end_time: None},
                   Segments::parse("1").unwrap());
        assert_eq!(Segments{ids: 1..2, start_time: 26, end_time: None},
                   Segments::parse("1.26-").unwrap());
        assert_eq!(Segments{ids: 1..2, start_time: 0, end_time: Some(42)},
                   Segments::parse("1.-42").unwrap());
        assert_eq!(Segments{ids: 1..2, start_time: 26, end_time: Some(42)},
                   Segments::parse("1.26-42").unwrap());
        assert_eq!(Segments{ids: 1..6, start_time: 0, end_time: None},
                   Segments::parse("1-5").unwrap());
        assert_eq!(Segments{ids: 1..6, start_time: 26, end_time: None},
                   Segments::parse("1-5.26-").unwrap());
        assert_eq!(Segments{ids: 1..6, start_time: 0, end_time: Some(42)},
                   Segments::parse("1-5.-42").unwrap());
        assert_eq!(Segments{ids: 1..6, start_time: 26, end_time: Some(42)},
                   Segments::parse("1-5.26-42").unwrap());
    }
}

#[cfg(all(test, feature="nightly"))]
mod bench {
    extern crate reqwest;
    extern crate test;

    use hyper;
    use self::test::Bencher;
    use testutil::{self, TestDb};

    struct Server {
        base_url: String,
    }

    impl Server {
        fn new() -> Server {
            let db = TestDb::new();
            testutil::add_dummy_recordings_to_db(&db.db, 1440);
            let (tx, rx) = ::std::sync::mpsc::channel();
            ::std::thread::spawn(move || {
                let addr = "127.0.0.1:0".parse().unwrap();
                let (db, dir) = (db.db.clone(), db.dir.clone());
                let server = hyper::server::Http::new()
                    .bind(&addr, move || Ok(super::Service::new(db.clone(), dir.clone())))
                    .unwrap();
                tx.send(server.local_addr().unwrap()).unwrap();
                server.run().unwrap();
            });
            let addr = rx.recv().unwrap();
            Server{base_url: format!("http://{}:{}", addr.ip(), addr.port())}
        }
    }

    lazy_static! {
        static ref SERVER: Server = { Server::new() };
    }

    #[bench]
    fn serve_camera_html(b: &mut Bencher) {
        testutil::init();
        let server = &*SERVER;
        let url = reqwest::Url::parse(&format!("{}/cameras/{}/", server.base_url,
                                               *testutil::TEST_CAMERA_UUID)).unwrap();
        let mut buf = Vec::new();
        let client = reqwest::Client::new().unwrap();
        let mut f = || {
            let mut resp = client.get(url.clone()).unwrap().send().unwrap();
            assert_eq!(resp.status(), reqwest::StatusCode::Ok);
            buf.clear();
            use std::io::Read;
            resp.read_to_end(&mut buf).unwrap();
        };
        f();  // warm.
        b.iter(f);
    }
}
