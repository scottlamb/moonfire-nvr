// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2019 Scott Lamb <slamb@slamb.org>
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

use base::bail_t;
use crate::coding;
use crate::db::FromSqlUuid;
use crate::recording;
use failure::{Error, bail, format_err};
use fnv::FnvHashMap;
use rusqlite::{Connection, Transaction, params};
use std::collections::{BTreeMap, BTreeSet};
use std::collections::btree_map::Entry;
use std::ops::Range;
use uuid::Uuid;

/// All state associated with signals. This is the entry point to this module.
pub(crate) struct State {
    signals_by_id: BTreeMap<u32, Signal>,

    /// All types with known states. Note that currently there's no requirement an entry here
    /// exists for every `type_` specified in a `Signal`, and there's an implied `0` (unknown)
    /// state for every `Type`.
    types_by_uuid: FnvHashMap<Uuid, Type>,

    points_by_time: BTreeMap<recording::Time, Point>,

    /// `points_by_time` entries which need to be flushed to the database.
    dirty_by_time: BTreeSet<recording::Time>,
}

/// Representation of all signals at a point in time.
/// Each point matches a `signal_change` table row (when flushed). However, the in-memory
/// representation keeps not only the changes as of that time but also the complete prior state.
#[derive(Default)]
struct Point {
    /// All data associated with the point.
    ///
    /// `data[0..changes_off]` represents previous state (immediately prior to this point).
    /// `data[changes_off..]` represents the changes at this point.
    ///
    /// This representation could be 8 bytes shorter on 64-bit platforms by using a u32 for the
    /// lengths, but this would require some unsafe code.
    ///
    /// The serialized form stored here must always be valid.
    data: Box<[u8]>,
    changes_off: usize,
}

impl Point {
    /// Creates a new point from `prev` and `changes`.
    ///
    /// The caller is responsible for validation. In particular, `changes` must be a valid
    /// serialized form.
    fn new(prev: &BTreeMap<u32, u16>, changes: &[u8]) -> Self {
        let mut data = Vec::with_capacity(3 * prev.len() + changes.len());
        append_serialized(prev, &mut data);
        let changes_off = data.len();
        data.extend(changes);
        Point {
            data: data.into_boxed_slice(),
            changes_off,
        }
    }

    fn swap(&mut self, other: &mut Point) {
        std::mem::swap(&mut self.data, &mut other.data);
        std::mem::swap(&mut self.changes_off, &mut other.changes_off);
    }

    /// Returns an iterator over state as of immediately before this point.
    fn prev(&self) -> PointDataIterator {
        PointDataIterator::new(&self.data[0..self.changes_off])
    }

    /// Returns an iterator over changes in this point.
    fn changes(&self) -> PointDataIterator {
        PointDataIterator::new(&self.data[self.changes_off..])
    }

    /// Returns a mapping of signals to states immediately after this point.
    fn after(&self) -> BTreeMap<u32, u16> {
        let mut after = BTreeMap::new();
        let mut it = self.prev();
        while let Some((signal, state)) = it.next().expect("in-mem prev is valid") {
            after.insert(signal, state);
        }
        let mut it = self.changes();
        while let Some((signal, state)) = it.next().expect("in-mem changes is valid") {
            if state == 0 {
                after.remove(&signal);
            } else {
                after.insert(signal, state);
            }
        }
        after
    }
}

/// Appends a serialized form of `from` into `to`.
///
/// `from` must be an iterator of `(signal, state)` with signal numbers in monotonically increasing
/// order.
fn append_serialized<'a, I>(from: I, to: &mut Vec<u8>)
where I: IntoIterator<Item = (&'a u32, &'a u16)> {
    let mut next_allowed = 0;
    for (&signal, &state) in from.into_iter() {
        assert!(signal >= next_allowed);
        coding::append_varint32(signal - next_allowed, to);
        coding::append_varint32(state as u32, to);
        next_allowed = signal + 1;
    }
}

fn serialize(from: &BTreeMap<u32, u16>) -> Vec<u8> {
    let mut to = Vec::with_capacity(3 * from.len());
    append_serialized(from, &mut to);
    to
}

struct PointDataIterator<'a> {
    data: &'a [u8],
    cur_pos: usize,
    cur_signal: u32,
}

impl<'a> PointDataIterator<'a> {
    fn new(data: &'a [u8]) -> Self {
        PointDataIterator {
            data,
            cur_pos: 0,
            cur_signal: 0,
        }
    }

    /// Returns an error, `None`, or `Some((signal, state))`.
    /// Note that errors should be impossible on in-memory data; this returns `Result` for
    /// validating blobs as they're read from the database.
    fn next(&mut self) -> Result<Option<(u32, u16)>, Error> {
        if self.cur_pos == self.data.len() {
            return Ok(None);
        }
        let (signal_delta, p) = coding::decode_varint32(self.data, self.cur_pos)
            .map_err(|()| format_err!("varint32 decode failure; data={:?} pos={}",
                                      self.data, self.cur_pos))?;
        let (state, p) = coding::decode_varint32(self.data, p)
            .map_err(|()| format_err!("varint32 decode failure; data={:?} pos={}",
                                      self.data, p))?;
        let signal = self.cur_signal.checked_add(signal_delta)
                         .ok_or_else(|| format_err!("signal overflow: {} + {}",
                                                    self.cur_signal, signal_delta))?;
        if state > u16::max_value() as u32 {
            bail!("state overflow: {}", state);
        }
        self.cur_pos = p;
        self.cur_signal = signal + 1;
        Ok(Some((signal, state as u16)))
    }

    fn to_map(mut self) -> Result<BTreeMap<u32, u16>, Error> {
        let mut out = BTreeMap::new();
        while let Some((signal, state)) = self.next()? {
            out.insert(signal, state);
        }
        Ok(out)
    }
}

/// Representation of a `signal_camera` row.
/// `signal_id` is implied by the `Signal` which owns this struct.
#[derive(Debug)]
pub struct SignalCamera {
    pub camera_id: i32,
    pub type_: SignalCameraType,
}

/// Representation of the `type` field in a `signal_camera` row.
#[derive(Debug)]
pub enum SignalCameraType {
    Direct = 0,
    Indirect = 1,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ListStateChangesRow {
    pub when: recording::Time,
    pub signal: u32,
    pub state: u16,
}

impl State {
    pub fn init(conn: &Connection) -> Result<Self, Error> {
        let mut signals_by_id = State::init_signals(conn)?;
        State::fill_signal_cameras(conn, &mut signals_by_id)?;
        Ok(State {
            signals_by_id,
            types_by_uuid: State::init_types(conn)?,
            points_by_time: State::init_points(conn)?,
            dirty_by_time: BTreeSet::new(),
        })
    }

    pub fn list_changes_by_time(
        &self, desired_time: Range<recording::Time>, f: &mut dyn FnMut(&ListStateChangesRow)) {

        // First find the state immediately before. If it exists, include it.
        if let Some((&when, p)) = self.points_by_time.range(..desired_time.start).next_back() {
            for (&signal, &state) in &p.after() {
                f(&ListStateChangesRow {
                    when,
                    signal,
                    state,
                });
            }
        }

        // Then include changes up to (but not including) the end time.
        for (&when, p) in self.points_by_time.range(desired_time.clone()) {
            let mut it = p.changes();
            while let Some((signal, state)) = it.next().expect("in-mem changes is valid") {
                f(&ListStateChangesRow {
                    when,
                    signal,
                    state,
                });
            }
        }
    }

    pub fn update_signals(
        &mut self, when: Range<recording::Time>, signals: &[u32], states: &[u16])
        -> Result<(), base::Error> {
        // Do input validation before any mutation.
        self.update_signals_validate(signals, states)?;

        // Follow the std::ops::Range convention of considering a range empty if its start >= end.
        // Bailing early in the empty case isn't just an optimization; apply_observation_end would
        // be incorrect otherwise.
        if when.end <= when.start {
            return Ok(());
        }

        // Apply the end before the start so that the `prev` state can be examined.
        self.update_signals_end(when.end, signals, states);
        self.update_signals_start(when.start, signals, states);
        self.update_signals_middle(when, signals, states);
        Ok(())
    }

    /// Helper for `update_signals` to do validation.
    fn update_signals_validate(&self, signals: &[u32], states: &[u16]) -> Result<(), base::Error> {
        if signals.len() != states.len() {
            bail_t!(InvalidArgument, "signals and states must have same length");
        }
        let mut next_allowed = 0u32;
        for (&signal, &state) in signals.iter().zip(states) {
            if signal < next_allowed {
                bail_t!(InvalidArgument, "signals must be monotonically increasing");
            }
            match self.signals_by_id.get(&signal) {
                None => bail_t!(InvalidArgument, "unknown signal {}", signal),
                Some(ref s) => {
                    let empty = Vec::new();
                    let states = self.types_by_uuid.get(&s.type_)
                                                   .map(|t| &t.states)
                                                   .unwrap_or(&empty);
                    if signal != 0 && states.binary_search_by_key(&state, |s| s.value).is_err() {
                        bail_t!(FailedPrecondition, "signal {} specifies unknown state {}",
                                signal, state);
                    }
                },
            }
            next_allowed = signal + 1;
        }
        Ok(())
    }

    /// Helper for `update_signals` to apply the end point.
    fn update_signals_end(&mut self, end: recording::Time, signals: &[u32], states: &[u16]) {
        let mut prev;
        let mut changes = BTreeMap::<u32, u16>::new();
        if let Some((&t, ref mut p)) = self.points_by_time.range_mut(..=end).next_back() {
            if t == end {
                // Already have a point at end. Adjust it. prev starts unchanged...
                prev = p.prev().to_map().expect("in-mem prev is valid");

                // ...and then prev and changes are altered to reflect the desired update.
                State::update_signals_end_maps(signals, states, &mut prev, &mut changes);

                // If this doesn't alter the new state, don't dirty the database.
                if changes.is_empty() {
                    return;
                }

                // Any existing changes should still be applied. They win over reverting to prev.
                let mut it = p.changes();
                while let Some((signal, state)) = it.next().expect("in-mem changes is valid") {
                    changes.entry(signal).and_modify(|e| *e = state).or_insert(state);
                }
                self.dirty_by_time.insert(t);
                p.swap(&mut Point::new(&prev, &serialize(&changes)));
                return;
            }

            // Don't have a point at end, but do have previous state.
            prev = p.after();
        } else {
            // No point at or before end. Start from scratch (all signals unknown).
            prev = BTreeMap::new();
        }

        // Create a new end point if necessary.
        State::update_signals_end_maps(signals, states, &mut prev, &mut changes);
        if changes.is_empty() {
            return;
        }
        self.dirty_by_time.insert(end);
        self.points_by_time.insert(end, Point::new(&prev, &serialize(&changes)));
    }

    /// Helper for `update_signals_end`. Adjusts `prev` (the state prior to the end point) to
    /// reflect the desired update (in `signals` and `states`). Adjusts `changes` (changes to
    /// execute at the end point) to undo the change.
    fn update_signals_end_maps(signals: &[u32], states: &[u16], prev: &mut BTreeMap<u32, u16>,
                       changes: &mut BTreeMap<u32, u16>) {
        for (&signal, &state) in signals.iter().zip(states) {
            match prev.entry(signal) {
                Entry::Vacant(e) => {
                    changes.insert(signal, 0);
                    e.insert(state);
                },
                Entry::Occupied(mut e) => {
                    if state == 0 {
                        changes.insert(signal, *e.get());
                        e.remove();
                    } else if *e.get() != state {
                        changes.insert(signal, *e.get());
                        *e.get_mut() = state;
                    }
                },
            }
        }
    }

    /// Helper for `update_signals` to apply the start point.
    fn update_signals_start(&mut self, start: recording::Time, signals: &[u32], states: &[u16]) {
        let prev;
        if let Some((&t, ref mut p)) = self.points_by_time.range_mut(..=start).next_back() {
            if t == start {
                // Reuse existing point at start.
                prev = p.prev().to_map().expect("in-mem prev is valid");
                let mut changes = p.changes().to_map().expect("in-mem changes is valid");
                let mut dirty = false;
                for (&signal, &state) in signals.iter().zip(states) {
                    match changes.entry(signal) {
                        Entry::Occupied(mut e) => {
                            if *e.get() != state {
                                dirty = true;
                                if state == *prev.get(&signal).unwrap_or(&0) {
                                    e.remove();
                                } else {
                                    *e.get_mut() = state;
                                }
                            }
                        },
                        Entry::Vacant(e) => {
                            if signal != 0 {
                                dirty = true;
                                e.insert(state);
                            }
                        },
                    }
                }
                if dirty {
                    p.swap(&mut Point::new(&prev, &serialize(&changes)));
                    self.dirty_by_time.insert(start);
                }
                return;
            }

            // Create new point at start, using state from previous point.
            prev = p.after();
        } else {
            // Create new point at start, from scratch.
            prev = BTreeMap::new();
        }

        let mut changes = BTreeMap::new();
        for (&signal, &state) in signals.iter().zip(states) {
            if state != *prev.get(&signal).unwrap_or(&0) {
                changes.insert(signal, state);
            }
        }

        if changes.is_empty() {
            return;
        }

        self.dirty_by_time.insert(start);
        self.points_by_time.insert(start, Point::new(&prev, &serialize(&changes)));
    }

    /// Helper for `update_signals` to apply all points in `(when.start, when.end)`.
    fn update_signals_middle(&mut self, when: Range<recording::Time>, signals: &[u32],
                             states: &[u16]) {
        let mut to_delete = Vec::new();
        let after_start = recording::Time(when.start.0+1);
        for (&t, ref mut p) in self.points_by_time.range_mut(after_start..when.end) {
            let mut prev = p.prev().to_map().expect("in-mem prev is valid");

            // Update prev to reflect desired update.
            for (&signal, &state) in signals.iter().zip(states) {
                match prev.entry(signal) {
                    Entry::Occupied(mut e) => {
                        if state == 0 {
                            e.remove_entry();
                        } else if *e.get() != state {
                            *e.get_mut() = state;
                        }
                    },
                    Entry::Vacant(e) => {
                        if state != 0 {
                            e.insert(state);
                        }
                    }
                }
            }

            // Trim changes to omit any change to signals.
            let mut changes = Vec::with_capacity(3*signals.len());
            let mut it = p.changes();
            let mut next_allowed = 0;
            let mut dirty = false;
            while let Some((signal, state)) = it.next().expect("in-memory changes is valid") {
                if signals.binary_search(&signal).is_ok() { // discard.
                    dirty = true;
                } else { // keep.
                    assert!(signal >= next_allowed);
                    coding::append_varint32(signal - next_allowed, &mut changes);
                    coding::append_varint32(state as u32, &mut changes);
                    next_allowed = signal + 1;
                }
            }
            if changes.is_empty() {
                to_delete.push(t);
            } else {
                p.swap(&mut Point::new(&prev, &changes));
            }
            if dirty {
                self.dirty_by_time.insert(t);
            }
        }

        // Delete any points with no more changes.
        for &t in &to_delete {
            self.points_by_time.remove(&t).expect("point exists");
        }
    }

    /// Flushes all pending database changes to the given transaction.
    ///
    /// The caller is expected to call `post_flush` afterward if the transaction is
    /// successfully committed. No mutations should happen between these calls.
    pub fn flush(&mut self, tx: &Transaction) -> Result<(), Error> {
        let mut i_stmt = tx.prepare(r#"
            insert or replace into signal_change (time_90k, changes) values (?, ?)
        "#)?;
        let mut d_stmt = tx.prepare(r#"
            delete from signal_change where time_90k = ?
        "#)?;
        for &t in &self.dirty_by_time {
            match self.points_by_time.entry(t) {
                Entry::Occupied(ref e) => {
                    let p = e.get();
                    i_stmt.execute(params![
                        t.0,
                        &p.data[p.changes_off..],
                    ])?;
                },
                Entry::Vacant(_) => {
                    d_stmt.execute(&[t.0])?;
                },
            }
        }
        Ok(())
    }

    /// Marks that the previous `flush` was completed successfully.
    ///
    /// See notes there.
    pub fn post_flush(&mut self) {
        self.dirty_by_time.clear();
    }

    fn init_signals(conn: &Connection) -> Result<BTreeMap<u32, Signal>, Error> {
        let mut signals = BTreeMap::new();
        let mut stmt = conn.prepare(r#"
            select
                id,
                source_uuid,
                type_uuid,
                short_name
            from
                signal
        "#)?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let id = row.get(0)?;
            let source: FromSqlUuid = row.get(1)?;
            let type_: FromSqlUuid = row.get(2)?;
            signals.insert(id, Signal {
                id,
                source: source.0,
                type_: type_.0,
                short_name: row.get(3)?,
                cameras: Vec::new(),
            });
        }
        Ok(signals)
    }

    fn init_points(conn: &Connection) -> Result<BTreeMap<recording::Time, Point>, Error> {
        let mut stmt = conn.prepare(r#"
            select
                time_90k,
                changes
            from
                signal_change
            order by time_90k
        "#)?;
        let mut rows = stmt.query(params![])?;
        let mut points = BTreeMap::new();
        let mut cur = BTreeMap::new();  // latest signal -> state, where state != 0
        while let Some(row) = rows.next()? {
            let time_90k = recording::Time(row.get(0)?);
            let changes = row.get_raw_checked(1)?.as_blob()?;
            let mut it = PointDataIterator::new(changes);
            while let Some((signal, state)) = it.next()? {
                if state == 0 {
                    cur.remove(&signal);
                } else {
                    cur.insert(signal, state);
                }
            }
            points.insert(time_90k, Point::new(&cur, changes));
        }
        Ok(points)
    }

    /// Fills the `cameras` field of the `Signal` structs within the supplied `signals`.
    fn fill_signal_cameras(conn: &Connection, signals: &mut BTreeMap<u32, Signal>)
                           -> Result<(), Error> {
        let mut stmt = conn.prepare(r#"
            select
                signal_id,
                camera_id,
                type
            from
                signal_camera
            order by signal_id, camera_id
        "#)?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let signal_id = row.get(0)?;
            let s = signals.get_mut(&signal_id)
                           .ok_or_else(|| format_err!("signal_camera row for unknown signal id {}",
                                                      signal_id))?;
            let type_ = row.get(2)?;
            s.cameras.push(SignalCamera {
                camera_id: row.get(1)?,
                type_: match type_ {
                    0 => SignalCameraType::Direct,
                    1 => SignalCameraType::Indirect,
                    _ => bail!("unknown signal_camera type {}", type_),
                },
            });
        }
        Ok(())
    }

    fn init_types(conn: &Connection) -> Result<FnvHashMap<Uuid, Type>, Error> {
        let mut types = FnvHashMap::default();
        let mut stmt = conn.prepare(r#"
            select
                type_uuid,
                value,
                name,
                motion,
                color
            from
                signal_type_enum
            order by type_uuid, value
        "#)?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let type_: FromSqlUuid = row.get(0)?;
            types.entry(type_.0).or_insert_with(Type::default).states.push(TypeState {
                value: row.get(1)?,
                name: row.get(2)?,
                motion: row.get(3)?,
                color: row.get(4)?,
            });
        }
        Ok(types)
    }

    pub fn signals_by_id(&self) -> &BTreeMap<u32, Signal> { &self.signals_by_id }
    pub fn types_by_uuid(&self) -> &FnvHashMap<Uuid, Type> { & self.types_by_uuid }
}

/// Representation of a `signal` row.
#[derive(Debug)]
pub struct Signal {
    pub id: u32,
    pub source: Uuid,
    pub type_: Uuid,
    pub short_name: String,

    /// The cameras this signal is associated with. Sorted by camera id, which is unique.
    pub cameras: Vec<SignalCamera>,
}

/// Representation of a `signal_type_enum` row.
/// `type_uuid` is implied by the `Type` which owns this struct.
#[derive(Debug)]
pub struct TypeState {
    pub value: u16,
    pub name: String,
    pub motion: bool,
    pub color: String,
}

/// Representation of a signal type; currently this just gathers together the TypeStates.
#[derive(Debug, Default)]
pub struct Type {
    /// The possible states associated with this type. They are sorted by value, which is unique.
    pub states: Vec<TypeState>,
}

#[cfg(test)]
mod tests {
    use crate::{db, testutil};
    use rusqlite::Connection;
    use super::*;

    #[test]
    fn test_point_data_it() {
        // Example taken from the .sql file.
        let data = b"\x01\x01\x01\x01\xc4\x01\x02";
        let mut it = super::PointDataIterator::new(data);
        assert_eq!(it.next().unwrap(), Some((1, 1)));
        assert_eq!(it.next().unwrap(), Some((3, 1)));
        assert_eq!(it.next().unwrap(), Some((200, 2)));
        assert_eq!(it.next().unwrap(), None);
    }

    #[test]
    fn test_empty_db() {
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let s = State::init(&conn).unwrap();
        s.list_changes_by_time(recording::Time::min_value() .. recording::Time::max_value(),
                               &mut |_r| panic!("no changes expected"));
    }

    #[test]
    fn round_trip() {
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        conn.execute_batch(r#"
            insert into signal (id, source_uuid, type_uuid, short_name)
                        values (1, x'1B3889C0A59F400DA24C94EBEB19CC3A',
                                x'EE66270FD9C648198B339720D4CBCA6B', 'a'),
                               (2, x'A4A73D9A53424EBCB9F6366F1E5617FA',
                                x'EE66270FD9C648198B339720D4CBCA6B', 'b');

            insert into signal_type_enum (type_uuid, value, name, motion, color)
               values (x'EE66270FD9C648198B339720D4CBCA6B', 1, 'still', 0, 'black'),
                      (x'EE66270FD9C648198B339720D4CBCA6B', 2, 'moving', 1, 'red');
        "#).unwrap();
        let mut s = State::init(&conn).unwrap();
        s.list_changes_by_time(recording::Time::min_value() .. recording::Time::max_value(),
                               &mut |_r| panic!("no changes expected"));
        const START: recording::Time = recording::Time(140067462600000); // 2019-04-26T11:59:00
        const NOW: recording::Time = recording::Time(140067468000000);   // 2019-04-26T12:00:00
        s.update_signals(START..NOW, &[1, 2], &[2, 1]).unwrap();
        let mut rows = Vec::new();

        const EXPECTED: &[ListStateChangesRow] = &[
            ListStateChangesRow {
                when: START,
                signal: 1,
                state: 2,
            },
            ListStateChangesRow {
                when: START,
                signal: 2,
                state: 1,
            },
            ListStateChangesRow {
                when: NOW,
                signal: 1,
                state: 0,
            },
            ListStateChangesRow {
                when: NOW,
                signal: 2,
                state: 0,
            },
            ];

        s.list_changes_by_time(recording::Time::min_value() .. recording::Time::max_value(),
                               &mut |r| rows.push(*r));
        assert_eq!(&rows[..], EXPECTED);

        {
            let tx = conn.transaction().unwrap();
            s.flush(&tx).unwrap();
            tx.commit().unwrap();
        }

        drop(s);
        let s = State::init(&conn).unwrap();
        rows.clear();
        s.list_changes_by_time(recording::Time::min_value() .. recording::Time::max_value(),
                               &mut |r| rows.push(*r));
        assert_eq!(&rows[..], EXPECTED);
    }
}
