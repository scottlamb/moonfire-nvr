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

use base::clock::Clocks;
use crate::db;
use crate::dir;
use fnv::FnvHashMap;
use mylog;
use rusqlite;
use std::env;
use std::sync::Arc;
use std::thread;
use tempdir::TempDir;
use time;
use uuid::Uuid;
use crate::writer;

static INIT: parking_lot::Once = parking_lot::Once::new();

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
///    * set `TZ=America/Los_Angeles` so that tests that care about calendar time get the expected
///      results regardless of machine setup.)
///    * use a fast but insecure password hashing format.
pub fn init() {
    INIT.call_once(|| {
        let h = mylog::Builder::new()
            .set_spec(&::std::env::var("MOONFIRE_LOG").unwrap_or("info".to_owned()))
            .build();
        h.install().unwrap();
        env::set_var("TZ", "America/Los_Angeles");
        time::tzset();
        crate::auth::set_test_config();
    });
}

pub struct TestDb<C: Clocks + Clone> {
    pub db: Arc<db::Database<C>>,
    pub dirs_by_stream_id: Arc<FnvHashMap<i32, Arc<dir::SampleFileDir>>>,
    pub syncer_channel: writer::SyncerChannel<::std::fs::File>,
    pub syncer_join: thread::JoinHandle<()>,
    pub tmpdir: TempDir,
    pub test_camera_uuid: Uuid,
}

impl<C: Clocks + Clone> TestDb<C> {
    /// Creates a test database with one camera.
    pub fn new(clocks: C) -> Self {
        Self::new_with_flush_if_sec(clocks, 0)
    }

    pub(crate) fn new_with_flush_if_sec(clocks: C, flush_if_sec: i64) -> Self {
        let tmpdir = TempDir::new("moonfire-nvr-test").unwrap();

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let db = Arc::new(db::Database::new(clocks, conn, true).unwrap());
        let (test_camera_uuid, sample_file_dir_id);
        let path = tmpdir.path().to_str().unwrap().to_owned();
        let dir;
        {
            let mut l = db.lock();
            sample_file_dir_id = l.add_sample_file_dir(path.to_owned()).unwrap();
            assert_eq!(TEST_CAMERA_ID, l.add_camera(db::CameraChange {
                short_name: "test camera".to_owned(),
                description: "".to_owned(),
                onvif_host: "test-camera".to_owned(),
                username: "foo".to_owned(),
                password: "bar".to_owned(),
                streams: [
                    db::StreamChange {
                        sample_file_dir_id: Some(sample_file_dir_id),
                        rtsp_url: "rtsp://test-camera/main".to_owned(),
                        record: true,
                        flush_if_sec,
                    },
                    Default::default(),
                ],
            }).unwrap());
            test_camera_uuid = l.cameras_by_id().get(&TEST_CAMERA_ID).unwrap().uuid;
            l.update_retention(&[db::RetentionChange {
                stream_id: TEST_STREAM_ID,
                new_record: true,
                new_limit: 1048576,
            }]).unwrap();
            dir = l.sample_file_dirs_by_id().get(&sample_file_dir_id).unwrap().get().unwrap();
        }
        let mut dirs_by_stream_id = FnvHashMap::default();
        dirs_by_stream_id.insert(TEST_STREAM_ID, dir.clone());
        let (syncer_channel, syncer_join) =
            writer::start_syncer(db.clone(), sample_file_dir_id).unwrap();
        TestDb {
            db,
            dirs_by_stream_id: Arc::new(dirs_by_stream_id),
            syncer_channel,
            syncer_join,
            tmpdir,
            test_camera_uuid,
        }
    }

    /// Creates a recording with a fresh `RecordingToInsert` row which has been touched only by
    /// a `SampleIndexEncoder`. Fills in a video sample entry id and such to make it valid.
    /// There will no backing sample file, so it won't be possible to generate a full `.mp4`.
    pub fn insert_recording_from_encoder(&self, r: db::RecordingToInsert)
                                                -> db::ListRecordingsRow {
        use crate::recording::{self, TIME_UNITS_PER_SEC};
        let mut db = self.db.lock();
        let video_sample_entry_id = db.insert_video_sample_entry(
            1920, 1080, [0u8; 100].to_vec(), "avc1.000000".to_owned()).unwrap();
        let (id, _) = db.add_recording(TEST_STREAM_ID, db::RecordingToInsert {
            start: recording::Time(1430006400i64 * TIME_UNITS_PER_SEC),
            video_sample_entry_id,
            ..r
        }).unwrap();
        db.mark_synced(id).unwrap();
        db.flush("create_recording_from_encoder").unwrap();
        let mut row = None;
        db.list_recordings_by_id(TEST_STREAM_ID, id.recording() .. id.recording()+1,
                                 &mut |r| { row = Some(r); Ok(()) }).unwrap();
        row.unwrap()
    }
}

// For benchmarking
#[cfg(feature="nightly")]
pub fn add_dummy_recordings_to_db(db: &db::Database, num: usize) {
    use crate::recording::{self, TIME_UNITS_PER_SEC};
    let mut data = Vec::new();
    data.extend_from_slice(include_bytes!("testdata/video_sample_index.bin"));
    let mut db = db.lock();
    let video_sample_entry_id = db.insert_video_sample_entry(
        1920, 1080, [0u8; 100].to_vec(), "avc1.000000".to_owned()).unwrap();
    let mut recording = db::RecordingToInsert {
        sample_file_bytes: 30104460,
        start: recording::Time(1430006400i64 * TIME_UNITS_PER_SEC),
        duration_90k: 5399985,
        video_samples: 1800,
        video_sync_samples: 60,
        video_sample_entry_id: video_sample_entry_id,
        video_index: data,
        run_offset: 0,
        ..Default::default()
    };
    for _ in 0..num {
        let (id, _) = db.add_recording(TEST_STREAM_ID, recording.clone()).unwrap();
        recording.start += recording::Duration(recording.duration_90k as i64);
        recording.run_offset += 1;
        db.mark_synced(id).unwrap();
    }
    db.flush("add_dummy_recordings_to_db").unwrap();
}
