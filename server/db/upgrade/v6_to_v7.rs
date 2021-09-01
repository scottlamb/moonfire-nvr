// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

/// Upgrades a version 6 schema to a version 7 schema.
use failure::Error;

pub fn run(_args: &super::Args, _tx: &rusqlite::Transaction) -> Result<(), Error> {
    Ok(())
}
