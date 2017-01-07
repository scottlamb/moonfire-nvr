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

//! Subcommand to check the database and sample file dir for errors.

use db;
use error::Error;
use recording;
use rusqlite;
use std::fs;
use uuid::Uuid;

#[derive(Debug, Eq, PartialEq)]
struct RecordingSummary {
    bytes: u64,
    video_samples: i32,
    video_sync_samples: i32,
    duration: i32,
    flags: i32,
}

fn summarize_index(video_index: &[u8]) -> Result<RecordingSummary, Error> {
    let mut it = recording::SampleIndexIterator::new();
    let mut duration = 0;
    let mut video_samples = 0;
    let mut video_sync_samples = 0;
    let mut bytes = 0;
    while it.next(video_index)? {
        bytes += it.bytes as u64;
        duration += it.duration_90k;
        video_samples += 1;
        video_sync_samples += if it.is_key { 1 } else { 0 };
    }
    Ok(RecordingSummary{
        bytes: bytes,
        video_samples: video_samples,
        video_sync_samples: video_sync_samples,
        duration: duration,
        flags: if it.duration_90k == 0 { db::RecordingFlags::TrailingZero as i32 } else { 0 },
    })
}

struct File {
    uuid: Uuid,
    len: u64,
    composite_id: Option<i64>,
}

pub fn run(conn: rusqlite::Connection, sample_file_dir: &str) -> Result<(), Error> {
    let mut files = Vec::new();
    for e in fs::read_dir(sample_file_dir)? {
        let e = e?;
        let uuid = match e.file_name().to_str().and_then(|f| Uuid::parse_str(f).ok()) {
            Some(f) => f,
            None => {
                error!("sample file directory contains file {} which isn't a uuid",
                       e.file_name().to_string_lossy());
                continue;
            }
        };
        let len = e.metadata()?.len();
        files.push(File{uuid: uuid, len: len, composite_id: None});
    }
    files.sort_by(|a, b| a.uuid.cmp(&b.uuid));

    // This statement should be a full outer join over the recording and recording_playback tables.
    // SQLite3 doesn't support that, though, so emulate it with a couple left joins and a union.
    const FIELDS: &'static str = r#"
            recording.composite_id,
            recording.flags,
            recording.sample_file_bytes,
            recording.duration_90k,
            recording.video_samples,
            recording.video_sync_samples,
            recording_playback.composite_id,
            recording_playback.sample_file_uuid,
            recording_playback.video_index
    "#;
    let mut stmt = conn.prepare(&format!(r#"
        select {}
        from recording left join recording_playback on
            (recording.composite_id = recording_playback.composite_id)
        union all
        select {}
        from recording_playback left join recording on
            (recording_playback.composite_id = recording.composite_id)
        where recording.composite_id is null
    "#, FIELDS, FIELDS))?;
    let mut rows = stmt.query(&[])?;
    while let Some(row) = rows.next() {
        let row = row?;
        let composite_id: Option<i64> = row.get_checked(0)?;
        let playback_composite_id: Option<i64> = row.get_checked(6)?;
        let composite_id = match (composite_id, playback_composite_id) {
            (Some(id1), Some(_)) => id1,
            (Some(id1), None) => {
                error!("composite id {} has recording row but no recording_playback row", id1);
                continue;
            },
            (None, Some(id2)) => {
                error!("composite id {} has recording_playback row but no recording row", id2);
                continue;
            },
            (None, None) => {
                return Err(Error::new("outer join returned fully empty row".to_owned()));
            },
        };
        let row_summary = RecordingSummary{
            flags: row.get_checked(1)?,
            bytes: row.get_checked::<_, i64>(2)? as u64,
            duration: row.get_checked(3)?,
            video_samples: row.get_checked(4)?,
            video_sync_samples: row.get_checked(5)?,
        };
        let sample_file_uuid = Uuid::from_bytes(&row.get_checked::<_, Vec<u8>>(7)?)?;
        let video_index: Vec<u8> = row.get_checked(8)?;
        let index_summary = match summarize_index(&video_index) {
            Ok(s) => s,
            Err(e) => {
                error!("composite id {} has bad video_index: {}", composite_id, e);
                continue;
            },
        };
        if row_summary != index_summary {
            error!("composite id {} row summary {:#?} inconsistent with index {:#?}",
                   composite_id, row_summary, index_summary);
        }
        let f = match files.binary_search_by(|f| f.uuid.cmp(&sample_file_uuid)) {
            Ok(i) => &mut files[i],
            Err(_) => {
                error!("composite id {} refers to missing sample file {}",
                       composite_id, sample_file_uuid);
                continue;
            }
        };
        if let Some(id) = f.composite_id {
            error!("composite id {} refers to sample file {} already used by id {}",
                   composite_id, sample_file_uuid, id);
        } else {
            f.composite_id = Some(composite_id);
        }
        if row_summary.bytes != f.len {
            error!("composite id {} declares length {}, but its sample file {} has length {}",
                   composite_id, row_summary.bytes, sample_file_uuid, f.len);
        }
    }

    for f in files {
        if f.composite_id.is_none() {
            error!("sample file {} not used by any recording", f.uuid);
        }
    }
    info!("Check done.");
    Ok(())
}
