// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

/// Upgrades the database schema.
///
/// See `guide/schema.md` for more information.
use failure::Error;
use structopt::StructOpt;

#[derive(StructOpt)]
pub struct Args {
    #[structopt(
        long,
        help = "Directory holding the SQLite3 index database.",
        default_value = "/var/lib/moonfire-nvr/db",
        parse(from_os_str)
    )]
    db_dir: std::path::PathBuf,

    #[structopt(
        help = "When upgrading from schema version 1 to 2, the sample file directory.",
        long,
        parse(from_os_str)
    )]
    sample_file_dir: Option<std::path::PathBuf>,

    #[structopt(
        help = "Resets the SQLite journal_mode to the specified mode prior to \
               the upgrade. The default, delete, is recommended. off is very \
               dangerous but may be desirable in some circumstances. See \
               guide/schema.md for more information. The journal mode will be \
               reset to wal after the upgrade.",
        long,
        default_value = "delete"
    )]
    preset_journal: String,

    #[structopt(help = "Skips the normal post-upgrade vacuum operation.", long)]
    no_vacuum: bool,
}

pub fn run(args: &Args) -> Result<i32, Error> {
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
