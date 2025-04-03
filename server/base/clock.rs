// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Clock interface and implementations for testability.
//!
//! Note these types are in a more standard nanosecond-based format, where
//! [`crate::time`] uses Moonfire's 90 kHz time base.

use crate::Mutex;
use nix::sys::time::{TimeSpec, TimeValLike as _};
use std::future::Future;
use std::panic::Location;
use std::sync::Arc;
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
pub trait Clocks: Clone + Send + Sync + 'static {
    /// Gets the current time from `CLOCK_REALTIME`.
    fn realtime(&self) -> SystemTime;

    /// Gets the current time from a monotonic clock.
    ///
    /// On Linux, this uses `CLOCK_BOOTTIME`, which includes suspended time.
    /// On other systems, it uses `CLOCK_MONOTONIC`.
    fn monotonic(&self) -> Instant;

    /// Causes the current thread to sleep for the specified time.
    fn sleep(&self, how_long: Duration) -> impl Future<Output = ()> + Send;
}

/// Waits a bit before retrying an operation.
///
/// Use as follows (for an operation that returns `()` on success):
///
/// ```no_compile
/// while let Err(e) = fallible_operation().await {
///    retry_wait(clocks, shutdown_rx, e).await?;
/// }
/// ```
///
/// or (for an operation that returns a value on success):
///
/// ```no_compile
/// let result = loop {
///     match fallible_operation().await {
///         Ok(result) => break result,
///         Err(e) => retry_wait(clocks, shutdown_rx, e).await?,
///     }
/// };
/// ```
///
/// # Interface note
///
/// The synchronous version of this method took a lambda and encompassed the
/// retry loop, as follows:
///
/// ```no_compile
/// fn retry<C: Clocks, E: Into<Error>>(
///     clocks: &C,
///     shutdown_rx: &crate::shutdown::Receiver,
///     f: &mut FnMut() -> Result<T, E>,,
/// ) -> Result<T, ShutdownError> { todo!() }
/// ```
///
/// Unfortunately it does not seem trivial to match this with async as of
/// Rust 1.85. The following attempt with async closures is close, but
/// critically the `F(..): Send + 'a` bound is not yet supported. Alternatively,
/// we could use a `FnMut -> Future<Output = Result<T, E>>` approach, but it's
/// not possible to allow the future to borrow from the `FnMut`.
///
/// ```no_compile
/// pub fn retry<'a, C, T, E, F>(
///     clocks: &'a C,
///     shutdown_rx: &'a crate::shutdown::Receiver,
///     mut f: F,
/// ) -> impl Future<Output = Result<T, ShutdownError>> + Send + 'a
/// where
///     C: Clocks,
///     E: Into<Error>,
///     F: AsyncFnMut() -> Result<T, E> + Send + 'a,
///     F(..): Send + 'a,
/// { todo!() }
/// ```
///
/// Alternatively, we could use a `FnMut -> Future<Output = Result<T, E>>`
/// approach, but it's not possible to allow the future to borrow from the
/// `FnMut`.
pub async fn retry_wait<C: Clocks>(
    clocks: &C,
    shutdown_rx: &crate::shutdown::Receiver,
    e: Error,
) -> Result<(), ShutdownError> {
    warn!(
        exception = %e.chain(),
        "sleeping for 1 s after error"
    );
    tokio::select! {
        biased;
        _ = shutdown_rx.as_future() => Err(ShutdownError),
        _ = clocks.sleep(Duration::from_secs(1)) => Ok(()),
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

    fn sleep(&self, how_long: Duration) -> impl Future<Output = ()> + Send {
        tokio::time::sleep(how_long)
    }
}

/// Logs a warning if the TimerGuard lives "too long", using the label created by a supplied
/// function.
pub struct TimerGuard<'a, C: Clocks, S: AsRef<str>, F: FnOnce(&'static Location<'static>) -> S + 'a>
{
    clocks: &'a C,
    location: &'static Location<'static>,
    label_f: Option<F>,
    start: Instant,
}

impl<'a, C: Clocks, S: AsRef<str>, F: FnOnce(&'static Location<'static>) -> S + 'a>
    TimerGuard<'a, C, S, F>
{
    #[track_caller]
    pub fn new(clocks: &'a C, label_f: F) -> Self {
        TimerGuard {
            clocks,
            location: Location::caller(),
            label_f: Some(label_f),
            start: clocks.monotonic(),
        }
    }
}

impl<'a, C, S, F> Drop for TimerGuard<'a, C, S, F>
where
    C: Clocks,
    S: AsRef<str>,
    F: FnOnce(&'static Location<'static>) -> S + 'a,
{
    fn drop(&mut self) {
        let elapsed = self.clocks.monotonic() - self.start;
        if elapsed.as_secs() >= 1 {
            let label_f = self.label_f.take().unwrap();
            warn!("{} took {:?}!", label_f(self.location).as_ref(), elapsed);
        }
    }
}

/// Simulated clock for testing.
#[derive(Clone)]
pub struct SimulatedClocks(Arc<SimulatedClocksInner>);

struct SimulatedClocksInner {
    boot: SystemTime,
    uptime: Mutex<Duration, 3>,
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
    async fn sleep(&self, how_long: Duration) {
        let mut l = self.0.uptime.lock();
        *l += how_long;
    }
}
