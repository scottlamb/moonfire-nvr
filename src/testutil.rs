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
use mylog;
use recording::{self, TIME_UNITS_PER_SEC};
use rusqlite;
use std::env;
use std::sync;
use std::thread;
use time;
use uuid::Uuid;

static INIT: sync::Once = sync::ONCE_INIT;

lazy_static! {
    pub static ref TEST_CAMERA_UUID: Uuid =
        Uuid::parse_str("ce2d9bc2-0cd3-4204-9324-7b5ccb07183c").unwrap();
}

/// id of the camera created by `TestDb::new` below.
pub const TEST_CAMERA_ID: i32 = 1;

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
    pub db: sync::Arc<db::Database>,
    pub dir: sync::Arc<dir::SampleFileDir>,
    pub syncer_channel: dir::SyncerChannel,
    pub syncer_join: thread::JoinHandle<()>,
    pub tmpdir: tempdir::TempDir,
}

impl TestDb {
    /// Creates a test database with one camera.
    pub fn new() -> TestDb {
        let tmpdir = tempdir::TempDir::new("moonfire-nvr-test").unwrap();

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let schema = include_str!("schema.sql");
        conn.execute_batch(schema).unwrap();
        let uuid_bytes = &TEST_CAMERA_UUID.as_bytes()[..];
        conn.execute_named(r#"
            insert into camera (uuid,  short_name,  description,  host,  username,  password,
                                main_rtsp_path,  sub_rtsp_path,  retain_bytes,  next_recording_id)
                        values (:uuid, :short_name, :description, :host, :username, :password,
                                :main_rtsp_path, :sub_rtsp_path, :retain_bytes, :next_recording_id)
        "#, &[
            (":uuid", &uuid_bytes),
            (":short_name", &"test camera"),
            (":description", &""),
            (":host", &"test-camera"),
            (":username", &"foo"),
            (":password", &"bar"),
            (":main_rtsp_path", &"/main"),
            (":sub_rtsp_path", &"/sub"),
            (":retain_bytes", &1048576i64),
            (":next_recording_id", &1i64),
        ]).unwrap();
        assert_eq!(TEST_CAMERA_ID as i64, conn.last_insert_rowid());
        let db = sync::Arc::new(db::Database::new(conn).unwrap());
        let path = tmpdir.path().to_str().unwrap().to_owned();
        let dir = dir::SampleFileDir::new(&path, db.clone()).unwrap();
        let (syncer_channel, syncer_join) = dir::start_syncer(dir.clone()).unwrap();
        TestDb{
            db: db,
            dir: dir,
            syncer_channel: syncer_channel,
            syncer_join: syncer_join,
            tmpdir: tmpdir,
        }
    }

    pub fn create_recording_from_encoder(&self, encoder: recording::SampleIndexEncoder)
                                         -> db::ListRecordingsRow {
        let mut db = self.db.lock();
        let video_sample_entry_id = db.insert_video_sample_entry(
            1920, 1080, [0u8; 100].to_vec(), "avc1.000000".to_owned()).unwrap();
        let row_id;
        {
            let mut tx = db.tx().unwrap();
            tx.bypass_reservation_for_testing = true;
            const START_TIME: recording::Time = recording::Time(1430006400i64 * TIME_UNITS_PER_SEC);
            row_id = tx.insert_recording(&db::RecordingToInsert{
                camera_id: TEST_CAMERA_ID,
                sample_file_bytes: encoder.sample_file_bytes,
                time: START_TIME ..
                      START_TIME + recording::Duration(encoder.total_duration_90k as i64),
                local_time_delta: recording::Duration(0),
                video_samples: encoder.video_samples,
                video_sync_samples: encoder.video_sync_samples,
                video_sample_entry_id: video_sample_entry_id,
                sample_file_uuid: Uuid::nil(),
                video_index: encoder.video_index,
                sample_file_sha1: [0u8; 20],
                run_offset: 0,  // TODO
                flags: 0,  // TODO
            }).unwrap();
            tx.commit().unwrap();
        }
        let mut row = None;
        db.list_recordings_by_id(TEST_CAMERA_ID, row_id .. row_id + 1,
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
    let mut recording = db::RecordingToInsert{
        camera_id: TEST_CAMERA_ID,
        sample_file_bytes: 30104460,
        flags: 0,
        time: START_TIME .. (START_TIME + DURATION),
        local_time_delta: recording::Duration(0),
        video_samples: 1800,
        video_sync_samples: 60,
        video_sample_entry_id: video_sample_entry_id,
        sample_file_uuid: Uuid::nil(),
        video_index: data,
        sample_file_sha1: [0; 20],
        run_offset: 0,
    };
    let mut tx = db.tx().unwrap();
    tx.bypass_reservation_for_testing = true;
    for _ in 0..num {
        tx.insert_recording(&recording).unwrap();
        recording.time.start += DURATION;
        recording.time.end += DURATION;
        recording.run_offset += 1;
    }
    tx.commit().unwrap();
}
