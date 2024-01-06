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
