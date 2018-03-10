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

//! Clock interface and implementations for testability.

use failure::Error;
use libc;
use parking_lot::Mutex;
use std::mem;
use std::sync::Arc;
use std::thread;
use time::{Duration, Timespec};

/// Abstract interface to the system clocks. This is for testability.
pub trait Clocks : Send + Sync + 'static {
    /// Gets the current time from `CLOCK_REALTIME`.
    fn realtime(&self) -> Timespec;

    /// Gets the current time from `CLOCK_MONOTONIC`.
    fn monotonic(&self) -> Timespec;

    /// Causes the current thread to sleep for the specified time.
    fn sleep(&self, how_long: Duration);
}

pub fn retry_forever<T, E: Into<Error>>(clocks: &Clocks, f: &mut FnMut() -> Result<T, E>) -> T {
    loop {
        let e = match f() {
            Ok(t) => return t,
            Err(e) => e.into(),
        };
        let sleep_time = Duration::seconds(1);
        warn!("sleeping for {:?} after error: {:?}", sleep_time, e);
        clocks.sleep(sleep_time);
    }
}

#[derive(Clone)]
pub struct RealClocks {}

impl RealClocks {
    fn get(&self, clock: libc::clockid_t) -> Timespec {
        unsafe {
            let mut ts = mem::uninitialized();
            assert_eq!(0, libc::clock_gettime(clock, &mut ts));
            Timespec::new(ts.tv_sec as i64, ts.tv_nsec as i32)
        }
    }
}

impl Clocks for RealClocks {
    fn realtime(&self) -> Timespec { self.get(libc::CLOCK_REALTIME) }
    fn monotonic(&self) -> Timespec { self.get(libc::CLOCK_MONOTONIC) }

    fn sleep(&self, how_long: Duration) {
        match how_long.to_std() {
            Ok(d) => thread::sleep(d),
            Err(e) => warn!("Invalid duration {:?}: {}", how_long, e),
        };
    }
}

/// Logs a warning if the TimerGuard lives "too long", using the label created by a supplied
/// function.
pub struct TimerGuard<'a, C: Clocks + ?Sized, S: AsRef<str>, F: FnOnce() -> S + 'a> {
    clocks: &'a C,
    label_f: Option<F>,
    start: Timespec,
}

impl<'a, C: Clocks + ?Sized, S: AsRef<str>, F: FnOnce() -> S + 'a> TimerGuard<'a, C, S, F> {
    pub fn new(clocks: &'a C, label_f: F) -> Self {
        TimerGuard {
            clocks,
            label_f: Some(label_f),
            start: clocks.monotonic(),
        }
    }
}

impl<'a, C, S, F> Drop for TimerGuard<'a, C, S, F>
where C: Clocks + ?Sized, S: AsRef<str>, F: FnOnce() -> S + 'a {
    fn drop(&mut self) {
        let elapsed = self.clocks.monotonic() - self.start;
        if elapsed.num_seconds() >= 1 {
            let label_f = self.label_f.take().unwrap();
            warn!("{} took {}!", label_f().as_ref(), elapsed);
        }
    }
}

/// Simulated clock for testing.
#[derive(Clone)]
pub struct SimulatedClocks(Arc<SimulatedClocksInner>);

struct SimulatedClocksInner {
    boot: Timespec,
    uptime: Mutex<Duration>,
}

impl SimulatedClocks {
    pub fn new(boot: Timespec) -> Self {
        SimulatedClocks(Arc::new(SimulatedClocksInner {
            boot: boot,
            uptime: Mutex::new(Duration::seconds(0)),
        }))
    }
}

impl Clocks for SimulatedClocks {
    fn realtime(&self) -> Timespec { self.0.boot + *self.0.uptime.lock() }
    fn monotonic(&self) -> Timespec { Timespec::new(0, 0) + *self.0.uptime.lock() }

    /// Advances the clock by the specified amount without actually sleeping.
    fn sleep(&self, how_long: Duration) {
        let mut l = self.0.uptime.lock();
        *l = *l + how_long;
    }
}
