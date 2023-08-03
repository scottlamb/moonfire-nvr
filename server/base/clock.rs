// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Clock interface and implementations for testability.

use std::mem;
use std::sync::Mutex;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration as StdDuration;
use time::{Duration, Timespec};
use tracing::warn;

use crate::error::Error;
use crate::shutdown::ShutdownError;

/// Abstract interface to the system clocks. This is for testability.
pub trait Clocks: Send + Sync + 'static {
    /// Gets the current time from `CLOCK_REALTIME`.
    fn realtime(&self) -> Timespec;

    /// Gets the current time from a monotonic clock.
    ///
    /// On Linux, this uses `CLOCK_BOOTTIME`, which includes suspended time.
    /// On other systems, it uses `CLOCK_MONOTONIC`.
    fn monotonic(&self) -> Timespec;

    /// Causes the current thread to sleep for the specified time.
    fn sleep(&self, how_long: Duration);

    /// Calls `rcv.recv_timeout` or substitutes a test implementation.
    fn recv_timeout<T>(
        &self,
        rcv: &mpsc::Receiver<T>,
        timeout: StdDuration,
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
        let sleep_time = Duration::seconds(1);
        warn!(
            exception = %e.chain(),
            "sleeping for 1 s after error"
        );
        clocks.sleep(sleep_time);
    }
}

#[derive(Copy, Clone)]
pub struct RealClocks {}

impl RealClocks {
    fn get(&self, clock: libc::clockid_t) -> Timespec {
        unsafe {
            let mut ts = mem::MaybeUninit::uninit();
            assert_eq!(0, libc::clock_gettime(clock, ts.as_mut_ptr()));
            let ts = ts.assume_init();
            Timespec::new(
                // On 32-bit arm builds, `tv_sec` is an `i32` and requires conversion.
                // On other platforms, the `.into()` is a no-op.
                #[allow(clippy::useless_conversion)]
                ts.tv_sec.into(),
                ts.tv_nsec as i32,
            )
        }
    }
}

impl Clocks for RealClocks {
    fn realtime(&self) -> Timespec {
        self.get(libc::CLOCK_REALTIME)
    }

    #[cfg(target_os = "linux")]
    fn monotonic(&self) -> Timespec {
        self.get(libc::CLOCK_BOOTTIME)
    }

    #[cfg(not(target_os = "linux"))]
    fn monotonic(&self) -> Timespec {
        self.get(libc::CLOCK_MONOTONIC)
    }

    fn sleep(&self, how_long: Duration) {
        match how_long.to_std() {
            Ok(d) => thread::sleep(d),
            Err(err) => warn!(%err, "invalid duration {:?}", how_long),
        };
    }

    fn recv_timeout<T>(
        &self,
        rcv: &mpsc::Receiver<T>,
        timeout: StdDuration,
    ) -> Result<T, mpsc::RecvTimeoutError> {
        rcv.recv_timeout(timeout)
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
where
    C: Clocks + ?Sized,
    S: AsRef<str>,
    F: FnOnce() -> S + 'a,
{
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
            boot,
            uptime: Mutex::new(Duration::seconds(0)),
        }))
    }
}

impl Clocks for SimulatedClocks {
    fn realtime(&self) -> Timespec {
        self.0.boot + *self.0.uptime.lock().unwrap()
    }
    fn monotonic(&self) -> Timespec {
        Timespec::new(0, 0) + *self.0.uptime.lock().unwrap()
    }

    /// Advances the clock by the specified amount without actually sleeping.
    fn sleep(&self, how_long: Duration) {
        let mut l = self.0.uptime.lock().unwrap();
        *l = *l + how_long;
    }

    /// Advances the clock by the specified amount if data is not immediately available.
    fn recv_timeout<T>(
        &self,
        rcv: &mpsc::Receiver<T>,
        timeout: StdDuration,
    ) -> Result<T, mpsc::RecvTimeoutError> {
        let r = rcv.recv_timeout(StdDuration::new(0, 0));
        if r.is_err() {
            self.sleep(Duration::from_std(timeout).unwrap());
        }
        r
    }
}
