// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Tools for propagating a graceful shutdown signal through the program.
//!
//! The receiver can be cloned, checked and used as a future in async code.
//! Also, for convenience, blocked in synchronous code without going through the
//! runtime.
//!
//! Surprisingly, I couldn't find any simple existing mechanism for anything
//! close to this in `futures::channels` or `tokio::sync`.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use crate::Condvar;
use crate::Mutex;
use futures::Future;
use slab::Slab;

#[derive(Debug)]
pub struct ShutdownError;

impl std::fmt::Display for ShutdownError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("shutdown requested")
    }
}

impl std::error::Error for ShutdownError {}

struct Inner {
    /// `None` iff shutdown has already happened.
    wakers: Mutex<Option<Slab<Waker>>>,

    condvar: Condvar,
}

pub struct Sender(Arc<Inner>);

impl Drop for Sender {
    fn drop(&mut self) {
        // Note sequencing: modify the lock state, then notify async/sync waiters.
        // The opposite order would create a race in which something might never wake.
        let mut wakers = self
            .0
            .wakers
            .lock()
            .take()
            .expect("only the single Sender takes the slab");
        for w in wakers.drain() {
            w.wake();
        }
        self.0.condvar.notify_all();
    }
}

#[derive(Clone)]
pub struct Receiver(Arc<Inner>);

pub struct ReceiverRefFuture<'receiver> {
    receiver: &'receiver Receiver,
    waker_i: usize,
}

pub struct ReceiverFuture {
    receiver: Arc<Inner>,
    waker_i: usize,
}

/// `waker_i` value to indicate no slot has been assigned.
///
/// There can't be `usize::MAX` items in the slab because there are other things
/// in the address space (and because `Waker` uses more than one byte anyway).
const NO_WAKER: usize = usize::MAX;

impl Receiver {
    pub fn check(&self) -> Result<(), ShutdownError> {
        if self.0.wakers.lock().is_none() {
            Err(ShutdownError)
        } else {
            Ok(())
        }
    }

    pub fn as_future(&self) -> ReceiverRefFuture<'_> {
        ReceiverRefFuture {
            receiver: self,
            waker_i: NO_WAKER,
        }
    }

    pub fn future(&self) -> ReceiverFuture {
        ReceiverFuture {
            receiver: self.0.clone(),
            waker_i: NO_WAKER,
        }
    }

    pub fn into_future(self) -> ReceiverFuture {
        ReceiverFuture {
            receiver: self.0,
            waker_i: NO_WAKER,
        }
    }

    pub fn wait_for(&self, timeout: std::time::Duration) -> Result<(), ShutdownError> {
        let l = self.0.wakers.lock();
        let result = self
            .0
            .condvar
            .wait_timeout_while(l, timeout, |wakers| wakers.is_some());
        if result.1.timed_out() {
            Ok(())
        } else {
            Err(ShutdownError)
        }
    }
}

fn poll_impl(inner: &Inner, waker_i: &mut usize, cx: &mut Context<'_>) -> Poll<()> {
    let mut l = inner.wakers.lock();
    let wakers = match &mut *l {
        None => return Poll::Ready(()),
        Some(w) => w,
    };
    let new_waker = cx.waker();
    if *waker_i == NO_WAKER {
        *waker_i = wakers.insert(new_waker.clone());
    } else {
        let existing_waker = &mut wakers[*waker_i];
        if !new_waker.will_wake(existing_waker) {
            existing_waker.clone_from(new_waker);
        }
    }
    Poll::Pending
}

impl Future for ReceiverRefFuture<'_> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        poll_impl(&self.receiver.0, &mut self.waker_i, cx)
    }
}

impl Drop for ReceiverRefFuture<'_> {
    fn drop(&mut self) {
        if self.waker_i == NO_WAKER {
            return;
        }
        let mut l = self.receiver.0.wakers.lock();
        if let Some(wakers) = &mut *l {
            wakers.remove(self.waker_i);
        }
    }
}

impl Future for ReceiverFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = Pin::into_inner(self);
        poll_impl(&this.receiver, &mut this.waker_i, cx)
    }
}

impl Drop for ReceiverFuture {
    fn drop(&mut self) {
        if self.waker_i == NO_WAKER {
            return;
        }
        let mut l = self.receiver.wakers.lock();
        if let Some(wakers) = &mut *l {
            wakers.remove(self.waker_i);
        }
    }
}

/// Returns a sender and receiver for graceful shutdown.
///
/// Dropping the sender will request shutdown.
///
/// The receiver can be used as a future or just polled when convenient.
pub fn channel() -> (Sender, Receiver) {
    let inner = Arc::new(Inner {
        wakers: Mutex::new(Some(Slab::new())),
        condvar: Condvar::new(),
    });
    (Sender(inner.clone()), Receiver(inner))
}

#[cfg(test)]
mod tests {
    use futures::Future;
    use std::task::{Context, Poll};

    #[test]
    fn simple_check() {
        let (tx, rx) = super::channel();
        rx.check().unwrap();
        drop(tx);
        rx.check().unwrap_err();
    }

    #[test]
    fn blocking() {
        let (tx, rx) = super::channel();
        rx.wait_for(std::time::Duration::from_secs(0)).unwrap();
        let h = std::thread::spawn(move || {
            rx.wait_for(std::time::Duration::from_secs(1000))
                .unwrap_err()
        });

        // Make it likely that rx has done its initial check and is waiting on the Condvar.
        std::thread::sleep(std::time::Duration::from_millis(10));

        drop(tx);
        h.join().unwrap();
    }

    #[test]
    fn future() {
        let (tx, rx) = super::channel();
        let waker = futures::task::noop_waker_ref();
        let mut cx = Context::from_waker(waker);
        let mut f = rx.as_future();
        assert_eq!(std::pin::Pin::new(&mut f).poll(&mut cx), Poll::Pending);
        drop(tx);
        assert_eq!(std::pin::Pin::new(&mut f).poll(&mut cx), Poll::Ready(()));
        // TODO: this doesn't actually check that waker is even used.
    }
}
