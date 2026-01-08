// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2026 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Sample entry management (video and, eventually, audio).
//!
//! Sample entries store parameters needed to decode the stream, such as width
//! and height. There are typically a small number of these entries over the
//! lifetime of the system, and thus they're all kept in RAM, forever. When
//! new ones are added, they're flushed to the database lazily, allowing
//! streamers to work without ever waiting for the database. They are never
//! removed.

use std::{collections::VecDeque, sync::Arc};

use base::{bail, err, Error, FastHashMap};
use derive_more::Debug;
use pretty_hex::PrettyHex as _;
use rusqlite::{named_params, params};

const INSERT_VIDEO_SAMPLE_ENTRY_SQL: &str = r#"
    insert into video_sample_entry (id, width,  height,  pasp_h_spacing,  pasp_v_spacing,
                                    rfc6381_codec, data)
                            values (:id, :width, :height, :pasp_h_spacing, :pasp_v_spacing,
                                    :rfc6381_codec, :data)
"#;

pub type Handle = Arc<base::Mutex<State, 3>>;

pub struct State {
    video_sample_entries_by_id: FastHashMap<i32, Arc<(i32, Video)>>,
    next_video_sample_entry_id: i32,

    /// These entries have not yet been committed to the database. They must be
    /// committed in the same transaction as any recordings which reference
    /// them.
    video_sample_entries_to_flush: VecDeque<Arc<(i32, Video)>>,
}

/// Entries returned by [`State::get_entries_to_flush`].
pub(crate) struct EntriesToFlush {
    video: Vec<Arc<(i32, Video)>>,
}

impl EntriesToFlush {
    pub fn perform_inserts(&self, tx: &rusqlite::Transaction) -> Result<(), Error> {
        let mut stmt = tx.prepare_cached(INSERT_VIDEO_SAMPLE_ENTRY_SQL)?;
        for entry in &self.video {
            let (id, ref entry) = **entry;
            stmt.execute(named_params! {
                ":id": id,
                ":width": entry.width,
                ":height": entry.height,
                ":pasp_h_spacing": entry.pasp_h_spacing,
                ":pasp_v_spacing": entry.pasp_v_spacing,
                ":rfc6381_codec": &entry.rfc6381_codec,
                ":data": &entry.data,
            })
            .map_err(|e| err!(e, msg("Unable to insert {entry:#?}")).build())?;
        }
        Ok(())
    }

    /// Reports that these entries have been committed to the database.
    pub fn post_flush(&self, state: &mut State) {
        #[cfg(debug_assertions)]
        itertools::assert_equal(
            self.video.iter().map(|e| e.0),
            state
                .video_sample_entries_to_flush
                .iter()
                .take(self.video.len())
                .map(|e| e.0),
        );
        state
            .video_sample_entries_to_flush
            .drain(..self.video.len());
    }
}

impl State {
    pub(crate) fn load(conn: &rusqlite::Connection) -> Result<Self, Error> {
        let mut stmt = conn.prepare(
            r#"
            select
                id,
                width,
                height,
                pasp_h_spacing,
                pasp_v_spacing,
                rfc6381_codec,
                data
            from
                video_sample_entry
            order by id
            "#,
        )?;
        let mut rows = stmt.query(params![])?;
        let mut video_sample_entries_by_id = FastHashMap::default();
        let mut next_video_sample_entry_id = 1;
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            let e = Video {
                width: row.get(1)?,
                height: row.get(2)?,
                pasp_h_spacing: row.get(3)?,
                pasp_v_spacing: row.get(4)?,
                rfc6381_codec: row.get(5)?,
                data: row.get(6)?,
            };
            video_sample_entries_by_id.insert(id, Arc::new((id, e)));
            next_video_sample_entry_id = id + 1;
        }
        Ok(Self {
            video_sample_entries_by_id,
            next_video_sample_entry_id,
            video_sample_entries_to_flush: VecDeque::new(),
        })
    }

    /// Inserts a new video sample entry into the in-RAM database if it's not already present.
    ///
    /// Errors only in the case that an entry with the same data exists but different other parameters, which is an error.
    pub fn insert_video(&mut self, entry: Video) -> Result<i32, Error> {
        // Check if it already exists.
        // There shouldn't be too many entries, so it's fine to enumerate everything.
        for value in self.video_sample_entries_by_id.values() {
            let (id, ref v) = **value;
            if v.data == entry.data {
                // The other fields are derived from data, so differences indicate a bug.
                if v.width != entry.width
                    || v.height != entry.height
                    || v.pasp_h_spacing != entry.pasp_h_spacing
                    || v.pasp_v_spacing != entry.pasp_v_spacing
                // XXX: this should also compare rfc6381_codec, but there are a pair of old bugs preventing it.
                // (1) prior to dad664c2, rfc codec was always lowercase, when it should be uppercase.
                // (2) in [dad664c2, b4836f3a) rfc codec was always `avc1.4d401e`!!!
                // || !v.rfc6381_codec.eq_ignore_ascii_case(&entry.rfc6381_codec)
                {
                    bail!(
                        Internal,
                        msg("video_sample_entry id {id}: existing entry {v:?}, new {entry:?}"),
                    );
                }
                return Ok(id);
            }
        }

        let id = self.next_video_sample_entry_id;
        self.next_video_sample_entry_id += 1;
        let arc = Arc::new((id, entry));
        self.video_sample_entries_by_id.insert(id, arc.clone());
        self.video_sample_entries_to_flush.push_back(arc);
        Ok(id)
    }

    pub fn get_video(&self, id: i32) -> Option<Arc<(i32, Video)>> {
        self.video_sample_entries_by_id.get(&id).cloned()
    }

    /// Gets entries to flush.
    ///
    /// The larger database is expected to perform the following sequence:
    ///
    /// 1. (under state lock) get_entries_to_flush()
    /// 2. (without state lock) EntriesToFlush::perform_inserts()
    /// 3. (under state lock) EntriesToFlush::post_flush()
    ///
    /// It's an error to have two `EntriesToFlush` instances active at the same
    /// time. `EntriesToFlush::post_flush` assumes that the entries it knows
    /// about are a prefix of the entries that exist in the `State` at time of
    /// call.
    pub(crate) fn get_entries_to_flush(&self) -> EntriesToFlush {
        EntriesToFlush {
            video: self.video_sample_entries_to_flush.iter().cloned().collect(),
        }
    }
}

/// A concrete box derived from a ISO/IEC 14496-12 section 8.5.2 VisualSampleEntry box. Describes
/// the codec, width, height, etc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Video {
    #[debug("{}", self.data.hex_dump())]
    pub data: Vec<u8>,
    pub rfc6381_codec: String,
    pub width: u16,
    pub height: u16,
    pub pasp_h_spacing: u16,
    pub pasp_v_spacing: u16,
}

impl Video {
    /// Returns the aspect ratio in minimized form.
    pub fn aspect(&self) -> num_rational::Ratio<u32> {
        num_rational::Ratio::new(
            u32::from(self.width) * u32::from(self.pasp_h_spacing),
            u32::from(self.height) * u32::from(self.pasp_v_spacing),
        )
    }
}
