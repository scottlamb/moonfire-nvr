// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Moonfire NVR's persistence layer.
//!
//! This manages both the SQLite database and the sample file directory.
//! Everything dealing with either flows through this crate. It keeps in-memory
//! state both as indexes and to batch SQLite database transactions.
//!
//! The core recording design is described in `design/recording.md` and is
//! mostly in the `db` module.

#![cfg_attr(all(feature = "nightly", test), feature(test))]

pub mod auth;
pub mod check;
mod coding;
mod compare;
pub mod days;
pub mod db;
pub mod dir;
mod fs;
pub mod json;
mod proto {
    include!(concat!(env!("OUT_DIR"), "/mod.rs"));
}
mod raw;
pub mod recording;
use proto::schema;
pub mod signal;
pub mod upgrade;
pub mod writer;

// This is only for #[cfg(test)], but it's also used by the dependent crate, and it appears that
// #[cfg(test)] is not passed on to dependencies.
pub mod testutil;

pub use crate::db::*;
pub use crate::schema::Permissions;
pub use crate::signal::Signal;
