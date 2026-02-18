// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

pub mod clock;
pub mod error;
pub mod shutdown;
pub mod strutil;
pub mod time;
pub mod tracing_setup;

use std::mem::ManuallyDrop;

pub use crate::error::{Error, ErrorBuilder, ErrorKind, ResultExt};

pub use ahash::RandomState;
pub type FastHashMap<K, V> = std::collections::HashMap<K, V, ahash::RandomState>;
pub type FastHashSet<K> = std::collections::HashSet<K, ahash::RandomState>;

const NOT_POISONED: &str =
    "mutex should be unpoisoned; this is a consequence of an earlier panic while guarded; see logs.";

const GUARD_SOME: &str = "MutexGuard should always be Some except during Condition::wait_* calls";

#[cfg(debug_assertions)]
#[derive(Copy, Clone, PartialEq, Eq)]
struct Hold {
    location: &'static std::panic::Location<'static>,
    order: usize,
}

#[cfg(debug_assertions)]
thread_local! {
    static CUR_HOLD: std::cell::Cell<Option<Hold>> = const { std::cell::Cell::new(None) };
}

/// [`std::sync::Mutex`] wrapper.
///
/// * Always panics on encountering poison with a decent error message, rather than
///   requiring the caller unwrap the result itself.
/// * In debug mode, enforces a deadlock-free lock ordering, e.g. stream mutexes can't
///   be held when acquiring the database mutex. This detects potential deadlocks slightly
///   more reliably, and with a clearer error message.
///
/// `ORDER` should be > 0; a thread may only acquire locks in ascending order.
#[derive(Default)]
pub struct Mutex<T, const ORDER: usize>(std::sync::Mutex<T>);

/// A holder of a mutex or an antilock.
struct Holder {
    #[cfg(debug_assertions)]
    prev: Option<Hold>,
    #[cfg(debug_assertions)]
    this: Hold,
}

impl Holder {
    #[inline]
    #[track_caller]
    fn new(order: usize) -> Self {
        #[cfg(not(debug_assertions))]
        let _ = order;

        #[cfg(debug_assertions)]
        let location = std::panic::Location::caller();
        Self {
            #[cfg(debug_assertions)]
            prev: {
                let prev = CUR_HOLD.replace(Some(Hold { location, order }));
                if let Some(prev) = prev {
                    if prev.order > order && !std::thread::panicking() {
                        panic!(
                            "order-{order} holder at {location}: acquired while holding\n\
                            order-{prev_order} holder at {prev_location}",
                            prev_order = prev.order,
                            prev_location = prev.location,
                        );
                    }
                }
                prev
            },
            #[cfg(debug_assertions)]
            this: Hold { location, order },
        }
    }
}

#[cfg(debug_assertions)]
impl Drop for Holder {
    fn drop(&mut self) {
        let latest = CUR_HOLD.replace(self.prev);
        if std::thread::panicking() {
            return;
        }
        if let Some(latest) = latest {
            if latest != self.this {
                panic!(
                    "released holder with order {this_order}, acquire location {this_location}\n\
                    but last acquired holder had order {latest_order}, acquire location {latest_location}",
                    this_order=self.this.order,
                    this_location=self.this.location,
                    latest_order=latest.order,
                    latest_location=latest.location,
                );
            }
        } else {
            panic!(
                "released holder with order {this_order}, acquire_location {this_location}\n\
                but no last acquired holder",
                this_order = self.this.order,
                this_location = self.this.location,
            );
        }
    }
}

impl<T, const ORDER: usize> Mutex<T, ORDER> {
    #[inline]
    pub const fn new(value: T) -> Self {
        Mutex(std::sync::Mutex::new(value))
    }

    #[inline]
    #[track_caller]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        MutexGuard {
            _holder: Holder::new(ORDER),
            inner: Some(self.0.lock().expect(NOT_POISONED)),
        }
    }

    #[inline]
    #[track_caller]
    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut().expect(NOT_POISONED)
    }

    #[inline]
    #[track_caller]
    pub fn into_inner(self) -> T {
        self.0.into_inner().expect(NOT_POISONED)
    }
}

pub struct MutexGuard<'t, T> {
    _holder: Holder,
    /// This should always be `Some` except during a `Condvar::wait_timeout_while` call.
    inner: Option<std::sync::MutexGuard<'t, T>>,
}

impl<T> std::ops::Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().expect(GUARD_SOME).deref()
    }
}

impl<T> std::ops::DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().expect(GUARD_SOME).deref_mut()
    }
}

/// [`std::sync::Condvar`] wrapper which always panics on encountering poison.
#[derive(Default)]
pub struct Condvar(std::sync::Condvar);

impl Condvar {
    #[inline]
    pub const fn new() -> Self {
        Self(std::sync::Condvar::new())
    }

    #[track_caller]
    #[inline]
    pub fn wait<'a, T>(&self, mut guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        let inner = self
            .0
            .wait(guard.inner.take().expect(GUARD_SOME))
            .expect(NOT_POISONED);
        guard.inner = Some(inner);
        guard
    }

    #[track_caller]
    #[inline]
    pub fn wait_timeout_while<'a, T, F>(
        &self,
        mut guard: MutexGuard<'a, T>,
        dur: std::time::Duration,
        condition: F,
    ) -> (MutexGuard<'a, T>, std::sync::WaitTimeoutResult)
    where
        F: FnMut(&mut T) -> bool,
    {
        let (inner, res) = self
            .0
            .wait_timeout_while(guard.inner.take().expect(GUARD_SOME), dur, condition)
            .expect(NOT_POISONED);
        guard.inner = Some(inner);
        (guard, res)
    }
}

impl std::ops::Deref for Condvar {
    type Target = std::sync::Condvar;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Ensures that the enclosed variable is not used while a lock above the given order is held.
#[derive(Default)]
pub struct Antilock<const ORDER: usize, T>(ManuallyDrop<T>);

impl<const ORDER: usize, T> Antilock<ORDER, T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(ManuallyDrop::new(value))
    }

    #[inline]
    pub fn borrow(&self) -> AntilockGuard<'_, T> {
        AntilockGuard {
            _holder: Holder::new(ORDER),
            inner: &self.0,
        }
    }

    #[inline]
    pub fn borrow_mut(&mut self) -> AntilockGuardMut<'_, T> {
        AntilockGuardMut {
            _holder: Holder::new(ORDER),
            inner: &mut self.0,
        }
    }

    #[inline]
    pub fn into_inner(mut self) -> T {
        assert_no_lock(ORDER);
        unsafe {
            // SAFETY: `ManuallyDrop` hasn't been dropped yet, and `forget` prevents double-use.
            let inner = ManuallyDrop::take(&mut self.0);
            std::mem::forget(self);
            inner
        }
    }
}

impl<const ORDER: usize, T> From<T> for Antilock<ORDER, T> {
    #[inline]
    fn from(value: T) -> Self {
        Self(ManuallyDrop::new(value))
    }
}

impl<const ORDER: usize, T> Drop for Antilock<ORDER, T> {
    #[inline]
    fn drop(&mut self) {
        if std::mem::needs_drop::<T>() {
            assert_no_lock(ORDER);
            unsafe {
                // SAFETY: `ManuallyDrop` hasn't been dropped yet.
                ManuallyDrop::drop(&mut self.0);
            }
        }
    }
}

#[track_caller]
fn assert_no_lock(order: usize) {
    #[cfg(debug_assertions)]
    if let Some(cur) = CUR_HOLD.get() {
        assert!(
            cur.order <= order,
            "accessed NoLockGuard<{order}> at {location} \
             while holding order-{cur_order} lock acquired at location {cur_location}",
            location = std::panic::Location::caller(),
            cur_order = cur.order,
            cur_location = cur.location,
        );
    }

    #[cfg(not(debug_assertions))]
    let _ = order;
}

pub struct AntilockGuard<'a, T> {
    _holder: Holder,
    inner: &'a T,
}

pub struct AntilockGuardMut<'a, T> {
    _holder: Holder,
    inner: &'a mut T,
}

impl<T> std::ops::Deref for AntilockGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.inner
    }
}

impl<T> std::ops::Deref for AntilockGuardMut<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.inner
    }
}

impl<T> std::ops::DerefMut for AntilockGuardMut<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
    }
}

pub fn ensure_malloc_used() {
    #[cfg(feature = "mimalloc")]
    {
        // This is a load-bearing debug line.
        // Building `libmimalloc-sys` with the `override` feature will override `malloc` and
        // `free` as used through the Rust global allocator, SQLite, and `libc`. But...`cargo`
        // doesn't seem to build `libmimalloc-sys` at all if it's not referenced from Rust code.
        tracing::debug!("mimalloc version {}", unsafe {
            libmimalloc_sys::mi_version()
        })
    }
}
