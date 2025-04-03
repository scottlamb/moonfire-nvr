// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Utilities for automated testing involving Moonfire NVR's persistence library.
//! Used for tests of both the `moonfire_db` crate itself and the `moonfire_nvr` crate.

use crate::db;
use crate::dir;
use crate::lifecycle;
use base::clock::Clocks;
use base::FastHashMap;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

static INIT: std::sync::Once = std::sync::Once::new();

/// id of the camera created by `TestDb::new` below.
pub const TEST_CAMERA_ID: i32 = 1;
pub const TEST_STREAM_ID: i32 = 1;

pub const TEST_VIDEO_SAMPLE_ENTRY_DATA: &[u8] =
    b"\x00\x00\x00\x7D\x61\x76\x63\x31\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\
    \x00\x00\x00\x00\x00\x00\x00\x00\x00\x07\x80\x04\x38\x00\x48\x00\x00\x00\x48\x00\x00\x00\x00\
    \x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\
    \x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x18\xFF\xFF\x00\x00\x00\x27\x61\x76\
    \x63\x43\x01\x4D\x00\x2A\xFF\xE1\x00\x10\x67\x4D\x00\x2A\x95\xA8\x1E\x00\x89\xF9\x66\xE0\x20\
    \x20\x20\x40\x01\x00\x04\x68\xEE\x3C\x80";

/// Performs global initialization for tests.
///    * set up logging. (Note the output can be confusing unless `RUST_TEST_THREADS=1` is set in
///      the program's environment prior to running.)
///    * set time zone `America/Los_Angeles` so that tests that care about
///      calendar time get the expected results regardless of machine setup.)
///    * use a fast but insecure password hashing format.
pub fn init() {
    INIT.call_once(|| {
        base::ensure_malloc_used();
        base::tracing_setup::install_for_tests();
        base::time::testutil::init_zone();
        crate::auth::set_test_config();
    });
}

pub struct TestDb<C: Clocks + Clone> {
    pub db: Arc<db::Database<C>>,
    pub dirs_by_stream_id: Arc<FastHashMap<i32, dir::Pool>>,
    pub shutdown_tx: base::shutdown::Sender,
    pub shutdown_rx: base::shutdown::Receiver,
    pub flusher_channel: lifecycle::FlusherChannel,
    pub flusher_join: tokio::task::JoinHandle<()>,
    pub tmpdir: TempDir,
    pub test_camera_uuid: Uuid,
}

impl<C: Clocks + Clone> TestDb<C> {
    /// Creates a test database with one camera.
    pub async fn new(clocks: C) -> Self {
        Self::new_with_flush_if_sec(clocks, 0).await
    }

    pub(crate) async fn new_with_flush_if_sec(clocks: C, flush_if_sec: u32) -> Self {
        let tmpdir = tempfile::Builder::new()
            .prefix("moonfire-nvr-test")
            .tempdir()
            .unwrap();

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let db = Arc::new(db::Database::new(clocks, conn, true).unwrap());
        let test_camera_uuid;
        let path = tmpdir.path().to_owned();
        let dir;
        let (flusher_channel, flusher_join) = lifecycle::start_flusher(db.clone());
        let sample_file_dir_id = db.add_sample_file_dir(path).await.unwrap();
        {
            let mut l = db.lock();
            assert_eq!(
                TEST_CAMERA_ID,
                l.add_camera(db::CameraChange {
                    short_name: "test camera".to_owned(),
                    config: crate::json::CameraConfig::default(),
                    //description: "".to_owned(),
                    //onvif_host: "test-camera".to_owned(),
                    //username: Some("foo".to_owned()),
                    //password: Some("bar".to_owned()),
                    streams: [
                        db::StreamChange {
                            sample_file_dir_id: Some(sample_file_dir_id),
                            config: crate::json::StreamConfig {
                                url: Some(url::Url::parse("rtsp://test-camera/main").unwrap()),
                                mode: crate::json::STREAM_MODE_RECORD.to_owned(),
                                flush_if_sec,
                                ..Default::default()
                            },
                        },
                        Default::default(),
                        Default::default(),
                    ],
                })
                .unwrap()
            );
            test_camera_uuid = l.cameras_by_id().get(&TEST_CAMERA_ID).unwrap().uuid;
            l.update_retention(&[db::RetentionChange {
                stream_id: TEST_STREAM_ID,
                new_record: true,
                new_limit: 1048576,
            }])
            .unwrap();
            dir = l
                .sample_file_dirs_by_id()
                .get(&sample_file_dir_id)
                .unwrap()
                .pool()
                .clone();
        }
        let mut dirs_by_stream_id = FastHashMap::default();
        dirs_by_stream_id.insert(TEST_STREAM_ID, dir);
        let (shutdown_tx, shutdown_rx) = base::shutdown::channel();
        TestDb {
            db,
            dirs_by_stream_id: Arc::new(dirs_by_stream_id),
            shutdown_tx,
            shutdown_rx,
            flusher_channel,
            flusher_join,
            tmpdir,
            test_camera_uuid,
        }
    }

    /// Creates a recording from a fresh `RecentRecording` row which has been touched only by
    /// a `SampleIndexEncoder`. Fills in a video sample entry id and such to make it valid.
    /// There will no backing sample file, so it won't be possible to generate a full `.mp4`.
    /// After insertion, calls `f` with the database lock and the inserted recording row,
    /// returning whatever `f` returns.
    pub fn insert_recording_from_encoder<
        T,
        F: FnOnce(&db::LockedDatabase, db::ListRecordingsRow) -> T,
    >(
        &self,
        r: db::RecentRecording,
        f: F,
    ) -> T {
        use crate::recording::{self, TIME_UNITS_PER_SEC};
        let mut db = self.db.lock();
        let video_sample_entry_id = db
            .insert_video_sample_entry(db::VideoSampleEntryToInsert {
                width: 1920,
                height: 1080,
                pasp_h_spacing: 1,
                pasp_v_spacing: 1,
                data: [0u8; 100].to_vec(),
                rfc6381_codec: "avc1.000000".to_owned(),
            })
            .unwrap();
        let id = {
            let s = db.streams_by_id().get(&TEST_STREAM_ID).unwrap();
            let mut s = s.inner.lock();
            assert_eq!(s.writer_state.recording_id, s.complete.cum_recordings);
            let id = s.add_recording(db::RecentRecording {
                start: recording::Time(1430006400i64 * TIME_UNITS_PER_SEC),
                video_sample_entry_id,
                wall_duration_90k: r.media_duration_90k,
                flags: db::RecordingFlags::UNCOMMITTED,
                ..r
            });
            assert_eq!(s.writer_state.recording_id, id);
            s.complete.cum_recordings += 1;
            s.writer_state.recording_id = s.complete.cum_recordings;
            crate::CompositeId::new(TEST_STREAM_ID, id)
        };

        db.flush("create_recording_from_encoder").unwrap();

        enum State<T, F: FnOnce(&db::LockedDatabase, db::ListRecordingsRow) -> T> {
            Uncalled(F),
            Calling,
            Called(T),
        }
        let mut state = State::Uncalled(f);
        db.list_recordings_by_id(
            TEST_STREAM_ID,
            id.recording()..id.recording() + 1,
            &mut |r| {
                match std::mem::replace(&mut state, State::Calling) {
                    State::Uncalled(f) => state = State::Called(f(&db, r)),
                    State::Calling => unreachable!(),
                    State::Called(_) => panic!("row should be found only once"),
                }
                Ok(())
            },
        )
        .unwrap();
        match state {
            State::Called(r) => r,
            State::Uncalled(_) => panic!(
                "row {} should be found immediately after insertion",
                id.recording()
            ),
            State::Calling => unreachable!(),
        }
    }
}

// For benchmarking
#[cfg(feature = "nightly")]
pub fn add_dummy_recordings_to_db(db: &db::Database, num: usize) {
    use crate::recording::{self, TIME_UNITS_PER_SEC};
    let mut data = Vec::new();
    data.extend_from_slice(include_bytes!("testdata/video_sample_index.bin"));
    let mut db = db.lock();
    let video_sample_entry_id = db
        .insert_video_sample_entry(db::VideoSampleEntryToInsert {
            width: 1920,
            height: 1080,
            pasp_h_spacing: 1,
            pasp_v_spacing: 1,
            data: [0u8; 100].to_vec(),
            rfc6381_codec: "avc1.000000".to_owned(),
        })
        .unwrap();
    let mut recording = db::RecentRecording {
        flags: db::RecordingFlags::UNCOMMITTED,
        sample_file_bytes: 30104460,
        start: recording::Time(1430006400i64 * TIME_UNITS_PER_SEC),
        media_duration_90k: 5399985,
        wall_duration_90k: 5399985,
        video_samples: 1800,
        video_sync_samples: 60,
        video_sample_entry_id,
        video_index: data,
        run_offset: 0,
        ..Default::default()
    };
    let stream = db.streams_by_id().get(&TEST_STREAM_ID).unwrap();
    {
        let mut stream = stream.inner.lock();
        let mut id = 0;
        for _ in 0..num {
            id = stream.add_recording(recording.clone());
            stream.complete.cum_recordings += 1;
            stream.complete.cum_media_duration.0 += i64::from(recording.media_duration_90k);
            stream.complete.cum_runs += i32::from(recording.run_offset == 0);
            recording.start += recording::Duration(recording.wall_duration_90k as i64);
            recording.run_offset += 1;
        }
        stream.writer_state.recording_id = id + 1;
        stream.flush_ready = id + 1;
    }
    db.flush("add_dummy_recordings_to_db").unwrap();
}
