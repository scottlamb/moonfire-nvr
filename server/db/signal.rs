// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2019 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Schema for "signals": enum-valued timeserieses.
//! See the `signal` table within `schema.sql` for more information.

use crate::json::{SignalConfig, SignalTypeConfig};
use crate::{coding, days};
use crate::{recording, SqlUuid};
use base::bail_t;
use failure::{bail, format_err, Error};
use fnv::FnvHashMap;
use rusqlite::{params, Connection, Transaction};
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::convert::TryFrom;
use std::ops::Range;
use tracing::debug;
use uuid::Uuid;

/// All state associated with signals. This is the entry point to this module.
pub(crate) struct State {
    signals_by_id: BTreeMap<u32, Signal>,

    /// All types with known states. Note that currently there's no requirement an entry here
    /// exists for every `type_` specified in a `Signal`, and there's an implied `0` (unknown)
    /// state for every `Type`.
    types_by_uuid: FnvHashMap<Uuid, Type>,

    /// All points in time.
    /// Invariants, checked by `State::debug_assert_point_invariants`:
    /// *   the first point must have an empty previous state (all signals at state 0).
    /// *   each point's prev state matches the previous point's after state.
    /// *   the last point must have an empty final state (all signals changed to state 0).
    points_by_time: BTreeMap<recording::Time, Point>,

    /// Times which need to be flushed to the database.
    /// These either have a matching `points_by_time` entry or represent a removal.
    dirty_by_time: BTreeSet<recording::Time>,

    max_signal_changes: Option<u32>,
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
        self.changes().update_map(&mut after);
        after
    }
}

/// Appends a serialized form of `from` into `to`.
///
/// `from` must be an iterator of `(signal, state)` with signal numbers in monotonically increasing
/// order.
fn append_serialized<'a, I>(from: I, to: &mut Vec<u8>)
where
    I: IntoIterator<Item = (&'a u32, &'a u16)>,
{
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
        let (signal_delta, p) = coding::decode_varint32(self.data, self.cur_pos).map_err(|()| {
            format_err!(
                "varint32 decode failure; data={:?} pos={}",
                self.data,
                self.cur_pos
            )
        })?;
        let (state, p) = coding::decode_varint32(self.data, p)
            .map_err(|()| format_err!("varint32 decode failure; data={:?} pos={}", self.data, p))?;
        let signal = self.cur_signal.checked_add(signal_delta).ok_or_else(|| {
            format_err!("signal overflow: {} + {}", self.cur_signal, signal_delta)
        })?;
        if state > u16::max_value() as u32 {
            bail!("state overflow: {}", state);
        }
        self.cur_pos = p;
        self.cur_signal = signal + 1;
        Ok(Some((signal, state as u16)))
    }

    fn into_map(mut self) -> Result<BTreeMap<u32, u16>, Error> {
        let mut out = BTreeMap::new();
        while let Some((signal, state)) = self.next()? {
            out.insert(signal, state);
        }
        Ok(out)
    }

    fn update_map(mut self, m: &mut BTreeMap<u32, u16>) {
        while let Some((signal, state)) = self.next().expect("in-mem changes is valid") {
            if state == 0 {
                m.remove(&signal);
            } else {
                m.insert(signal, state);
            }
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ListStateChangesRow {
    pub when: recording::Time,
    pub signal: u32,
    pub state: u16,
}

impl State {
    pub fn init(conn: &Connection, config: &crate::json::GlobalConfig) -> Result<Self, Error> {
        let mut signals_by_id = State::init_signals(conn)?;
        let mut points_by_time = BTreeMap::new();
        State::fill_points(conn, &mut points_by_time, &mut signals_by_id)?;
        let s = State {
            max_signal_changes: config.max_signal_changes,
            signals_by_id,
            types_by_uuid: State::init_types(conn)?,
            points_by_time,
            dirty_by_time: BTreeSet::new(),
        };
        s.debug_assert_point_invariants();
        Ok(s)
    }

    pub fn list_changes_by_time(
        &self,
        desired_time: Range<recording::Time>,
        f: &mut dyn FnMut(&ListStateChangesRow),
    ) {
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
        // BTreeMap has a strange behavior in which it will panic if end < start, even though
        // std::ops::Range says "it is empty if start >= end". Make the behavior sane by hand.
        let t = desired_time.start..std::cmp::max(desired_time.end, desired_time.start);
        for (&when, p) in self.points_by_time.range(t) {
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
        &mut self,
        when: Range<recording::Time>,
        signals: &[u32],
        states: &[u16],
    ) -> Result<(), base::Error> {
        // Do input validation before any mutation.
        self.update_signals_validate(signals, states)?;

        // Follow the std::ops::Range convention of considering a range empty if its start >= end.
        // Bailing early in the empty case isn't just an optimization; apply_observation_end would
        // be incorrect otherwise.
        if when.end <= when.start {
            return Ok(());
        }

        // Apply the end before the start so that the `prev` state can be examined.
        self.update_signals_end(when.clone(), signals, states);
        self.update_signals_start(when.start, signals, states);
        self.update_signals_middle(when, signals, states);
        self.debug_assert_point_invariants();

        self.gc();
        Ok(())
    }

    /// Performs garbage collection if the number of points exceeds `max_signal_changes`.
    fn gc(&mut self) {
        let max = match self.max_signal_changes {
            None => return,
            Some(m) => m as usize,
        };
        let to_remove = match self.points_by_time.len().checked_sub(max) {
            None => return,
            Some(p) => p,
        };
        debug!(
            "Performing signal GC: have {} points, want only {}, so removing {}",
            self.points_by_time.len(),
            max,
            to_remove
        );

        self.gc_days(to_remove);
        let remove: smallvec::SmallVec<[recording::Time; 4]> = self
            .points_by_time
            .keys()
            .take(to_remove)
            .copied()
            .collect();

        for t in &remove {
            self.points_by_time.remove(t);
            self.dirty_by_time.insert(*t);
        }

        // Update the first remaining point to keep state starting from it unchanged.
        let (t, p) = match self.points_by_time.iter_mut().next() {
            Some(e) => e,
            None => return,
        };
        let combined = p.after();
        p.changes_off = 0;
        p.data = serialize(&combined).into_boxed_slice();
        self.dirty_by_time.insert(*t);
        self.debug_assert_point_invariants();
    }

    /// Adjusts each signal's days index to reflect garbage-collecting the first `to_remove` points.
    fn gc_days(&mut self, to_remove: usize) {
        let mut it = self.points_by_time.iter().take(to_remove + 1);
        let (mut prev_time, mut prev_state) = match it.next() {
            None => return, // nothing to do.
            Some(p) => (*p.0, p.1.after()),
        };
        for (&new_time, point) in it {
            let mut changes = point.changes();
            while let Some((signal, state)) = changes.next().expect("in-mem points valid") {
                let s = self
                    .signals_by_id
                    .get_mut(&signal)
                    .expect("in-mem point signals valid");
                let prev_state = prev_state.entry(signal).or_default();
                s.days.adjust(prev_time..new_time, *prev_state, state);
                *prev_state = state;
            }
            prev_time = new_time;
        }
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
                Some(s) => {
                    let states = self
                        .types_by_uuid
                        .get(&s.type_)
                        .map(|t| t.valid_states)
                        .unwrap_or(0);
                    if state >= 16 || (states & (1 << state)) == 0 {
                        bail_t!(
                            FailedPrecondition,
                            "signal {} specifies unknown state {}",
                            signal,
                            state
                        );
                    }
                }
            }
            next_allowed = signal + 1;
        }
        Ok(())
    }

    /// Helper for `update_signals` to apply the end point.
    fn update_signals_end(
        &mut self,
        when: Range<recording::Time>,
        signals: &[u32],
        states: &[u16],
    ) {
        let mut prev;
        let mut changes = BTreeMap::<u32, u16>::new();
        let prev_t = self
            .points_by_time
            .range(when.clone())
            .next_back()
            .map(|e| *e.0)
            .unwrap_or(when.start);
        let days_range = prev_t..when.end;
        if let Some((&t, ref mut p)) = self.points_by_time.range_mut(..=when.end).next_back() {
            if t == when.end {
                // Already have a point at end. Adjust it. prev starts unchanged...
                prev = p.prev().into_map().expect("in-mem prev is valid");

                // ...and then prev and changes are altered to reflect the desired update.
                State::update_signals_end_maps(
                    signals,
                    states,
                    days_range,
                    &mut self.signals_by_id,
                    &mut prev,
                    &mut changes,
                );

                // If this doesn't alter the new state, don't dirty the database.
                if changes.is_empty() {
                    return;
                }

                // Any existing changes should still be applied. They win over reverting to prev.
                let mut it = p.changes();
                while let Some((signal, state)) = it.next().expect("in-mem changes is valid") {
                    changes
                        .entry(signal)
                        .and_modify(|e| *e = state)
                        .or_insert(state);
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
        State::update_signals_end_maps(
            signals,
            states,
            days_range,
            &mut self.signals_by_id,
            &mut prev,
            &mut changes,
        );
        if changes.is_empty() {
            return;
        }
        self.dirty_by_time.insert(when.end);
        self.points_by_time
            .insert(when.end, Point::new(&prev, &serialize(&changes)));
    }

    /// Helper for `update_signals_end`. Adjusts `prev` (the state prior to the end point) to
    /// reflect the desired update (in `signals` and `states`). Adjusts `changes` (changes to
    /// execute at the end point) to undo the change. Adjust each signal's days index for
    /// the range from the penultimate point of the range (or lacking that, its start) to the end.
    fn update_signals_end_maps(
        signals: &[u32],
        states: &[u16],
        days_range: Range<recording::Time>,
        signals_by_id: &mut BTreeMap<u32, Signal>,
        prev: &mut BTreeMap<u32, u16>,
        changes: &mut BTreeMap<u32, u16>,
    ) {
        for (&signal, &state) in signals.iter().zip(states) {
            let old_state;
            match prev.entry(signal) {
                Entry::Vacant(e) => {
                    old_state = 0;
                    changes.insert(signal, 0);
                    e.insert(state);
                }
                Entry::Occupied(mut e) => {
                    old_state = *e.get();
                    if state == 0 {
                        changes.insert(signal, *e.get());
                        e.remove();
                    } else if *e.get() != state {
                        changes.insert(signal, *e.get());
                        *e.get_mut() = state;
                    }
                }
            }
            signals_by_id
                .get_mut(&signal)
                .expect("signal valid")
                .days
                .adjust(days_range.clone(), old_state, state);
        }
    }

    /// Helper for `update_signals` to apply the start point.
    fn update_signals_start(&mut self, start: recording::Time, signals: &[u32], states: &[u16]) {
        let prev;
        if let Some((&t, ref mut p)) = self.points_by_time.range_mut(..=start).next_back() {
            if t == start {
                // Reuse existing point at start.
                prev = p.prev().into_map().expect("in-mem prev is valid");
                let mut changes = p.changes().into_map().expect("in-mem changes is valid");
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
                        }
                        Entry::Vacant(e) => {
                            if signal != 0 {
                                dirty = true;
                                e.insert(state);
                            }
                        }
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
        self.points_by_time
            .insert(start, Point::new(&prev, &serialize(&changes)));
    }

    /// Helper for `update_signals` to apply all points in `(when.start, when.end)`.
    /// This also updates each signal's days index for the points it finds.
    fn update_signals_middle(
        &mut self,
        when: Range<recording::Time>,
        signals: &[u32],
        states: &[u16],
    ) {
        let mut to_delete = Vec::new();
        let after_start = recording::Time(when.start.0 + 1);
        let mut prev_t = when.start;
        for (&t, ref mut p) in self.points_by_time.range_mut(after_start..when.end) {
            let mut prev = p.prev().into_map().expect("in-mem prev is valid");

            // Update prev to reflect desired update; likewise each signal's days index.
            for (&signal, &state) in signals.iter().zip(states) {
                let s = self.signals_by_id.get_mut(&signal).expect("valid signals");
                let prev_state;
                match prev.entry(signal) {
                    Entry::Occupied(mut e) => {
                        prev_state = *e.get();
                        if state == 0 {
                            e.remove_entry();
                        } else if *e.get() != state {
                            *e.get_mut() = state;
                        }
                    }
                    Entry::Vacant(e) => {
                        prev_state = 0;
                        if state != 0 {
                            e.insert(state);
                        }
                    }
                }
                s.days.adjust(prev_t..t, prev_state, state);
                prev_t = t;
            }

            // Trim changes to omit any change to signals.
            let mut changes = Vec::with_capacity(3 * signals.len());
            let mut it = p.changes();
            let mut next_allowed = 0;
            let mut dirty = false;
            while let Some((signal, state)) = it.next().expect("in-memory changes is valid") {
                if signals.binary_search(&signal).is_ok() {
                    // discard.
                    dirty = true;
                } else {
                    // keep.
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
        let mut i_stmt = tx.prepare(
            r#"
            insert or replace into signal_change (time_90k, changes) values (?, ?)
            "#,
        )?;
        let mut d_stmt = tx.prepare(
            r#"
            delete from signal_change where time_90k = ?
            "#,
        )?;
        for &t in &self.dirty_by_time {
            match self.points_by_time.entry(t) {
                Entry::Occupied(ref e) => {
                    let p = e.get();
                    i_stmt.execute(params![t.0, &p.data[p.changes_off..],])?;
                }
                Entry::Vacant(_) => {
                    d_stmt.execute(params![t.0])?;
                }
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
        let mut stmt = conn.prepare(
            r#"
            select
                id,
                uuid,
                type_uuid,
                config
            from
                signal
            "#,
        )?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let id: i32 = row.get(0)?;
            let id = u32::try_from(id)?;
            let uuid: SqlUuid = row.get(1)?;
            let type_: SqlUuid = row.get(2)?;
            let config: SignalConfig = row.get(3)?;
            signals.insert(
                id,
                Signal {
                    id,
                    uuid: uuid.0,
                    days: days::Map::default(),
                    type_: type_.0,
                    config,
                },
            );
        }
        Ok(signals)
    }

    fn init_types(conn: &Connection) -> Result<FnvHashMap<Uuid, Type>, Error> {
        let mut types = FnvHashMap::default();
        let mut stmt = conn.prepare(
            r#"
            select
                uuid,
                config
            from
                signal_type
            "#,
        )?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let uuid: SqlUuid = row.get(0)?;
            let mut type_ = Type {
                valid_states: 1, // bit 0 (unknown state) is always valid.
                config: row.get(1)?,
            };
            for &value in type_.config.values.keys() {
                if value == 0 || value >= 16 {
                    bail!(
                        "signal type {} value {} out of accepted range [0, 16)",
                        uuid.0,
                        value
                    );
                }
                type_.valid_states |= 1 << value;
            }
            types.insert(uuid.0, type_);
        }
        Ok(types)
    }

    /// Fills `points_by_time` from the database, also filling the `days`
    /// index of each signal.
    fn fill_points(
        conn: &Connection,
        points_by_time: &mut BTreeMap<recording::Time, Point>,
        signals_by_id: &mut BTreeMap<u32, Signal>,
    ) -> Result<(), Error> {
        let mut stmt = conn.prepare(
            r#"
            select
                time_90k,
                changes
            from
                signal_change
            order by time_90k
            "#,
        )?;
        let mut rows = stmt.query(params![])?;
        let mut cur = BTreeMap::new(); // latest signal -> state, where state != 0

        let mut sig_last_state = BTreeMap::new();
        while let Some(row) = rows.next()? {
            let time_90k = recording::Time(row.get(0)?);

            let changes = row.get_ref(1)?.as_blob()?;
            let before = cur.clone();
            let mut it = PointDataIterator::new(changes);
            while let Some((signal, state)) = it.next()? {
                let e = sig_last_state.entry(signal);
                if let Entry::Occupied(ref e) = e {
                    let (prev_time, prev_state) = *e.get();
                    let s = signals_by_id.get_mut(&signal).ok_or_else(|| {
                        format_err!("time {} references invalid signal {}", time_90k, signal)
                    })?;
                    s.days.adjust(prev_time..time_90k, 0, prev_state);
                }
                if state == 0 {
                    cur.remove(&signal);
                    if let Entry::Occupied(e) = e {
                        e.remove_entry();
                    }
                } else {
                    cur.insert(signal, state);
                    *e.or_default() = (time_90k, state);
                }
            }
            points_by_time.insert(time_90k, Point::new(&before, changes));
        }
        if !cur.is_empty() {
            bail!(
                "far future state should be unknown for all signals; is: {:?}",
                cur
            );
        }
        Ok(())
    }

    pub fn signals_by_id(&self) -> &BTreeMap<u32, Signal> {
        &self.signals_by_id
    }
    pub fn types_by_uuid(&self) -> &FnvHashMap<Uuid, Type> {
        &self.types_by_uuid
    }

    #[cfg(not(debug_assertions))]
    fn debug_assert_point_invariants(&self) {}

    /// Checks invariants on `points_by_time` (expensive).
    #[cfg(debug_assertions)]
    fn debug_assert_point_invariants(&self) {
        let mut expected_prev = BTreeMap::new();
        for (t, p) in self.points_by_time.iter() {
            let cur = p.prev().into_map().expect("in-mem prev is valid");
            assert_eq!(&expected_prev, &cur, "time {t} prev mismatch");
            p.changes().update_map(&mut expected_prev);
        }
        assert_eq!(
            expected_prev.len(),
            0,
            "last point final state should be empty"
        );
    }
}

/// Representation of a `signal` row.
#[derive(Debug)]
pub struct Signal {
    pub id: u32,
    pub uuid: Uuid,
    pub type_: Uuid,
    pub days: days::Map<days::SignalValue>,
    pub config: SignalConfig,
}

#[derive(Debug, Default)]
pub struct Type {
    pub valid_states: u16,
    pub config: SignalTypeConfig,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db,
        json::{GlobalConfig, SignalTypeConfig, SignalTypeValueConfig},
        testutil,
    };
    use rusqlite::Connection;
    use smallvec::smallvec;

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
        let s = State::init(&conn, &GlobalConfig::default()).unwrap();
        s.list_changes_by_time(
            recording::Time::min_value()..recording::Time::max_value(),
            &mut |_r| panic!("no changes expected"),
        );
    }

    #[test]
    fn round_trip() {
        testutil::init();
        let mut conn = Connection::open_in_memory().unwrap();
        db::init(&mut conn).unwrap();
        let mut type_config = SignalTypeConfig::default();
        type_config.values.insert(
            1,
            SignalTypeValueConfig {
                name: "still".to_owned(),
                motion: false,
                color: "black".to_owned(),
                ..Default::default()
            },
        );
        type_config.values.insert(
            2,
            SignalTypeValueConfig {
                name: "moving".to_owned(),
                motion: true,
                color: "red".to_owned(),
                ..Default::default()
            },
        );
        conn.execute(
            "insert into signal_type (uuid, config) values (?, ?)",
            params![
                SqlUuid(Uuid::parse_str("ee66270f-d9c6-4819-8b33-9720d4cbca6b").unwrap()),
                &type_config,
            ],
        )
        .unwrap();
        conn.execute_batch(
            r#"
            insert into signal (id, uuid, type_uuid, config)
                        values (1, x'1B3889C0A59F400DA24C94EBEB19CC3A',
                                x'EE66270FD9C648198B339720D4CBCA6B', '{"name": "a"}'),
                               (2, x'A4A73D9A53424EBCB9F6366F1E5617FA',
                                x'EE66270FD9C648198B339720D4CBCA6B', '{"name": "b"}');

            "#,
        )
        .unwrap();
        let config = GlobalConfig {
            max_signal_changes: Some(2),
            ..Default::default()
        };
        let mut s = State::init(&conn, &config).unwrap();
        s.list_changes_by_time(
            recording::Time::min_value()..recording::Time::max_value(),
            &mut |_r| panic!("no changes expected"),
        );
        const START: recording::Time = recording::Time(140067462600000); // 2019-04-26T11:59:00
        const NOW: recording::Time = recording::Time(140067468000000); // 2019-04-26T12:00:00
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

        s.list_changes_by_time(
            recording::Time::min_value()..recording::Time::max_value(),
            &mut |r| rows.push(*r),
        );
        s.list_changes_by_time(
            recording::Time::max_value()..recording::Time::min_value(),
            &mut |_r| panic!("no changes expected"),
        );
        assert_eq!(&rows[..], EXPECTED);
        let mut expected_days = days::Map::default();
        expected_days.0.insert(
            days::Key(*b"2019-04-26"),
            days::SignalValue {
                states: smallvec![0, (NOW - START).0 as u64],
            },
        );
        assert_eq!(&s.signals_by_id.get(&1).unwrap().days, &expected_days);
        expected_days.0.clear();
        expected_days.0.insert(
            days::Key(*b"2019-04-26"),
            days::SignalValue {
                states: smallvec![(NOW - START).0 as u64],
            },
        );
        assert_eq!(&s.signals_by_id.get(&2).unwrap().days, &expected_days);

        {
            let tx = conn.transaction().unwrap();
            s.flush(&tx).unwrap();
            tx.commit().unwrap();
        }

        drop(s);
        let mut s = State::init(&conn, &config).unwrap();
        rows.clear();
        s.list_changes_by_time(
            recording::Time::min_value()..recording::Time::max_value(),
            &mut |r| rows.push(*r),
        );
        assert_eq!(&rows[..], EXPECTED);

        // Go through it again. This time, hit the max number of signals, forcing START to be
        // dropped.
        const SOON: recording::Time = recording::Time(140067473400000); // 2019-04-26T12:01:00
        s.update_signals(NOW..SOON, &[1, 2], &[1, 2]).unwrap();
        rows.clear();
        const EXPECTED2: &[ListStateChangesRow] = &[
            ListStateChangesRow {
                when: NOW,
                signal: 1,
                state: 1,
            },
            ListStateChangesRow {
                when: NOW,
                signal: 2,
                state: 2,
            },
            ListStateChangesRow {
                when: SOON,
                signal: 1,
                state: 0,
            },
            ListStateChangesRow {
                when: SOON,
                signal: 2,
                state: 0,
            },
        ];
        s.list_changes_by_time(
            recording::Time::min_value()..recording::Time::max_value(),
            &mut |r| rows.push(*r),
        );
        assert_eq!(&rows[..], EXPECTED2);

        {
            let tx = conn.transaction().unwrap();
            s.flush(&tx).unwrap();
            tx.commit().unwrap();
        }
        drop(s);
        let s = State::init(&conn, &config).unwrap();
        rows.clear();
        s.list_changes_by_time(
            recording::Time::min_value()..recording::Time::max_value(),
            &mut |r| rows.push(*r),
        );
        assert_eq!(&rows[..], EXPECTED2);
    }
}
