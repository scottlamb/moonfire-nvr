// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 Scott Lamb <slamb@slamb.org>
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

use db::{self, CompositeId, FromSqlUuid};
use dir;
use failure::Error;
use fnv::FnvHashMap;
use raw;
use recording;
use rusqlite;
use schema;
use std::os::unix::ffi::OsStrExt;
use std::fs;

pub struct Options {
    pub compare_lens: bool,
}

pub fn run(conn: &rusqlite::Connection, opts: &Options) -> Result<(), Error> {
    let db_uuid = raw::get_db_uuid(&conn)?;

    // Scan directories.
    let mut files_by_dir = FnvHashMap::default();
    {
        let mut stmt = conn.prepare(r#"
            select d.id, d.path, d.uuid, d.last_complete_open_id, o.uuid
            from sample_file_dir d left join open o on (d.last_complete_open_id = o.id)
        "#)?;
        let mut rows = stmt.query(&[])?;
        while let Some(row) = rows.next() {
            let row = row?;
            let mut meta = schema::DirMeta::default();
            let dir_id = row.get_checked(0)?;
            let dir_path: String = row.get_checked(1)?;
            let dir_uuid: FromSqlUuid = row.get_checked(2)?;
            let open_id = row.get_checked(3)?;
            let open_uuid: FromSqlUuid = row.get_checked(4)?;
            meta.db_uuid.extend_from_slice(&db_uuid.as_bytes()[..]);
            meta.dir_uuid.extend_from_slice(&dir_uuid.0.as_bytes()[..]);
            {
                let o = meta.mut_last_complete_open();
                o.id = open_id;
                o.uuid.extend_from_slice(&open_uuid.0.as_bytes()[..]);
            }

            // Open the directory (checking its metadata) and hold it open (for the lock).
            let _dir = dir::SampleFileDir::open(&dir_path, &meta)?;
            let files = read_dir(&dir_path, opts)?;
            files_by_dir.insert(dir_id, files);
        }
    }

    // Scan streams.
    {
        let mut stmt = conn.prepare(r#"
            select id, sample_file_dir_id from stream
        "#)?;
        let mut rows = stmt.query(&[])?;
        while let Some(row) = rows.next() {
            let row = row?;
            let stream_id = row.get_checked(0)?;
            let dir_id = row.get_checked(1)?;
            let mut empty = FnvHashMap::default();
            let files = match dir_id {
                None => &mut empty,
                Some(id) => files_by_dir.get_mut(&id).unwrap(),
            };
            compare_stream(conn, stream_id, opts, files)?;
        }
    }

    for (&dir_id, files) in &mut files_by_dir {
        compare_dir(conn, dir_id, files)?;
    }

    Ok(())
}

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
        video_sync_samples += it.is_key() as i32;
    }
    Ok(RecordingSummary {
        bytes: bytes,
        video_samples: video_samples,
        video_sync_samples: video_sync_samples,
        duration: duration,
        flags: if it.duration_90k == 0 { db::RecordingFlags::TrailingZero as i32 } else { 0 },
    })
}

/// Reads through the given sample file directory.
/// Logs unexpected files and creates a hash map of the files found there.
/// If `opts.compare_lens` is set, the values are lengths; otherwise they're insignificant.
fn read_dir(path: &str, opts: &Options) -> Result<FnvHashMap<CompositeId, u64>, Error> {
    let mut files = FnvHashMap::default();
    for e in fs::read_dir(path)? {
        let e = e?;
        let f = e.file_name();
        let f = f.as_bytes();
        match f {
            //"." | ".." => continue,
            b"meta" | b"meta-tmp" => continue,
            _ => {},
        };
        let id = match dir::parse_id(f) {
            Ok(id) => id,
            Err(_) => {
                error!("sample file directory contains file {:?} which isn't an id", f);
                continue;
            }
        };
        let len = if opts.compare_lens { e.metadata()?.len() } else { 0 };
        files.insert(id, len);
    }
    Ok(files)
}

/// Looks through the stream for errors.
/// Removes found recordings from the given file map.
fn compare_stream(conn: &rusqlite::Connection, stream_id: i32, opts: &Options,
                  files: &mut FnvHashMap<CompositeId, u64>)
                  -> Result<(), Error> {
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
            recording_playback.video_index
    "#;
    let mut stmt = conn.prepare_cached(&format!(r#"
        select {}
        from recording left join recording_playback on
            (recording.composite_id = recording_playback.composite_id)
        where :start <= recording.composite_id and recording.composite_id < :end
        union all
        select {}
        from recording_playback left join recording on
            (recording_playback.composite_id = recording.composite_id)
        where recording.composite_id is null and
              :start <= recording_playback.composite_id and recording_playback.composite_id < :end
    "#, FIELDS, FIELDS))?;
    let mut rows = stmt.query_named(&[
        (":start", &CompositeId::new(stream_id, 0).0),
        (":end", &CompositeId::new(stream_id + 1, 0).0),
    ])?;
    while let Some(row) = rows.next() {
        let row = row?;
        let id = row.get_checked::<_, Option<i64>>(0)?.map(|id| CompositeId(id));
        let playback_id = row.get_checked::<_, Option<i64>>(6)?.map(|id| CompositeId(id));
        let id = match (id, playback_id) {
            (Some(id1), Some(_)) => id1,
            (Some(id1), None) => {
                error!("id {} has recording row but no recording_playback row", id1);
                continue;
            },
            (None, Some(id2)) => {
                error!("id {} has recording_playback row but no recording row", id2);
                continue;
            },
            (None, None) => bail!("outer join returned fully empty row"),
        };
        let row_summary = RecordingSummary {
            flags: row.get_checked(1)?,
            bytes: row.get_checked::<_, i64>(2)? as u64,
            duration: row.get_checked(3)?,
            video_samples: row.get_checked(4)?,
            video_sync_samples: row.get_checked(5)?,
        };
        let video_index: Vec<u8> = row.get_checked(7)?;
        let index_summary = match summarize_index(&video_index) {
            Ok(s) => s,
            Err(e) => {
                error!("id {} has bad video_index: {}", id, e);
                continue;
            },
        };
        if row_summary != index_summary {
            error!("id {} row summary {:#?} inconsistent with index {:#?}",
                   id, row_summary, index_summary);
        }
        let len = match files.remove(&id) {
            Some(l) => l,
            None => {
                error!("id {} missing", id);
                continue;
            }
        };
        if opts.compare_lens && row_summary.bytes != len {
            error!("id {} declares length {}, but its sample file has length {}",
                   id, row_summary.bytes, len);
        }
    }
    Ok(())
}

fn compare_dir(conn: &rusqlite::Connection, dir_id: i32,
               files: &mut FnvHashMap<CompositeId, u64>) -> Result<(), Error> {
    let mut stmt = conn.prepare_cached(
        "select composite_id from garbage where sample_file_dir_id = ?")?;
    let mut rows = stmt.query(&[&dir_id])?;
    while let Some(row) = rows.next() {
        let row = row?;
        files.remove(&CompositeId(row.get_checked(0)?));
    }

    for (k, _) in files {
        error!("dir {}: Unexpected file {}", dir_id, k);
    }
    Ok(())
}
