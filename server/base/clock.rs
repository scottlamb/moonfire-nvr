// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Clock interface and implementations for testability.
//!
//! Note these types are in a more standard nanosecond-based format, where
//! [`crate::time`] uses Moonfire's 90 kHz time base.

use crate::Mutex;
use nix::sys::time::{TimeSpec, TimeValLike as _};
use std::sync::{mpsc, Arc};
use std::thread;
pub use std::time::Duration;
use tracing::warn;

use crate::error::Error;
use crate::shutdown::ShutdownError;

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct SystemTime(pub TimeSpec);

impl SystemTime {
    pub fn new(sec: u64, nsec: i64) -> Self {
        // `TimeSpec::new`'s arguments vary by platform.
        // * currently uses 32-bit time_t on musl <https://github.com/rust-lang/libc/issues/1848>
        // * nsec likewise can vary.
        SystemTime(TimeSpec::new(sec as _, nsec as _))
    }

    pub fn as_secs(&self) -> i64 {
        self.0.num_seconds()
    }
}

impl std::ops::Add<Duration> for SystemTime {
    type Output = SystemTime;

    fn add(self, rhs: Duration) -> SystemTime {
        SystemTime(self.0 + TimeSpec::from(rhs))
    }
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant(pub TimeSpec);

impl Instant {
    pub fn from_secs(secs: i64) -> Self {
        Instant(TimeSpec::seconds(secs))
    }

    pub fn saturating_sub(&self, o: &Instant) -> Duration {
        if o > self {
            Duration::default()
        } else {
            Duration::from(self.0 - o.0)
        }
    }
}

impl std::fmt::Debug for Instant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// TODO: should use saturating always?
impl std::ops::Sub<Instant> for Instant {
    type Output = Duration;

    fn sub(self, rhs: Instant) -> Duration {
        Duration::from(self.0 - rhs.0)
    }
}

impl std::ops::Add<Duration> for Instant {
    type Output = Instant;

    fn add(self, rhs: Duration) -> Instant {
        Instant(self.0 + TimeSpec::from(rhs))
    }
}

/// Abstract interface to the system clocks. This is for testability.
pub trait Clocks: Send + Sync + 'static {
    /// Gets the current time from `CLOCK_REALTIME`.
    fn realtime(&self) -> SystemTime;

    /// Gets the current time from a monotonic clock.
    ///
    /// On Linux, this uses `CLOCK_BOOTTIME`, which includes suspended time.
    /// On other systems, it uses `CLOCK_MONOTONIC`.
    fn monotonic(&self) -> Instant;

    /// Causes the current thread to sleep for the specified time.
    fn sleep(&self, how_long: Duration);

    /// Calls `rcv.recv_timeout` or substitutes a test implementation.
    fn recv_timeout<T>(
        &self,
        rcv: &mpsc::Receiver<T>,
        timeout: Duration,
    ) -> Result<T, mpsc::RecvTimeoutError>;
}

pub fn retry<C, T, E>(
    clocks: &C,
    shutdown_rx: &crate::shutdown::Receiver,
    f: &mut dyn FnMut() -> Result<T, E>,
) -> Result<T, ShutdownError>
where
    C: Clocks,
    E: Into<Error>,
{
    loop {
        let e = match f() {
            Ok(t) => return Ok(t),
            Err(e) => e.into(),
        };
        shutdown_rx.check()?;
        let sleep_time = Duration::from_secs(1);
        warn!(
            exception = %e.chain(),
            "sleeping for 1 s after error"
        );
        clocks.sleep(sleep_time);
    }
}

#[derive(Copy, Clone)]
pub struct RealClocks {}

impl Clocks for RealClocks {
    fn realtime(&self) -> SystemTime {
        SystemTime(
            nix::time::clock_gettime(nix::time::ClockId::CLOCK_REALTIME)
                .expect("clock_gettime(REALTIME) should succeed"),
        )
    }

    #[cfg(target_os = "linux")]
    fn monotonic(&self) -> Instant {
        Instant(
            nix::time::clock_gettime(nix::time::ClockId::CLOCK_BOOTTIME)
                .expect("clock_gettime(BOOTTIME) should succeed"),
        )
    }

    #[cfg(not(target_os = "linux"))]
    fn monotonic(&self) -> Instant {
        Instant(
            nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC)
                .expect("clock_gettime(MONOTONIC) should succeed"),
        )
    }

    fn sleep(&self, how_long: Duration) {
        thread::sleep(how_long)
    }

    fn recv_timeout<T>(
        &self,
        rcv: &mpsc::Receiver<T>,
        timeout: Duration,
    ) -> Result<T, mpsc::RecvTimeoutError> {
        rcv.recv_timeout(timeout)
    }
}

/// Logs a warning if the TimerGuard lives "too long", using the label created by a supplied
/// function.
pub struct TimerGuard<'a, C: Clocks + ?Sized, S: AsRef<str>, F: FnOnce() -> S + 'a> {
    clocks: &'a C,
    label_f: Option<F>,
    start: Instant,
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
where
    C: Clocks + ?Sized,
    S: AsRef<str>,
    F: FnOnce() -> S + 'a,
{
    fn drop(&mut self) {
        let elapsed = self.clocks.monotonic() - self.start;
        if elapsed.as_secs() >= 1 {
            let label_f = self.label_f.take().unwrap();
            warn!("{} took {:?}!", label_f().as_ref(), elapsed);
        }
    }
}

/// Simulated clock for testing.
#[derive(Clone)]
pub struct SimulatedClocks(Arc<SimulatedClocksInner>);

struct SimulatedClocksInner {
    boot: SystemTime,
    uptime: Mutex<Duration>,
}

impl SimulatedClocks {
    pub fn new(boot: SystemTime) -> Self {
        SimulatedClocks(Arc::new(SimulatedClocksInner {
            boot,
            uptime: Mutex::new(Duration::from_secs(0)),
        }))
    }
}

impl Clocks for SimulatedClocks {
    fn realtime(&self) -> SystemTime {
        self.0.boot + *self.0.uptime.lock()
    }
    fn monotonic(&self) -> Instant {
        Instant(TimeSpec::from(*self.0.uptime.lock()))
    }

    /// Advances the clock by the specified amount without actually sleeping.
    fn sleep(&self, how_long: Duration) {
        let mut l = self.0.uptime.lock();
        *l += how_long;
    }

    /// Advances the clock by the specified amount if data is not immediately available.
    fn recv_timeout<T>(
        &self,
        rcv: &mpsc::Receiver<T>,
        timeout: Duration,
    ) -> Result<T, mpsc::RecvTimeoutError> {
        let r = rcv.recv_timeout(Duration::new(0, 0));
        if r.is_err() {
            self.sleep(timeout);
        }
        r
    }
}
