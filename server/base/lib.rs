// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

pub mod clock;
mod error;
pub mod strutil;
pub mod time;

pub use crate::error::{prettify_failure, Error, ErrorKind, ResultExt};
