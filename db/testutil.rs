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

extern crate tempdir;

use db;
use dir;
use fnv::FnvHashMap;
use mylog;
use recording::{self, TIME_UNITS_PER_SEC};
use rusqlite;
use std::env;
use std::sync::{self, Arc};
use std::thread;
use time;
use uuid::Uuid;

static INIT: sync::Once = sync::ONCE_INIT;

/// id of the camera created by `TestDb::new` below.
pub const TEST_CAMERA_ID: i32 = 1;
pub const TEST_STREAM_ID: i32 = 1;

/// Performs global initialization for tests.
///    * set up logging. (Note the output can be confusing unless `RUST_TEST_THREADS=1` is set in
///      the program's environment prior to running.)
///    * set `TZ=America/Los_Angeles` so that tests that care about calendar time get the expected
///      results regardless of machine setup.)
pub fn init() {
    INIT.call_once(|| {
        let h = mylog::Builder::new()
            .set_spec(&::std::env::var("MOONFIRE_LOG").unwrap_or("info".to_owned()))
            .build();
        h.install().unwrap();
        env::set_var("TZ", "America/Los_Angeles");
        time::tzset();
    });
}

pub struct TestDb {
    pub db: Arc<db::Database>,
    pub dirs_by_stream_id: Arc<FnvHashMap<i32, Arc<dir::SampleFileDir>>>,
    pub syncer_channel: dir::SyncerChannel,
    pub syncer_join: thread::JoinHandle<()>,
    pub tmpdir: tempdir::TempDir,
    pub test_camera_uuid: Uuid,
}

impl TestDb {
    /// Creates a test database with one camera.
    pub fn new() -> TestDb {
        let tmpdir = tempdir::TempDir::new("moonfire-nvr-test").unwrap();

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        db::Database::init(&mut conn).unwrap();
        let db = Arc::new(db::Database::new(conn, true).unwrap());
        let (test_camera_uuid, sample_file_dir_id);
        let path = tmpdir.path().to_str().unwrap().to_owned();
        let dir;
        {
            let mut l = db.lock();
            sample_file_dir_id = l.add_sample_file_dir(path.to_owned()).unwrap();
            assert_eq!(TEST_CAMERA_ID, l.add_camera(db::CameraChange {
                short_name: "test camera".to_owned(),
                description: "".to_owned(),
                host: "test-camera".to_owned(),
                username: "foo".to_owned(),
                password: "bar".to_owned(),
                streams: [
                    db::StreamChange {
                        sample_file_dir_id: Some(sample_file_dir_id),
                        rtsp_path: "/main".to_owned(),
                        record: true,
                        flush_if_sec: 0,
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
            dir::start_syncer(db.clone(), sample_file_dir_id).unwrap();
        TestDb {
            db,
            dirs_by_stream_id: Arc::new(dirs_by_stream_id),
            syncer_channel,
            syncer_join,
            tmpdir,
            test_camera_uuid,
        }
    }

    pub fn create_recording_from_encoder(&self, encoder: recording::SampleIndexEncoder)
                                         -> db::ListRecordingsRow {
        let mut db = self.db.lock();
        let video_sample_entry_id = db.insert_video_sample_entry(
            1920, 1080, [0u8; 100].to_vec(), "avc1.000000".to_owned()).unwrap();
        const START_TIME: recording::Time = recording::Time(1430006400i64 * TIME_UNITS_PER_SEC);
        let (id, u) = db.add_recording(TEST_STREAM_ID).unwrap();
        u.lock().recording = Some(db::RecordingToInsert {
            sample_file_bytes: encoder.sample_file_bytes,
            time: START_TIME ..
                  START_TIME + recording::Duration(encoder.total_duration_90k as i64),
            local_time_delta: recording::Duration(0),
            video_samples: encoder.video_samples,
            video_sync_samples: encoder.video_sync_samples,
            video_sample_entry_id: video_sample_entry_id,
            video_index: encoder.video_index,
            sample_file_sha1: [0u8; 20],
            run_offset: 0,
            flags: db::RecordingFlags::TrailingZero as i32,
        });
        u.lock().synced = true;
        db.flush("create_recording_from_encoder").unwrap();
        let mut row = None;
        db.list_recordings_by_id(TEST_STREAM_ID, id.recording() .. id.recording()+1,
                                   |r| { row = Some(r); Ok(()) }).unwrap();
        row.unwrap()
    }
}

// For benchmarking
#[cfg(feature="nightly")]
pub fn add_dummy_recordings_to_db(db: &db::Database, num: usize) {
    let mut data = Vec::new();
    data.extend_from_slice(include_bytes!("testdata/video_sample_index.bin"));
    let mut db = db.lock();
    let video_sample_entry_id = db.insert_video_sample_entry(
        1920, 1080, [0u8; 100].to_vec(), "avc1.000000".to_owned()).unwrap();
    const START_TIME: recording::Time = recording::Time(1430006400i64 * TIME_UNITS_PER_SEC);
    const DURATION: recording::Duration = recording::Duration(5399985);
    let mut recording = db::RecordingToInsert {
        id: db::CompositeId::new(TEST_STREAM_ID, 1),
        sample_file_bytes: 30104460,
        flags: 0,
        time: START_TIME .. (START_TIME + DURATION),
        local_time_delta: recording::Duration(0),
        video_samples: 1800,
        video_sync_samples: 60,
        video_sample_entry_id: video_sample_entry_id,
        video_index: data,
        sample_file_sha1: [0; 20],
        run_offset: 0,
    };
    let mut tx = db.tx().unwrap();
    for i in 0..num {
        tx.insert_recording(&recording).unwrap();
        recording.id = db::CompositeId::new(TEST_STREAM_ID, 2 + i as i32);
        recording.time.start += DURATION;
        recording.time.end += DURATION;
        recording.run_offset += 1;
    }
    tx.commit().unwrap();
}
