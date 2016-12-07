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

//! Clock interface and implementations for testability.

#[cfg(test)] use std::sync::Mutex;
use std::thread;
use time;

/// Abstract interface to the system clock. This is for testability.
pub trait Clock : Sync {
    /// Gets the current time.
    fn get_time(&self) -> time::Timespec;

    /// Causes the current thread to sleep for the specified time.
    fn sleep(&self, how_long: time::Duration);
}

/// Singleton "real" clock.
pub static REAL: RealClock = RealClock {};

/// Real clock; see static `REAL` instance.
pub struct RealClock {}

impl Clock for RealClock {
    fn get_time(&self) -> time::Timespec { time::get_time() }

    fn sleep(&self, how_long: time::Duration) {
        match how_long.to_std() {
            Ok(d) => thread::sleep(d),
            Err(e) => warn!("Invalid duration {:?}: {}", how_long, e),
        };
    }
}

/// Simulated clock for testing.
#[cfg(test)]
pub struct SimulatedClock(Mutex<time::Timespec>);

#[cfg(test)]
impl SimulatedClock {
    pub fn new() -> SimulatedClock { SimulatedClock(Mutex::new(time::Timespec::new(0, 0))) }
}

#[cfg(test)]
impl Clock for SimulatedClock {
    fn get_time(&self) -> time::Timespec { *self.0.lock().unwrap() }

    /// Advances the clock by the specified amount without actually sleeping.
    fn sleep(&self, how_long: time::Duration) {
        let mut l = self.0.lock().unwrap();
        *l = *l + how_long;
    }
}
