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
use error::{Error, Result};
use hyper::{header,server,status};
use hyper::uri::RequestUri;
use mime;
use mp4;
use recording;
use resource;
use serde_json;
use serde::ser::Serializer;
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::result;
use std::sync::{Arc,MutexGuard};
use time;
use url::form_urlencoded;
use uuid::Uuid;

const BINARY_PREFIXES: &'static [&'static str] = &[" ", " Ki", " Mi", " Gi", " Ti", " Pi", " Ei"];
const DECIMAL_PREFIXES: &'static [&'static str] =&[" ", " k", " M", " G", " T", " P", " E"];

lazy_static! {
    static ref JSON: mime::Mime = mime!(Application/Json);
    static ref HTML: mime::Mime = mime!(Text/Html);
}

enum Path {
    CamerasList,              // "/" or "/cameras/"
    Camera(Uuid),             // "/cameras/<uuid>/"
    CameraRecordings(Uuid),   // "/cameras/<uuid>/recordings"
    CameraViewMp4(Uuid),      // "/cameras/<uuid>/view.mp4"
    NotFound,
}

fn get_path_and_query(uri: &RequestUri) -> (&str, &str) {
    match *uri {
        RequestUri::AbsolutePath(ref both) => match both.find('?') {
            Some(split) => (&both[..split], &both[split+1..]),
            None => (both, ""),
        },
        RequestUri::AbsoluteUri(ref u) => (u.path(), u.query().unwrap_or("")),
        _ => ("", ""),
    }
}

fn decode_path(path: &str) -> Path {
    if path == "/" {
        return Path::CamerasList;
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
        _ => Path::NotFound,
    }
}

fn is_json(req: &server::Request) -> bool {
    if let Some(accept) = req.headers.get::<header::Accept>() {
        return accept.len() == 1 && accept[0].item == *JSON &&
               accept[0].quality == header::Quality(1000);
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

pub struct Handler {
    db: Arc<db::Database>,
    dir: Arc<SampleFileDir>,
}

#[derive(Serialize)]
struct ListCameras<'a> {
    // Use a custom serializer which presents the map's values as a sequence.
    #[serde(serialize_with = "ListCameras::serialize_cameras")]
    cameras: &'a BTreeMap<i32, db::Camera>,
}

impl<'a> ListCameras<'a> {
    fn serialize_cameras<S>(cameras: &BTreeMap<i32, db::Camera>,
                            serializer: &mut S) -> result::Result<(), S::Error>
    where S: Serializer {
        let mut state = serializer.serialize_seq(Some(cameras.len()))?;
        for c in cameras.values() {
            serializer.serialize_seq_elt(&mut state, c)?;
        }
        serializer.serialize_seq_end(state)
    }
}

impl Handler {
    pub fn new(db: Arc<db::Database>, dir: Arc<SampleFileDir>) -> Self {
        Handler{db: db, dir: dir}
    }

    fn not_found(&self, mut res: server::Response) -> Result<()> {
        *res.status_mut() = status::StatusCode::NotFound;
        res.send(b"not found")?;
        Ok(())
    }

    fn list_cameras(&self, req: &server::Request, mut res: server::Response) -> Result<()> {
        let json = is_json(req);
        let buf = {
            let db = self.db.lock();
            if json {
                serde_json::to_vec(&ListCameras{cameras: db.cameras_by_id()})?
            } else {
                self.list_cameras_html(db)?
            }
        };
        res.headers_mut().set(header::ContentType(if json { JSON.clone() } else { HTML.clone() }));
        res.send(&buf)?;
        Ok(())
    }

    fn list_cameras_html(&self, db: MutexGuard<db::LockedDatabase>) -> Result<Vec<u8>> {
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

    fn camera(&self, uuid: Uuid, req: &server::Request, mut res: server::Response) -> Result<()> {
        let json = is_json(req);
        let buf = {
            let db = self.db.lock();
            if json {
                let camera = db.get_camera(uuid)
                               .ok_or_else(|| Error::new("no such camera".to_owned()))?;
                serde_json::to_vec(&camera)?
            } else {
                self.camera_html(db, uuid)?
            }
        };
        res.headers_mut().set(header::ContentType(if json { JSON.clone() } else { HTML.clone() }));
        res.send(&buf)?;
        Ok(())
    }

    fn camera_html(&self, db: MutexGuard<db::LockedDatabase>, uuid: Uuid) -> Result<Vec<u8>> {
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
        let r = recording::Time(i64::min_value()) .. recording::Time(i64::max_value());

        // Rather than listing each 60-second recording, generate a HTML row for aggregated .mp4
        // files of up to FORCE_SPLIT_DURATION each, provided there is no gap or change in video
        // parameters between recordings.
        static FORCE_SPLIT_DURATION: recording::Duration =
            recording::Duration(60 * 60 * recording::TIME_UNITS_PER_SEC);
        db.list_aggregated_recordings(camera.id, &r, FORCE_SPLIT_DURATION, |row| {
            let seconds = (row.range.end.0 - row.range.start.0) / recording::TIME_UNITS_PER_SEC;
            write!(&mut buf, "\
                <tr><td><a href=\"view.mp4?start_time_90k={}&end_time_90k={}\">{}</a></td>\
                <td>{}</td><td>{}x{}</td><td>{:.0}</td><td>{:b}B</td><td>{}bps</td></tr>\n",
                row.range.start.0, row.range.end.0,
                HumanizedTimestamp(Some(row.range.start)),
                HumanizedTimestamp(Some(row.range.end)), row.video_sample_entry.width,
                row.video_sample_entry.height,
                if seconds == 0 { 0. } else { row.video_samples as f32 / seconds as f32 },
                Humanized(row.sample_file_bytes),
                Humanized(if seconds == 0 { 0 } else { row.sample_file_bytes * 8 / seconds }))?;
            Ok(())
        })?;
        buf.extend_from_slice(b"</table>\n</html>\n");
        Ok(buf)
    }

    fn camera_recordings(&self, _uuid: Uuid, _req: &server::Request,
                         mut res: server::Response) -> Result<()> {
        *res.status_mut() = status::StatusCode::NotImplemented;
        res.send(b"not implemented")?;
        Ok(())
    }

    fn camera_view_mp4(&self, uuid: Uuid, query: &str, req: &server::Request,
                       res: server::Response) -> Result<()> {
        let camera_id = {
            let db = self.db.lock();
            let camera = db.get_camera(uuid)
                           .ok_or_else(|| Error::new("no such camera".to_owned()))?;
            camera.id
        };
        let mut start = None;
        let mut end = None;
        let mut include_ts = false;
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            let (key, value) = (key.borrow(), value.borrow());
            match key {
                "start_time_90k" => start = Some(recording::Time(i64::from_str(value)?)),
                "end_time_90k" => end = Some(recording::Time(i64::from_str(value)?)),
                "ts" => { include_ts = value == "true"; },
                _ => {},
            }
        };
        let start = start.ok_or_else(|| Error::new("start_time_90k missing".to_owned()))?;
        let end = end.ok_or_else(|| Error::new("end_time_90k missing".to_owned()))?;
        let desired_range = start .. end;
        let mut builder = mp4::Mp4FileBuilder::new();

        // There should be roughly ceil((end - start) / desired_recording_duration) recordings
        // in the desired timespan if there are no gaps or overlap. Add a couple more to be safe:
        // one for misalignment of the requested timespan with the rotate offset, another because
        // rotation only happens at key frames.
        let ceil_durations = ((end - start).0 + recording::DESIRED_RECORDING_DURATION - 1) /
                             recording::DESIRED_RECORDING_DURATION;
        let est_records = (ceil_durations + 2) as usize;
        let mut next_start = start;
        builder.reserve(est_records);
        {
            let db = self.db.lock();
            db.list_recordings(camera_id, &desired_range, |r| {
                if builder.len() == 0 && r.start > next_start {
                    return Err(Error::new(format!("recording started late ({} vs requested {})",
                                                  r.start, start)));
                } else if builder.len() != 0 && r.start != next_start {
                    return Err(Error::new(format!("gap/overlap in recording: {} to {} after row {}",
                                                  next_start, r.start, builder.len())));
                }
                next_start = r.start + recording::Duration(r.duration_90k as i64);
                // TODO: check for inconsistent video sample entries.

                let rel_start = if r.start < start {
                    (start - r.start).0 as i32
                } else {
                    0
                };
                let rel_end = if r.start + recording::Duration(r.duration_90k as i64) > end {
                    (end - r.start).0 as i32
                } else {
                    r.duration_90k
                };
                builder.append(&db, r, rel_start .. rel_end)?;
                Ok(())
            })?;
        }
        if next_start < end {
            return Err(Error::new(format!(
                        "recording ends early: {}, not requested: {} after {} rows.",
                        next_start, end, builder.len())))
        }
        if builder.len() > est_records {
            warn!("Estimated {} records for time [{}, {}); actually were {}",
                  est_records, start, end, builder.len());
        } else {
            debug!("Estimated {} records for time [{}, {}); actually were {}",
                   est_records, start, end, builder.len());
        }
        builder.include_timestamp_subtitle_track(include_ts);
        let mp4 = builder.build(self.db.clone(), self.dir.clone())?;
        resource::serve(&mp4, req, res)?;
        Ok(())
    }
}

impl server::Handler for Handler {
    fn handle(&self, req: server::Request, res: server::Response) {
        let (path, query) = get_path_and_query(&req.uri);
        let res = match decode_path(path) {
            Path::CamerasList => self.list_cameras(&req, res),
            Path::Camera(uuid) => self.camera(uuid, &req, res),
            Path::CameraRecordings(uuid) => self.camera_recordings(uuid, &req, res),
            Path::CameraViewMp4(uuid) => self.camera_view_mp4(uuid, query, &req, res),
            Path::NotFound => self.not_found(res),
        };
        if let Err(ref e) = res {
            warn!("Error handling request: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HtmlEscaped, Humanized};

    #[test]
    fn test_humanize() {
        assert_eq!("1.0 B",    format!("{:b}B", Humanized(1)));
        assert_eq!("1.0 EiB",  format!("{:b}B", Humanized(1i64 << 60)));
        assert_eq!("1.5 EiB",  format!("{:b}B", Humanized((1i64 << 60) + (1i64 << 59))));
        assert_eq!("8.0 EiB", format!("{:b}B", Humanized(i64::max_value())));
        assert_eq!("1.0 Mbps", format!("{}bps", Humanized(1_000_000)));
    }

    #[test]
    fn test_html_escaped() {
        assert_eq!("", format!("{}", HtmlEscaped("")));
        assert_eq!("no special chars", format!("{}", HtmlEscaped("no special chars")));
        assert_eq!("a &lt;tag> &amp; text", format!("{}", HtmlEscaped("a <tag> & text")));
    }
}
