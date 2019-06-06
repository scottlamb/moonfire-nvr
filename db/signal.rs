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

use crate::coding;
use crate::db::FromSqlUuid;
use crate::recording;
use failure::{Error, bail, format_err};
use fnv::FnvHashMap;
use rusqlite::{Connection, types::ToSql};
use std::collections::BTreeMap;
use std::ops::Range;
use uuid::Uuid;

/// All state associated with signals. This is the entry point to this module.
pub(crate) struct State {
    signals_by_id: BTreeMap<u32, Signal>,
    types_by_uuid: FnvHashMap<Uuid, Type>,
    points_by_time: BTreeMap<u32, Point>,
}

struct Point {
    data: Vec<u8>,
    changes_off: usize,
}

impl Point {
    fn new(cur: &BTreeMap<u32, u16>, changes: &[u8]) -> Self {
        let mut data = Vec::with_capacity(changes.len());
        let mut last_signal = 0;
        for (&signal, &state) in cur {
            let delta = (signal - last_signal) as u32;
            coding::append_varint32(delta, &mut data);
            coding::append_varint32(state as u32, &mut data);
            last_signal = signal;
        }
        let changes_off = data.len();
        data.extend(changes);
        Point {
            data,
            changes_off,
        }
    }

    fn cur(&self) -> PointDataIterator {
        PointDataIterator::new(&self.data[0..self.changes_off])
    }

    fn changes(&self) -> PointDataIterator {
        PointDataIterator::new(&self.data[self.changes_off..])
    }
}

struct PointDataIterator<'a> {
    data: &'a [u8],
    cur_pos: usize,
    cur_signal: u32,
    cur_state: u16,
}

impl<'a> PointDataIterator<'a> {
    fn new(data: &'a [u8]) -> Self {
        PointDataIterator {
            data,
            cur_pos: 0,
            cur_signal: 0,
            cur_state: 0,
        }
    }

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
        self.cur_signal = signal;
        self.cur_state = state as u16;
        Ok(Some((signal, self.cur_state)))
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

#[derive(Debug)]
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
        })
    }

    pub fn list_changes_by_time(
        &self, desired_time: Range<recording::Time>, f: &mut FnMut(&ListStateChangesRow))
        -> Result<(), Error> {

        // Convert the desired range to seconds. Reducing precision of the end carefully.
        let start = desired_time.start.unix_seconds() as u32;
        let mut end = desired_time.end.unix_seconds();
        end += ((end * recording::TIME_UNITS_PER_SEC) < desired_time.end.0) as i64;
        let end = end as u32;

        // First find the state immediately before. If it exists, include it.
        if let Some((&t, p)) = self.points_by_time.range(..start).next_back() {
            let mut cur = BTreeMap::new();
            let mut it = p.cur();
            while let Some((signal, state)) = it.next()? {
                cur.insert(signal, state);
            }
            let mut it = p.changes();
            while let Some((signal, state)) = it.next()? {
                if state == 0 {
                    cur.remove(&signal);
                } else {
                    cur.insert(signal, state);
                }
            }
            for (&signal, &state) in &cur {
                f(&ListStateChangesRow {
                    when: recording::Time(t as i64 * recording::TIME_UNITS_PER_SEC),
                    signal,
                    state,
                });
            }
        }

        // Then include changes up to (but not including) the end time.
        for (&t, p) in self.points_by_time.range(start..end) {
            let mut it = p.changes();
            while let Some((signal, state)) = it.next()? {
                f(&ListStateChangesRow {
                    when: recording::Time(t as i64 * recording::TIME_UNITS_PER_SEC),
                    signal,
                    state,
                });
            }
        }

        Ok(())
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
        let mut rows = stmt.query(&[] as &[&ToSql])?;
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

    fn init_points(conn: &Connection) -> Result<BTreeMap<u32, Point>, Error> {
        let mut stmt = conn.prepare(r#"
            select
                time_sec,
                changes
            from
                signal_state
            order by time_sec
        "#)?;
        let mut rows = stmt.query(&[] as &[&ToSql])?;
        let mut points = BTreeMap::new();
        let mut cur = BTreeMap::new();  // latest signal -> state, where state != 0
        while let Some(row) = rows.next()? {
            let time_sec = row.get(0)?;
            let changes = row.get_raw_checked(1)?.as_blob()?;
            let mut it = PointDataIterator::new(changes);
            while let Some((signal, state)) = it.next()? {
                if state == 0 {
                    cur.remove(&signal);
                } else {
                    cur.insert(signal, state);
                }
            }
            points.insert(time_sec, Point::new(&cur, changes));
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
        let mut rows = stmt.query(&[] as &[&ToSql])?;
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
        let mut rows = stmt.query(&[] as &[&ToSql])?;
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
    #[test]
    fn test_point_data_it() {
        // Example taken from the .sql file.
        let data = b"\x01\x01\x02\x01\x00\x00\xc5\x01\x02";
        let mut it = super::PointDataIterator::new(data);
        assert_eq!(it.next().unwrap(), Some((1, 1)));
        assert_eq!(it.next().unwrap(), Some((3, 1)));
        assert_eq!(it.next().unwrap(), Some((3, 0)));
        assert_eq!(it.next().unwrap(), Some((200, 2)));
        assert_eq!(it.next().unwrap(), None);
    }
}
