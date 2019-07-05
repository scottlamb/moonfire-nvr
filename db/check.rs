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

use crate::db::{self, CompositeId, FromSqlUuid};
use crate::dir;
use crate::raw;
use crate::recording;
use failure::Error;
use fnv::FnvHashMap;
use log::error;
use protobuf::prelude::MessageField;
use rusqlite::types::ToSql;
use crate::schema;
use std::os::unix::ffi::OsStrExt;
use std::fs;

pub struct Options {
    pub compare_lens: bool,
}

pub fn run(conn: &rusqlite::Connection, opts: &Options) -> Result<(), Error> {
    let db_uuid = raw::get_db_uuid(&conn)?;

    // Scan directories.
    let mut streams_by_dir: FnvHashMap<i32, Dir> = FnvHashMap::default();
    {
        let mut dir_stmt = conn.prepare(r#"
            select d.id, d.path, d.uuid, d.last_complete_open_id, o.uuid
            from sample_file_dir d left join open o on (d.last_complete_open_id = o.id)
        "#)?;
        let mut garbage_stmt = conn.prepare_cached(
            "select composite_id from garbage where sample_file_dir_id = ?")?;
        let mut rows = dir_stmt.query(&[] as &[&dyn ToSql])?;
        while let Some(row) = rows.next()? {
            let mut meta = schema::DirMeta::default();
            let dir_id: i32 = row.get(0)?;
            let dir_path: String = row.get(1)?;
            let dir_uuid: FromSqlUuid = row.get(2)?;
            let open_id = row.get(3)?;
            let open_uuid: FromSqlUuid = row.get(4)?;
            meta.db_uuid.extend_from_slice(&db_uuid.as_bytes()[..]);
            meta.dir_uuid.extend_from_slice(&dir_uuid.0.as_bytes()[..]);
            {
                let o = meta.last_complete_open.mut_message();
                o.id = open_id;
                o.uuid.extend_from_slice(&open_uuid.0.as_bytes()[..]);
            }

            // Open the directory (checking its metadata) and hold it open (for the lock).
            let _dir = dir::SampleFileDir::open(&dir_path, &meta)?;
            let mut streams = read_dir(&dir_path, opts)?;
            let mut rows = garbage_stmt.query(&[&dir_id])?;
            while let Some(row) = rows.next()? {
                let id = CompositeId(row.get(0)?);
                let s = streams.entry(id.stream()).or_insert_with(Stream::default);
                s.entry(id.recording()).or_insert_with(Recording::default).garbage_row = true;
            }
            streams_by_dir.insert(dir_id, streams);
        }
    }

    // Scan known streams.
    {
        let mut stmt = conn.prepare(r#"
            select id, sample_file_dir_id from stream where sample_file_dir_id is not null
        "#)?;
        let mut rows = stmt.query(&[] as &[&dyn ToSql])?;
        while let Some(row) = rows.next()? {
            let stream_id = row.get(0)?;
            let dir_id = row.get(1)?;
            let stream = match streams_by_dir.get_mut(&dir_id) {
                None => Stream::default(),
                Some(d) => d.remove(&stream_id).unwrap_or_else(Stream::default),
            };
            compare_stream(conn, stream_id, opts, stream)?;
        }
    }

    // Expect the rest to have only garbage.
    for (&dir_id, streams) in &streams_by_dir {
        for (&stream_id, stream) in streams {
            for (&recording_id, r) in stream {
                let id = CompositeId::new(stream_id, recording_id);
                if r.recording_row.is_some() || r.playback_row.is_some() ||
                   r.integrity_row || !r.garbage_row {
                    error!("dir {} recording {} for unknown stream: {:#?}", dir_id, id, r);
                }
            }
        }
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

#[derive(Debug, Default)]
struct Recording {
    /// Present iff there is a file. When `args.compare_lens` is true, the length; otherwise 0.
    file: Option<u64>,

    /// Iff a `recording` row is present, a `RecordingSummary` from those fields.
    recording_row: Option<RecordingSummary>,

    /// Iff a `recording_playback` row is present, a `RecordingSummary` computed from the index.
    /// This should match the recording row.
    playback_row: Option<RecordingSummary>,

    /// True iff a `recording_integrity` row is present.
    integrity_row: bool,

    /// True iff a `garbage` row is present.
    garbage_row: bool,
}

type Stream = FnvHashMap<i32, Recording>;
type Dir = FnvHashMap<i32, Stream>;

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
        bytes,
        video_samples,
        video_sync_samples,
        duration,
        flags: if it.duration_90k == 0 { db::RecordingFlags::TrailingZero as i32 } else { 0 },
    })
}

/// Reads through the given sample file directory.
/// Logs unexpected files and creates a hash map of the files found there.
/// If `opts.compare_lens` is set, the values are lengths; otherwise they're insignificant.
fn read_dir(path: &str, opts: &Options) -> Result<Dir, Error> {
    let mut dir = Dir::default();
    for e in fs::read_dir(path)? {
        let e = e?;
        let f = e.file_name();
        let f = f.as_bytes();
        match f {
            b"meta" => continue,
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
        let stream = dir.entry(id.stream()).or_insert_with(Stream::default);
        stream.entry(id.recording()).or_insert_with(Recording::default).file = Some(len);
    }
    Ok(dir)
}

/// Looks through a known stream for errors.
fn compare_stream(conn: &rusqlite::Connection, stream_id: i32, opts: &Options,
                  mut stream: Stream) -> Result<(), Error> {
    let start = CompositeId::new(stream_id, 0);
    let end = CompositeId::new(stream_id, i32::max_value());

    // recording row.
    {
        let mut stmt = conn.prepare_cached(r#"
            select
              composite_id,
              flags,
              sample_file_bytes,
              duration_90k,
              video_samples,
              video_sync_samples
            from
              recording
            where
              composite_id between ? and ?
        "#)?;
        let mut rows = stmt.query(&[&start.0, &end.0])?;
        while let Some(row) = rows.next()? {
            let id = CompositeId(row.get(0)?);
            let s = RecordingSummary {
                flags: row.get(1)?,
                bytes: row.get::<_, i64>(2)? as u64,
                duration: row.get(3)?,
                video_samples: row.get(4)?,
                video_sync_samples: row.get(5)?,
            };
            stream.entry(id.recording())
                  .or_insert_with(Recording::default)
                  .recording_row = Some(s);
        }
    }

    // recording_playback row.
    {
        let mut stmt = conn.prepare_cached(r#"
            select
              composite_id,
              video_index
            from
              recording_playback
            where
              composite_id between ? and ?
        "#)?;
        let mut rows = stmt.query(&[&start.0, &end.0])?;
        while let Some(row) = rows.next()? {
            let id = CompositeId(row.get(0)?);
            let video_index: Vec<u8> = row.get(1)?;
            let s = match summarize_index(&video_index) {
                Ok(s) => s,
                Err(e) => {
                    error!("id {} has bad video_index: {}", id, e);
                    continue;
                },
            };
            stream.entry(id.recording())
                  .or_insert_with(Recording::default)
                  .playback_row = Some(s);
        }
    }

    // recording_integrity row.
    {
        let mut stmt = conn.prepare_cached(r#"
            select
              composite_id
            from
              recording_integrity
            where
              composite_id between ? and ?
        "#)?;
        let mut rows = stmt.query(&[&start.0, &end.0])?;
        while let Some(row) = rows.next()? {
            let id = CompositeId(row.get(0)?);
            stream.entry(id.recording())
                  .or_insert_with(Recording::default)
                  .integrity_row = true;
        }
    }

    for (&id, recording) in &stream {
        let id = CompositeId::new(stream_id, id);
        let r = match recording.recording_row {
            Some(ref r) => r,
            None => {
                if !recording.garbage_row || recording.playback_row.is_some() ||
                   recording.integrity_row {
                    error!("Missing recording row for {}: {:#?}", id, recording);
                }
                continue;
            },
        };
        match recording.playback_row {
            Some(ref p) => {
                if r != p {
                    error!("Recording {} summary doesn't match video_index: {:#?}", id, recording);
                }
            },
            None => error!("Recording {} missing playback row: {:#?}", id, recording),
        }
        match recording.file {
            Some(len) => if opts.compare_lens && r.bytes != len {
                error!("Recording {} length mismatch: {:#?}", id, recording);
            },
            None => error!("Recording {} missing file: {:#?}", id, recording),
        }
    }

    Ok(())
}
