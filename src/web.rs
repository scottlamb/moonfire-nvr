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
use futures_cpupool;
use json;
use http_entity;
use http_file;
use hyper::header;
use hyper::server::{self, Request, Response};
use mime;
use mp4;
use recording;
use reffers::ARefs;
use regex::Regex;
use serde_json;
use slices;
use std::collections::HashMap;
use std::cmp;
use std::fs;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use strutil;
use url::form_urlencoded;
use uuid::Uuid;

lazy_static! {
    /// Regex used to parse the `s` query parameter to `view.mp4`.
    /// As described in `design/api.md`, this is of the form
    /// `START_ID[-END_ID][.[REL_START_TIME]-[REL_END_TIME]]`.
    static ref SEGMENTS_RE: Regex = Regex::new(r"^(\d+)(-\d+)?(?:\.(\d+)?-(\d+)?)?$").unwrap();
}

enum Path {
    TopLevel,                       // "/api/"
    InitSegment([u8; 20]),          // "/api/init/<sha1>.mp4"
    Camera(Uuid),                   // "/api/cameras/<uuid>/"
    CameraRecordings(Uuid),         // "/api/cameras/<uuid>/recordings"
    CameraViewMp4(Uuid),            // "/api/cameras/<uuid>/view.mp4"
    CameraViewMp4Segment(Uuid),     // "/api/cameras/<uuid>/view.m4s"
    Static,                         // "<other path>"
    NotFound,
}

fn decode_path(path: &str) -> Path {
    if !path.starts_with("/api/") {
        return Path::Static;
    }
    let path = &path["/api".len()..];
    if path == "/" {
        return Path::TopLevel;
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

/// A user interface file (.html, .js, etc).
/// The list of files is loaded into the server at startup; this makes path canonicalization easy.
/// The files themselves are opened on every request so they can be changed during development.
#[derive(Debug)]
struct UiFile {
    mime: mime::Mime,
    path: PathBuf,
}

struct ServiceInner {
    db: Arc<db::Database>,
    dir: Arc<SampleFileDir>,
    ui_files: HashMap<String, UiFile>,
    pool: futures_cpupool::CpuPool,
    time_zone_name: String,
}

impl ServiceInner {
    fn not_found(&self) -> Result<Response<slices::Body>, Error> {
        let body: slices::Body = Box::new(stream::once(Ok(ARefs::new(&b"not found"[..]))));
        Ok(Response::new()
            .with_status(hyper::StatusCode::NotFound)
            .with_header(header::ContentType(mime::TEXT_PLAIN))
            .with_body(body))
    }

    fn top_level(&self, query: Option<&str>) -> Result<Response<slices::Body>, Error> {
        let mut days = false;
        if let Some(q) = query {
            for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                let (key, value) : (_, &str) = (key.borrow(), value.borrow());
                match key {
                    "days" => days = value == "true",
                    _ => {},
                };
            }
        }

        let buf = {
            let db = self.db.lock();
            serde_json::to_vec(&json::TopLevel {
                time_zone_name: &self.time_zone_name,
                cameras: (db.cameras_by_id(), days),
            })?
        };
        let len = buf.len();
        let body: slices::Body = Box::new(stream::once(Ok(ARefs::new(buf))));
        Ok(Response::new()
           .with_header(header::ContentType(mime::APPLICATION_JSON))
           .with_header(header::ContentLength(len as u64))
           .with_body(body))
    }

    fn camera(&self, uuid: Uuid) -> Result<Response<slices::Body>, Error> {
        let buf = {
            let db = self.db.lock();
            let camera = db.get_camera(uuid)
                           .ok_or_else(|| Error::new("no such camera".to_owned()))?;
            serde_json::to_vec(&json::Camera::new(camera, true))?
        };
        let len = buf.len();
        let body: slices::Body = Box::new(stream::once(Ok(ARefs::new(buf))));
        Ok(Response::new()
           .with_header(header::ContentType(mime::APPLICATION_JSON))
           .with_header(header::ContentLength(len as u64))
           .with_body(body))
    }

    fn camera_recordings(&self, uuid: Uuid, query: Option<&str>)
                         -> Result<Response<slices::Body>, Error> {
        let (r, split) = {
            let mut time = recording::Time(i64::min_value()) .. recording::Time(i64::max_value());
            let mut split = recording::Duration(i64::max_value());
            if let Some(q) = query {
                for (key, value) in form_urlencoded::parse(q.as_bytes()) {
                    let (key, value) = (key.borrow(), value.borrow());
                    match key {
                        "startTime90k" => time.start = recording::Time::parse(value)?,
                        "endTime90k" => time.end = recording::Time::parse(value)?,
                        "split90k" => split = recording::Duration(i64::from_str(value)?),
                        _ => {},
                    }
                };
            }
            (time, split)
        };
        let mut out = json::ListRecordings{recordings: Vec::new()};
        {
            let db = self.db.lock();
            let camera = db.get_camera(uuid)
                           .ok_or_else(|| Error::new("no such camera".to_owned()))?;
            db.list_aggregated_recordings(camera.id, r, split, |row| {
                let end = row.ids.end - 1;  // in api, ids are inclusive.
                out.recordings.push(json::Recording {
                    start_id: row.ids.start,
                    end_id: if end == row.ids.start + 1 { None } else { Some(end) },
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

    fn static_file(&self, req: &Request) -> Result<Response<slices::Body>, Error> {
        let s = match self.ui_files.get(req.uri().path()) {
            None => { return self.not_found() },
            Some(s) => s,
        };
        let f = fs::File::open(&s.path)?;
        let e = http_file::ChunkedReadFile::new(f, Some(self.pool.clone()), s.mime.clone())?;
        Ok(http_entity::serve(e, &req))
    }
}

#[derive(Clone)]
pub struct Service(Arc<ServiceInner>);

impl Service {
    pub fn new(db: Arc<db::Database>, dir: Arc<SampleFileDir>, ui_dir: Option<&str>, zone: String)
               -> Result<Self, Error> {
        let mut ui_files = HashMap::new();
        if let Some(d) = ui_dir {
            Service::fill_ui_files(d, &mut ui_files);
        }
        debug!("UI files: {:#?}", ui_files);
        Ok(Service(Arc::new(ServiceInner {
            db,
            dir,
            ui_files,
            pool: futures_cpupool::Builder::new().pool_size(1).name_prefix("static").create(),
            time_zone_name: zone,
        })))
    }

    fn fill_ui_files(dir: &str, files: &mut HashMap<String, UiFile>) {
        let r = match fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) => {
                warn!("Unable to search --ui-dir={}; will serve no static files. Error was: {}",
                      dir, e);
                return;
            }
        };
        for e in r {
            let e = match e {
                Ok(e) => e,
                Err(e) => {
                    warn!("Error searching UI directory; may be missing files. Error was: {}", e);
                    continue;
                },
            };
            let (p, mime) = match e.file_name().to_str() {
                Some(n) if n == "index.html" => ("/".to_owned(), mime::TEXT_HTML),
                Some(n) if n.ends_with(".js") => (format!("/{}", n), mime::TEXT_JAVASCRIPT),
                Some(n) if n.ends_with(".html") => (format!("/{}", n), mime::TEXT_HTML),
                Some(n) if n.ends_with(".png") => (format!("/{}", n), mime::IMAGE_PNG),
                Some(n) => {
                    warn!("UI directory file {:?} has unknown extension; skipping", n);
                    continue;
                },
                None => {
                    warn!("UI directory file {:?} is not a valid UTF-8 string; skipping",
                          e.file_name());
                    continue;
                },
            };
            files.insert(p, UiFile {
                mime,
                path: e.path(),
            });
        }
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
            Path::InitSegment(sha1) => self.0.init_segment(sha1, &req),
            Path::TopLevel => self.0.top_level(req.uri().query()),
            Path::Camera(uuid) => self.0.camera(uuid),
            Path::CameraRecordings(uuid) => self.0.camera_recordings(uuid, req.uri().query()),
            Path::CameraViewMp4(uuid) => {
                self.0.camera_view_mp4(uuid, mp4::Type::Normal, req.uri().query(), &req)
            },
            Path::CameraViewMp4Segment(uuid) => {
                self.0.camera_view_mp4(uuid, mp4::Type::MediaSegment, req.uri().query(), &req)
            },
            Path::NotFound => self.0.not_found(),
            Path::Static => self.0.static_file(&req),
        };
        future::result(res.map_err(|e| {
            error!("error: {}", e);
            hyper::Error::Incomplete
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::Segments;
    use testutil;

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
                let service = super::Service::new(db.clone(), dir.clone(), None);
                let server = hyper::server::Http::new()
                    .bind(&addr, move || Ok(service.clone()))
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
    fn serve_camera_recordings(b: &mut Bencher) {
        testutil::init();
        let server = &*SERVER;
        let url = reqwest::Url::parse(&format!("{}/cameras/{}/recordings", server.base_url,
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
