// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

pub mod clock;
pub mod error;
pub mod shutdown;
pub mod strutil;
pub mod time;
pub mod tracing_setup;

pub use crate::error::{Error, ErrorBuilder, ErrorKind, ResultExt};

pub use ahash::RandomState;
pub type FastHashMap<K, V> = std::collections::HashMap<K, V, ahash::RandomState>;
pub type FastHashSet<K> = std::collections::HashSet<K, ahash::RandomState>;

const NOT_POISONED: &str =
    "not poisoned; this is a consequence of an earlier panic while holding this mutex; see logs.";

/// [`std::sync::Mutex`] wrapper which always panics on encountering poison.
#[derive(Default)]
pub struct Mutex<T>(std::sync::Mutex<T>);

impl<T> Mutex<T> {
    #[inline]
    pub const fn new(value: T) -> Self {
        Mutex(std::sync::Mutex::new(value))
    }

    #[track_caller]
    #[inline]
    pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
        self.0.lock().expect(NOT_POISONED)
    }

    #[track_caller]
    #[inline]
    pub fn into_inner(self) -> T {
        self.0.into_inner().expect(NOT_POISONED)
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
    pub fn wait_timeout_while<'a, T, F>(
        &self,
        guard: std::sync::MutexGuard<'a, T>,
        dur: std::time::Duration,
        condition: F,
    ) -> (std::sync::MutexGuard<'a, T>, std::sync::WaitTimeoutResult)
    where
        F: FnMut(&mut T) -> bool,
    {
        self.0
            .wait_timeout_while(guard, dur, condition)
            .expect(NOT_POISONED)
    }
}

impl std::ops::Deref for Condvar {
    type Target = std::sync::Condvar;

    fn deref(&self) -> &Self::Target {
        &self.0
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
