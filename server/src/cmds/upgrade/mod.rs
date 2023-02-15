// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use bpaf::Bpaf;
/// Upgrades the database schema.
///
/// See `guide/schema.md` for more information.
use failure::Error;

/// Upgrades to the latest database schema.
#[derive(Bpaf, Debug)]
#[bpaf(options)]
pub struct Args {
    #[bpaf(external(crate::parse_db_dir))]
    db_dir: std::path::PathBuf,

    /// When upgrading from schema version 1 to 2, the sample file directory.
    #[bpaf(argument("PATH"))]
    sample_file_dir: Option<std::path::PathBuf>,

    /// Resets the SQLite journal_mode to the specified mode prior to
    /// the upgrade.
    ///
    ///
    /// default: `delete` (recommended). `off` is very dangerous but may be
    /// desirable in some circumstances. See `guide/schema.md` for more
    /// information. The journal mode will be reset to `wal` after the upgrade.
    #[bpaf(argument("MODE"), fallback_with(|| Ok::<_, std::convert::Infallible>("delete".into())))]
    preset_journal: String,

    /// Skips the normal post-upgrade vacuum operation.
    no_vacuum: bool,
}

pub fn subcommand() -> impl bpaf::Parser<Args> {
    crate::subcommand(args(), "upgrade")
}

pub fn run(args: Args) -> Result<i32, Error> {
    let (_db_dir, mut conn) = super::open_conn(&args.db_dir, super::OpenMode::ReadWrite)?;

    db::upgrade::run(
        &db::upgrade::Args {
            sample_file_dir: args.sample_file_dir.as_deref(),
            preset_journal: &args.preset_journal,
            no_vacuum: args.no_vacuum,
        },
        &mut conn,
    )?;
    Ok(0)
}
