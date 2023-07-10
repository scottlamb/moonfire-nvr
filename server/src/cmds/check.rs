// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Subcommand to check the database and sample file dir for errors.

use base::Error;
use bpaf::Bpaf;
use db::check;
use std::path::PathBuf;

/// Checks database integrity (like fsck).
#[derive(Bpaf, Debug)]
#[bpaf(command("check"))]
pub struct Args {
    #[bpaf(external(crate::parse_db_dir))]
    db_dir: PathBuf,

    /// Compares sample file lengths on disk to the database.
    compare_lens: bool,

    /// Trashes sample files without matching recording rows in the database.
    /// This addresses `Missing ... row` errors. The ids are added to the
    /// `garbage` table to indicate the files need to be deleted. Garbage is
    /// collected on normal startup.
    trash_orphan_sample_files: bool,

    /// Deletes recording rows in the database without matching sample files.
    /// This addresses `Recording ... missing file` errors.
    delete_orphan_rows: bool,

    /// Trashes recordings when their database rows appear corrupt.
    /// This addresses "bad video_index" errors. The ids are added to the
    /// `garbage` table to indicate their files need to be deleted. Garbage is
    /// collected on normal startup.
    trash_corrupt_rows: bool,
}

pub fn run(args: Args) -> Result<i32, Error> {
    let (_db_dir, mut conn) = super::open_conn(&args.db_dir, super::OpenMode::ReadWrite)?;
    check::run(
        &mut conn,
        &check::Options {
            compare_lens: args.compare_lens,
            trash_orphan_sample_files: args.trash_orphan_sample_files,
            delete_orphan_rows: args.delete_orphan_rows,
            trash_corrupt_rows: args.trash_corrupt_rows,
        },
    )
}
