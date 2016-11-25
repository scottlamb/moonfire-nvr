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

use db::{Camera, Database};
use dir;
use error::Error;
use h264;
use recording;
use std::result::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use stream::StreamSource;
use time;

pub static ROTATE_INTERVAL_SEC: i64 = 60;

pub struct Streamer {
    shutdown: Arc<AtomicBool>,

    // State below is only used by the thread in Run.
    rotate_offset_sec: i64,
    db: Arc<Database>,
    dir: Arc<dir::SampleFileDir>,
    syncer_channel: dir::SyncerChannel,
    camera_id: i32,
    short_name: String,
    url: String,
    redacted_url: String,
}

impl Streamer {
    pub fn new(db: Arc<Database>, dir: Arc<dir::SampleFileDir>, syncer_channel: dir::SyncerChannel,
               shutdown: Arc<AtomicBool>, camera_id: i32, c: &Camera, rotate_offset_sec: i64)
               -> Self {
        Streamer{
            shutdown: shutdown,
            rotate_offset_sec: rotate_offset_sec,
            db: db,
            dir: dir,
            syncer_channel: syncer_channel,
            camera_id: camera_id,
            short_name: c.short_name.to_owned(),
            url: format!("rtsp://{}:{}@{}{}", c.username, c.password, c.host, c.main_rtsp_path),
            redacted_url: format!("rtsp://{}:redacted@{}{}", c.username, c.host, c.main_rtsp_path),
        }
    }

    pub fn short_name(&self) -> &str { &self.short_name }

    pub fn run(&mut self) {
        while !self.shutdown.load(Ordering::SeqCst) {
            if let Err(e) = self.run_once() {
                let sleep_time = Duration::from_secs(1);
                warn!("{}: sleeping for {:?} after error: {}", self.short_name, sleep_time, e);
                thread::sleep(sleep_time);
            }
        }
        info!("{}: shutting down", self.short_name);
    }

    fn run_once(&mut self) -> Result<(), Error> {
        info!("{}: Opening input: {}", self.short_name, self.redacted_url);

        // TODO: mockability?
        let mut stream = StreamSource::Rtsp(&self.url).open()?;
        // TODO: verify time base.
        // TODO: verify width/height.
        let extra_data = stream.get_extra_data()?;
        let video_sample_entry_id =
            self.db.lock().insert_video_sample_entry(extra_data.width, extra_data.height,
                                                     &extra_data.sample_entry)?;
        debug!("{}: video_sample_entry_id={}", self.short_name, video_sample_entry_id);
        let mut seen_key_frame = false;
        let mut rotate = None;
        let mut writer: Option<recording::Writer> = None;
        let mut transformed = Vec::new();
        let mut next_start = None;
        while !self.shutdown.load(Ordering::SeqCst) {
            let pkt = stream.get_next()?;
            if !seen_key_frame && !pkt.is_key() {
                continue;
            } else if !seen_key_frame {
                debug!("{}: have first key frame", self.short_name);
                seen_key_frame = true;
            }
            let frame_realtime = time::get_time();
            if let Some(r) = rotate {
                if frame_realtime.sec > r && pkt.is_key() {
                    let w = writer.take().expect("rotate set implies writer is set");
                    next_start = Some(w.end());
                    // TODO: restore this log message.
                    // info!("{}: wrote {}: [{}, {})", self.short_name, r.sample_file_uuid,
                    //       r.time.start, r.time.end);
                    self.syncer_channel.async_save_writer(w)?;
                }
            };
            let mut w = match writer {
                Some(w) => w,
                None => {
                    let r = frame_realtime.sec -
                            (frame_realtime.sec % ROTATE_INTERVAL_SEC) +
                            self.rotate_offset_sec;
                    rotate = Some(
                        if r <= frame_realtime.sec { r + ROTATE_INTERVAL_SEC } else { r });
                    let local_realtime = recording::Time::new(frame_realtime);

                    self.dir.create_writer(next_start.unwrap_or(local_realtime), local_realtime,
                                           self.camera_id, video_sample_entry_id)?
                },
            };
            let orig_data = match pkt.data() {
                Some(d) => d,
                None => return Err(Error::new("packet has no data".to_owned())),
            };
            let transformed_data = if extra_data.need_transform {
                h264::transform_sample_data(orig_data, &mut transformed)?;
                transformed.as_slice()
            } else {
                orig_data
            };
            w.write(transformed_data, pkt.duration() as i32, pkt.is_key())?;
            writer = Some(w);
        }
        if let Some(w) = writer {
            self.syncer_channel.async_save_writer(w)?;
        }
        Ok(())
    }
}
