// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! In-memory indexes by calendar day.

use base::time::{Duration, Time, TIME_UNITS_PER_SEC};
use base::{err, Error};
use smallvec::SmallVec;
use std::cmp;
use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::io::Write;
use std::ops::Range;
use std::str;
use tracing::{error, trace};

/// A calendar day in `YYYY-mm-dd` format.
#[derive(Copy, Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct Key(pub(crate) [u8; 10]);

impl Key {
    fn new(tm: time::Tm) -> Result<Self, Error> {
        let mut s = Key([0u8; 10]);
        write!(
            &mut s.0[..],
            "{}",
            tm.strftime("%Y-%m-%d")
                .map_err(|e| err!(Internal, source(e)))?
        )?;
        Ok(s)
    }

    pub fn bounds(&self) -> Range<Time> {
        let mut my_tm = time::strptime(self.as_ref(), "%Y-%m-%d").expect("days must be parseable");
        my_tm.tm_utcoff = 1; // to the time crate, values != 0 mean local time.
        my_tm.tm_isdst = -1;
        let start = Time(my_tm.to_timespec().sec * TIME_UNITS_PER_SEC);
        my_tm.tm_hour = 0;
        my_tm.tm_min = 0;
        my_tm.tm_sec = 0;
        my_tm.tm_mday += 1;
        let end = Time(my_tm.to_timespec().sec * TIME_UNITS_PER_SEC);
        start..end
    }
}

impl AsRef<str> for Key {
    fn as_ref(&self) -> &str {
        str::from_utf8(&self.0[..]).expect("days are always UTF-8")
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", self.as_ref())
    }
}

pub trait Value: std::fmt::Debug + Default {
    type Change: std::fmt::Debug;

    /// Applies the given change to this value.
    fn apply(&mut self, c: &Self::Change);

    fn is_empty(&self) -> bool;
}

/// In-memory state about a particular stream on a particular day.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamValue {
    /// The number of recordings that overlap with this day.
    pub recordings: i64,

    /// The total duration recorded on this day. This can be 0; because frames' durations are taken
    /// from the time of the next frame, a recording that ends unexpectedly after a single frame
    /// will have 0 duration of that frame and thus the whole recording.
    pub duration: Duration,
}

impl Value for StreamValue {
    type Change = Self;

    fn apply(&mut self, c: &StreamValue) {
        self.recordings += c.recordings;
        self.duration += c.duration;
    }

    fn is_empty(&self) -> bool {
        self.recordings == 0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignalValue {
    /// `states[i]` represents the amount of time spent in state `i+1`.
    /// (The signal is the unknown state, 0, for the remainder of the time.)
    pub states: SmallVec<[u64; 4]>,
}

impl Value for SignalValue {
    type Change = SignalChange;

    fn apply(&mut self, c: &SignalChange) {
        if self.states.len() < usize::try_from(c.new_state).unwrap() {
            self.states.resize(c.new_state as usize, 0);
        }

        if c.new_state > 0 {
            // add to new state.
            let s = &mut self.states[c.new_state as usize - 1];
            let n = s
                .checked_add(u64::try_from(c.duration.0).unwrap())
                .unwrap_or_else(|| panic!("add range violation: s={s:?} c={c:?}"));
            *s = n;
        }

        if c.old_state > 0 {
            // remove from old state.
            let i = usize::try_from(c.old_state).unwrap() - 1;
            assert!(
                self.states.len() > i,
                "no such old state: s={self:?} c={c:?}"
            );
            let s = &mut self.states[c.old_state as usize - 1];
            let n = s
                .checked_sub(u64::try_from(c.duration.0).unwrap())
                .unwrap_or_else(|| panic!("sub range violation: s={s:?} c={c:?}"));
            *s = n;
        }

        // Normalize.
        let mut l = self.states.len();
        while l > 0 && self.states[l - 1] == 0 {
            l -= 1;
        }
        self.states.truncate(l);
    }

    fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}

/// A change to a signal within a single day.
#[derive(Debug)]
pub struct SignalChange {
    /// The duration of time being altered.
    duration: Duration,

    /// The state of the given range before this change.
    old_state: u16,

    /// The state of the given range after this change.
    new_state: u16,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Map<V: Value>(pub(crate) BTreeMap<Key, V>);

impl<V: Value> Map<V> {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn get(&self, k: &Key) -> Option<&V> {
        self.0.get(k)
    }

    /// Adds non-zero `delta` to the day represented by `day` in the map `m`.
    /// Inserts a map entry if absent; removes the entry if it has 0 entries on exit.
    fn adjust_day(&mut self, day: Key, c: V::Change) {
        trace!("adjust_day {} {:?}", day.as_ref(), &c);
        use ::std::collections::btree_map::Entry;
        match self.0.entry(day) {
            Entry::Vacant(e) => e.insert(Default::default()).apply(&c),
            Entry::Occupied(mut e) => {
                let v = e.get_mut();
                v.apply(&c);
                if v.is_empty() {
                    e.remove_entry();
                }
            }
        }
    }
}

impl<'a, V: Value> IntoIterator for &'a Map<V> {
    type Item = (&'a Key, &'a V);
    type IntoIter = std::collections::btree_map::Iter<'a, Key, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl Map<StreamValue> {
    /// Adjusts `self` to reflect the range of the given recording.
    /// Note that the specified range may span two days. It will never span more because the maximum
    /// length of a recording entry is less than a day (even a 23-hour "spring forward" day).
    ///
    /// This function swallows/logs date formatting errors because they shouldn't happen and there's
    /// not much that can be done about them. (The database operation has already gone through.)
    pub(crate) fn adjust(&mut self, r: Range<Time>, sign: i64) {
        // Find first day key.
        let sec = r.start.unix_seconds();
        let mut my_tm = time::at(time::Timespec { sec, nsec: 0 });
        let day = match Key::new(my_tm) {
            Ok(d) => d,
            Err(ref e) => {
                error!(
                    "Unable to fill first day key from {:?}->{:?}: {}; will ignore.",
                    r, my_tm, e
                );
                return;
            }
        };

        // Determine the start of the next day.
        // Use mytm to hold a non-normalized representation of the boundary.
        my_tm.tm_isdst = -1;
        my_tm.tm_hour = 0;
        my_tm.tm_min = 0;
        my_tm.tm_sec = 0;
        my_tm.tm_mday += 1;
        let boundary = my_tm.to_timespec();
        let boundary_90k = boundary.sec * TIME_UNITS_PER_SEC;

        // Adjust the first day.
        let first_day_delta = StreamValue {
            recordings: sign,
            duration: Duration(sign * (cmp::min(r.end.0, boundary_90k) - r.start.0)),
        };
        self.adjust_day(day, first_day_delta);

        if r.end.0 <= boundary_90k {
            return;
        }

        // Fill day with the second day. This requires a normalized representation so recalculate.
        // (The C mktime(3) already normalized for us once, but .to_timespec() discarded that
        // result.)
        let my_tm = time::at(boundary);
        let day = match Key::new(my_tm) {
            Ok(d) => d,
            Err(ref e) => {
                error!(
                    "Unable to fill second day key from {:?}: {}; will ignore.",
                    my_tm, e
                );
                return;
            }
        };
        let second_day_delta = StreamValue {
            recordings: sign,
            duration: Duration(sign * (r.end.0 - boundary_90k)),
        };
        self.adjust_day(day, second_day_delta);
    }
}

impl Map<SignalValue> {
    /// Adjusts `self` to reflect the range of the given recording.
    /// Note that the specified range may span several days (unlike StreamValue).
    ///
    /// This function swallows/logs date formatting errors because they shouldn't happen and there's
    /// not much that can be done about them. (The database operation has already gone through.)
    pub(crate) fn adjust(&mut self, mut r: Range<Time>, old_state: u16, new_state: u16) {
        // Find first day key.
        let sec = r.start.unix_seconds();
        let mut my_tm = time::at(time::Timespec { sec, nsec: 0 });
        let mut day = match Key::new(my_tm) {
            Ok(d) => d,
            Err(ref e) => {
                error!(
                    "Unable to fill first day key from {:?}->{:?}: {}; will ignore.",
                    r, my_tm, e
                );
                return;
            }
        };

        // Determine the start of the next day.
        // Use mytm to hold a non-normalized representation of the boundary.
        my_tm.tm_isdst = -1;
        my_tm.tm_hour = 0;
        my_tm.tm_min = 0;
        my_tm.tm_sec = 0;

        loop {
            my_tm.tm_mday += 1;
            let boundary_90k = my_tm.to_timespec().sec * TIME_UNITS_PER_SEC;

            // Adjust this day.
            let duration = Duration(cmp::min(r.end.0, boundary_90k) - r.start.0);
            self.adjust_day(
                day,
                SignalChange {
                    duration,
                    old_state,
                    new_state,
                },
            );

            if r.end.0 <= boundary_90k {
                return;
            }

            // Fill day with the next day. This requires a normalized representation so
            // recalculate. (The C mktime(3) already normalized for us once, but .to_timespec()
            // discarded that result.)
            let my_tm = time::at(time::Timespec {
                sec: Time(boundary_90k).unix_seconds(),
                nsec: 0,
            });
            day = match Key::new(my_tm) {
                Ok(d) => d,
                Err(ref e) => {
                    error!(
                        "Unable to fill day key from {:?}: {}; will ignore.",
                        my_tm, e
                    );
                    return;
                }
            };
            r.start.0 = boundary_90k;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Key, Map, SignalValue, StreamValue};
    use crate::testutil;
    use base::time::{Duration, Time, TIME_UNITS_PER_SEC};
    use smallvec::smallvec;

    #[test]
    fn test_adjust_stream() {
        testutil::init();
        let mut m: Map<StreamValue> = Map::default();

        // Create a day.
        let test_time = Time(130647162600000i64); // 2015-12-31 23:59:00 (Pacific).
        let one_min = Duration(60 * TIME_UNITS_PER_SEC);
        let two_min = Duration(2 * 60 * TIME_UNITS_PER_SEC);
        let three_min = Duration(3 * 60 * TIME_UNITS_PER_SEC);
        let four_min = Duration(4 * 60 * TIME_UNITS_PER_SEC);
        let test_day1 = &Key(*b"2015-12-31");
        let test_day2 = &Key(*b"2016-01-01");
        m.adjust(test_time..test_time + one_min, 1);
        assert_eq!(1, m.len());
        assert_eq!(
            Some(&StreamValue {
                recordings: 1,
                duration: one_min
            }),
            m.get(test_day1)
        );

        // Add to a day.
        m.adjust(test_time..test_time + one_min, 1);
        assert_eq!(1, m.len());
        assert_eq!(
            Some(&StreamValue {
                recordings: 2,
                duration: two_min
            }),
            m.get(test_day1)
        );

        // Subtract from a day.
        m.adjust(test_time..test_time + one_min, -1);
        assert_eq!(1, m.len());
        assert_eq!(
            Some(&StreamValue {
                recordings: 1,
                duration: one_min
            }),
            m.get(test_day1)
        );

        // Remove a day.
        m.adjust(test_time..test_time + one_min, -1);
        assert_eq!(0, m.len());

        // Create two days.
        m.adjust(test_time..test_time + three_min, 1);
        assert_eq!(2, m.len());
        assert_eq!(
            Some(&StreamValue {
                recordings: 1,
                duration: one_min
            }),
            m.get(test_day1)
        );
        assert_eq!(
            Some(&StreamValue {
                recordings: 1,
                duration: two_min
            }),
            m.get(test_day2)
        );

        // Add to two days.
        m.adjust(test_time..test_time + three_min, 1);
        assert_eq!(2, m.len());
        assert_eq!(
            Some(&StreamValue {
                recordings: 2,
                duration: two_min
            }),
            m.get(test_day1)
        );
        assert_eq!(
            Some(&StreamValue {
                recordings: 2,
                duration: four_min
            }),
            m.get(test_day2)
        );

        // Subtract from two days.
        m.adjust(test_time..test_time + three_min, -1);
        assert_eq!(2, m.len());
        assert_eq!(
            Some(&StreamValue {
                recordings: 1,
                duration: one_min
            }),
            m.get(test_day1)
        );
        assert_eq!(
            Some(&StreamValue {
                recordings: 1,
                duration: two_min
            }),
            m.get(test_day2)
        );

        // Remove two days.
        m.adjust(test_time..test_time + three_min, -1);
        assert_eq!(0, m.len());
    }

    #[test]
    fn test_adjust_signal() {
        testutil::init();
        let mut m: Map<SignalValue> = Map::default();

        let test_time = Time(130646844000000i64); // 2015-12-31 23:00:00 (Pacific).
        let hr = Duration(60 * 60 * TIME_UNITS_PER_SEC);
        let test_day1 = &Key(*b"2015-12-31");
        let test_day2 = &Key(*b"2016-01-01");
        let test_day3 = &Key(*b"2016-01-02");
        m.adjust(test_time..test_time + hr * 30, 0, 3);
        assert_eq!(3, m.len());
        assert_eq!(
            m.get(test_day1),
            Some(&SignalValue {
                states: smallvec![0, 0, hr.0 as u64],
            })
        );
        assert_eq!(
            m.get(test_day2),
            Some(&SignalValue {
                states: smallvec![0, 0, 24 * hr.0 as u64],
            })
        );
        assert_eq!(
            m.get(test_day3),
            Some(&SignalValue {
                states: smallvec![0, 0, 5 * hr.0 as u64],
            })
        );

        m.adjust(Time(130647168000000)..Time(130654944000000), 3, 1); // entire 2016-01-01
        assert_eq!(3, m.len());
        assert_eq!(
            m.get(test_day1),
            Some(&SignalValue {
                states: smallvec![0, 0, hr.0 as u64],
            })
        );
        assert_eq!(
            m.get(test_day2),
            Some(&SignalValue {
                states: smallvec![24 * hr.0 as u64],
            })
        );
        assert_eq!(
            m.get(test_day3),
            Some(&SignalValue {
                states: smallvec![0, 0, 5 * hr.0 as u64],
            })
        );

        m.adjust(Time(130647168000000)..Time(130654944000000), 1, 0); // entire 2016-01-01
        assert_eq!(2, m.len());
        assert_eq!(
            m.get(test_day1),
            Some(&SignalValue {
                states: smallvec![0, 0, hr.0 as u64],
            })
        );
        assert_eq!(
            m.get(test_day3),
            Some(&SignalValue {
                states: smallvec![0, 0, 5 * hr.0 as u64],
            })
        );
    }

    #[test]
    fn test_day_bounds() {
        testutil::init();
        assert_eq!(
            Key(*b"2017-10-10").bounds(), // normal day (24 hrs)
            Time(135685692000000)..Time(135693468000000)
        );
        assert_eq!(
            Key(*b"2017-03-12").bounds(), // spring forward (23 hrs)
            Time(134037504000000)..Time(134044956000000)
        );
        assert_eq!(
            Key(*b"2017-11-05").bounds(), // fall back (25 hrs)
            Time(135887868000000)..Time(135895968000000)
        );
    }
}
