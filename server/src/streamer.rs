// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use crate::h264;
use crate::stream;
use base::clock::{Clocks, TimerGuard};
use db::{dir, recording, writer, Camera, Database, Stream};
use failure::{bail, format_err, Error};
use log::{debug, info, trace, warn};
use std::result::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use url::Url;

pub static ROTATE_INTERVAL_SEC: i64 = 60;

/// Common state that can be used by multiple `Streamer` instances.
pub struct Environment<'a, 'b, C, S>
where
    C: Clocks + Clone,
    S: 'a + stream::Stream,
{
    pub opener: &'a dyn stream::Opener<S>,
    pub db: &'b Arc<Database<C>>,
    pub shutdown: &'b Arc<AtomicBool>,
}

/// Connects to a given RTSP stream and writes recordings to the database via [`writer::Writer`].
/// Streamer is meant to be long-lived; it will sleep and retry after each failure.
pub struct Streamer<'a, C, S>
where
    C: Clocks + Clone,
    S: 'a + stream::Stream,
{
    shutdown: Arc<AtomicBool>,

    // State below is only used by the thread in Run.
    rotate_offset_sec: i64,
    rotate_interval_sec: i64,
    db: Arc<Database<C>>,
    dir: Arc<dir::SampleFileDir>,
    syncer_channel: writer::SyncerChannel<::std::fs::File>,
    opener: &'a dyn stream::Opener<S>,
    stream_id: i32,
    short_name: String,
    url: Url,
    redacted_url: Url,
}

impl<'a, C, S> Streamer<'a, C, S>
where
    C: 'a + Clocks + Clone,
    S: 'a + stream::Stream,
{
    pub fn new<'b>(
        env: &Environment<'a, 'b, C, S>,
        dir: Arc<dir::SampleFileDir>,
        syncer_channel: writer::SyncerChannel<::std::fs::File>,
        stream_id: i32,
        c: &Camera,
        s: &Stream,
        rotate_offset_sec: i64,
        rotate_interval_sec: i64,
    ) -> Result<Self, Error> {
        let mut url = Url::parse(&s.rtsp_url)?;
        let mut redacted_url = url.clone();
        if !c.username.is_empty() {
            url.set_username(&c.username)
                .map_err(|_| format_err!("can't set username"))?;
            redacted_url.set_username(&c.username).unwrap();
            url.set_password(Some(&c.password)).unwrap();
            redacted_url.set_password(Some("redacted")).unwrap();
        }
        Ok(Streamer {
            shutdown: env.shutdown.clone(),
            rotate_offset_sec,
            rotate_interval_sec,
            db: env.db.clone(),
            dir,
            syncer_channel,
            opener: env.opener,
            stream_id,
            short_name: format!("{}-{}", c.short_name, s.type_.as_str()),
            url,
            redacted_url,
        })
    }

    pub fn short_name(&self) -> &str {
        &self.short_name
    }

    pub fn run(&mut self) {
        while !self.shutdown.load(Ordering::SeqCst) {
            if let Err(e) = self.run_once() {
                let sleep_time = time::Duration::seconds(1);
                warn!(
                    "{}: sleeping for {:?} after error: {}",
                    self.short_name,
                    sleep_time,
                    base::prettify_failure(&e)
                );
                self.db.clocks().sleep(sleep_time);
            }
        }
        info!("{}: shutting down", self.short_name);
    }

    fn run_once(&mut self) -> Result<(), Error> {
        info!("{}: Opening input: {}", self.short_name, self.redacted_url);
        let clocks = self.db.clocks();

        let mut stream = {
            let _t = TimerGuard::new(&clocks, || format!("opening {}", self.redacted_url));
            self.opener.open(stream::Source::Rtsp {
                url: self.url.as_str(),
                redacted_url: self.redacted_url.as_str(),
            })?
        };
        let realtime_offset = self.db.clocks().realtime() - clocks.monotonic();
        let extra_data = stream.get_extra_data()?;
        let video_sample_entry_id = {
            let _t = TimerGuard::new(&clocks, || "inserting video sample entry");
            self.db.lock().insert_video_sample_entry(extra_data.entry)?
        };
        let mut seen_key_frame = false;

        // Seconds since epoch at which to next rotate.
        let mut rotate: Option<i64> = None;
        let mut transformed = Vec::new();
        let mut w = writer::Writer::new(
            &self.dir,
            &self.db,
            &self.syncer_channel,
            self.stream_id,
            video_sample_entry_id,
        );
        while !self.shutdown.load(Ordering::SeqCst) {
            let pkt = {
                let _t = TimerGuard::new(&clocks, || "getting next packet");
                stream.get_next()?
            };
            let pts = pkt.pts().ok_or_else(|| format_err!("packet with no pts"))?;
            if !seen_key_frame && !pkt.is_key() {
                continue;
            } else if !seen_key_frame {
                debug!("{}: have first key frame", self.short_name);
                seen_key_frame = true;
            }
            let frame_realtime = clocks.monotonic() + realtime_offset;
            let local_time = recording::Time::new(frame_realtime);
            rotate = if let Some(r) = rotate {
                if frame_realtime.sec > r && pkt.is_key() {
                    trace!("{}: write on normal rotation", self.short_name);
                    let _t = TimerGuard::new(&clocks, || "closing writer");
                    w.close(Some(pts))?;
                    None
                } else {
                    Some(r)
                }
            } else {
                None
            };
            let r = match rotate {
                Some(r) => r,
                None => {
                    let sec = frame_realtime.sec;
                    let r = sec - (sec % self.rotate_interval_sec) + self.rotate_offset_sec;
                    let r = r + if r <= sec {
                        self.rotate_interval_sec
                    } else {
                        0
                    };

                    // On the first recording, set rotate time to not the next rotate offset, but
                    // the one after, so that it's longer than usual rather than shorter than
                    // usual.  This ensures there's plenty of frame times to use when calculating
                    // the start time.
                    let r = r + if w.previously_opened()? {
                        0
                    } else {
                        self.rotate_interval_sec
                    };
                    let _t = TimerGuard::new(&clocks, || "creating writer");
                    r
                }
            };
            let orig_data = match pkt.data() {
                Some(d) => d,
                None => bail!("packet has no data"),
            };
            let transformed_data = if extra_data.need_transform {
                h264::transform_sample_data(orig_data, &mut transformed)?;
                transformed.as_slice()
            } else {
                orig_data
            };
            let _t = TimerGuard::new(&clocks, || {
                format!("writing {} bytes", transformed_data.len())
            });
            w.write(transformed_data, local_time, pts, pkt.is_key())?;
            rotate = Some(r);
        }
        if rotate.is_some() {
            let _t = TimerGuard::new(&clocks, || "closing writer");
            w.close(None)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::h264;
    use crate::stream::{self, Opener, Stream};
    use base::clock::{self, Clocks};
    use db::{recording, testutil, CompositeId};
    use failure::{bail, Error};
    use log::trace;
    use parking_lot::Mutex;
    use std::cmp;
    use std::convert::TryFrom;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use time;

    struct ProxyingStream<'a> {
        clocks: &'a clock::SimulatedClocks,
        inner: stream::FfmpegStream,
        buffered: time::Duration,
        slept: time::Duration,
        ts_offset: i64,
        ts_offset_pkts_left: u32,
        pkts_left: u32,
    }

    impl<'a> ProxyingStream<'a> {
        fn new(
            clocks: &'a clock::SimulatedClocks,
            buffered: time::Duration,
            inner: stream::FfmpegStream,
        ) -> ProxyingStream {
            clocks.sleep(buffered);
            ProxyingStream {
                clocks: clocks,
                inner: inner,
                buffered: buffered,
                slept: time::Duration::seconds(0),
                ts_offset: 0,
                ts_offset_pkts_left: 0,
                pkts_left: 0,
            }
        }
    }

    impl<'a> Stream for ProxyingStream<'a> {
        fn get_next(&mut self) -> Result<ffmpeg::avcodec::Packet, ffmpeg::Error> {
            if self.pkts_left == 0 {
                return Err(ffmpeg::Error::eof());
            }
            self.pkts_left -= 1;

            let mut pkt = self.inner.get_next()?;

            // Emulate the behavior of real cameras that send some pre-buffered frames immediately
            // on connect. After that, advance clock to the end of this frame.
            // Avoid accumulating conversion error by tracking the total amount to sleep and how
            // much we've already slept, rather than considering each frame in isolation.
            {
                let goal = pkt.pts().unwrap() + pkt.duration() as i64;
                let goal = time::Duration::nanoseconds(
                    goal * 1_000_000_000 / recording::TIME_UNITS_PER_SEC,
                );
                let duration = goal - self.slept;
                let buf_part = cmp::min(self.buffered, duration);
                self.buffered = self.buffered - buf_part;
                self.clocks.sleep(duration - buf_part);
                self.slept = goal;
            }

            if self.ts_offset_pkts_left > 0 {
                self.ts_offset_pkts_left -= 1;
                let old_pts = pkt.pts().unwrap();
                let old_dts = pkt.dts();
                pkt.set_pts(Some(old_pts + self.ts_offset));
                pkt.set_dts(old_dts + self.ts_offset);

                // In a real rtsp stream, the duration of a packet is not known until the
                // next packet. ffmpeg's duration is an unreliable estimate. Set it to something
                // ridiculous.
                pkt.set_duration(i32::try_from(3600 * recording::TIME_UNITS_PER_SEC).unwrap());
            }

            Ok(pkt)
        }

        fn get_video_codecpar(&self) -> ffmpeg::avcodec::InputCodecParameters<'_> {
            self.inner.get_video_codecpar()
        }

        fn get_extra_data(&self) -> Result<h264::ExtraData, Error> {
            self.inner.get_extra_data()
        }
    }

    struct MockOpener<'a> {
        expected_url: String,
        streams: Mutex<Vec<ProxyingStream<'a>>>,
        shutdown: Arc<AtomicBool>,
    }

    impl<'a> stream::Opener<ProxyingStream<'a>> for MockOpener<'a> {
        fn open(&self, src: stream::Source) -> Result<ProxyingStream<'a>, Error> {
            match src {
                stream::Source::Rtsp { url, .. } => assert_eq!(url, &self.expected_url),
                stream::Source::File(_) => panic!("expected rtsp url"),
            };
            let mut l = self.streams.lock();
            match l.pop() {
                Some(stream) => {
                    trace!("MockOpener returning next stream");
                    Ok(stream)
                }
                None => {
                    trace!("MockOpener shutting down");
                    self.shutdown.store(true, Ordering::SeqCst);
                    bail!("done")
                }
            }
        }
    }

    #[derive(Debug, Eq, PartialEq)]
    struct Frame {
        start_90k: i32,
        duration_90k: i32,
        is_key: bool,
    }

    fn get_frames(db: &db::LockedDatabase, id: CompositeId) -> Vec<Frame> {
        db.with_recording_playback(id, &mut |rec| {
            let mut it = recording::SampleIndexIterator::default();
            let mut frames = Vec::new();
            while it.next(&rec.video_index).unwrap() {
                frames.push(Frame {
                    start_90k: it.start_90k,
                    duration_90k: it.duration_90k,
                    is_key: it.is_key(),
                });
            }
            Ok(frames)
        })
        .unwrap()
    }

    #[test]
    fn basic() {
        testutil::init();
        // 2015-04-25 00:00:00 UTC
        let clocks = clock::SimulatedClocks::new(time::Timespec::new(1429920000, 0));
        clocks.sleep(time::Duration::seconds(86400)); // to 2015-04-26 00:00:00 UTC

        let stream = stream::FFMPEG
            .open(stream::Source::File("src/testdata/clip.mp4"))
            .unwrap();
        let mut stream = ProxyingStream::new(&clocks, time::Duration::seconds(2), stream);
        stream.ts_offset = 123456; // starting pts of the input should be irrelevant
        stream.ts_offset_pkts_left = u32::max_value();
        stream.pkts_left = u32::max_value();
        let opener = MockOpener {
            expected_url: "rtsp://foo:bar@test-camera/main".to_owned(),
            streams: Mutex::new(vec![stream]),
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        let db = testutil::TestDb::new(clocks.clone());
        let env = super::Environment {
            opener: &opener,
            db: &db.db,
            shutdown: &opener.shutdown,
        };
        let mut stream;
        {
            let l = db.db.lock();
            let camera = l.cameras_by_id().get(&testutil::TEST_CAMERA_ID).unwrap();
            let s = l.streams_by_id().get(&testutil::TEST_STREAM_ID).unwrap();
            let dir = db
                .dirs_by_stream_id
                .get(&testutil::TEST_STREAM_ID)
                .unwrap()
                .clone();
            stream = super::Streamer::new(
                &env,
                dir,
                db.syncer_channel.clone(),
                testutil::TEST_STREAM_ID,
                camera,
                s,
                0,
                3,
            )
            .unwrap();
        }
        stream.run();
        assert!(opener.streams.lock().is_empty());
        db.syncer_channel.flush();
        let db = db.db.lock();

        // Compare frame-by-frame. Note below that while the rotation is scheduled to happen near
        // 3-second boundaries (such as 2016-04-26 00:00:03), rotation happens somewhat later:
        // * the first rotation is always skipped
        // * the second rotation is deferred until a key frame.
        #[rustfmt::skip]
        assert_eq!(get_frames(&db, CompositeId::new(testutil::TEST_STREAM_ID, 0)), &[
            Frame { start_90k:      0, duration_90k: 90379, is_key:  true },
            Frame { start_90k:  90379, duration_90k: 89884, is_key: false },
            Frame { start_90k: 180263, duration_90k: 89749, is_key: false },
            Frame { start_90k: 270012, duration_90k: 89981, is_key: false }, // pts_time 3.0001...
            Frame { start_90k: 359993, duration_90k: 90055, is_key:  true },
            Frame { start_90k: 450048, duration_90k: 89967, is_key: false },
            Frame { start_90k: 540015, duration_90k: 90021, is_key: false }, // pts_time 6.0001...
            Frame { start_90k: 630036, duration_90k: 89958, is_key: false },
        ]);
        #[rustfmt::skip]
        assert_eq!(get_frames(&db, CompositeId::new(testutil::TEST_STREAM_ID, 1)), &[
            Frame { start_90k:      0, duration_90k: 90011, is_key:  true },
            Frame { start_90k:  90011, duration_90k:     0, is_key: false },
        ]);
        let mut recordings = Vec::new();
        db.list_recordings_by_id(testutil::TEST_STREAM_ID, 0..2, &mut |r| {
            recordings.push(r);
            Ok(())
        })
        .unwrap();
        assert_eq!(2, recordings.len());
        assert_eq!(0, recordings[0].id.recording());
        assert_eq!(recording::Time(128700575999999), recordings[0].start);
        assert_eq!(0, recordings[0].flags);
        assert_eq!(1, recordings[1].id.recording());
        assert_eq!(recording::Time(128700576719993), recordings[1].start);
        assert_eq!(db::RecordingFlags::TrailingZero as i32, recordings[1].flags);
    }
}
