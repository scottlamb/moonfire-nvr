// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018-2020 The Moonfire NVR Authors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In addition, as a special exception, the copyright holders give
// permission to link the code of portions of this program with the
// OpenSSL library under certain conditions as described in each
// individual source file, and distribute linked combinations including
// the two.
//
// You must obey the GNU General Public License in all respects for all
// of the code used other than OpenSSL. If you modify file(s) with this
// exception, you may extend this exception to your version of the
// file(s), but you are not obligated to do so. If you do not wish to do
// so, delete this exception statement from your version. If you delete
// this exception statement from all source files in the program, then
// also delete it here.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Subcommand to check the database and sample file dir for errors.

use db::check;
use failure::Error;
use std::path::PathBuf;
use structopt::StructOpt;

#[derive(StructOpt)]
pub struct Args {
    /// Directory holding the SQLite3 index database.
    #[structopt(long, default_value = "/var/lib/moonfire-nvr/db", value_name="path",
                parse(from_os_str))]
    db_dir: PathBuf,

    /// Compare sample file lengths on disk to the database.
    #[structopt(long)]
    compare_lens: bool,

    /// Trash sample files without matching recording rows in the database.
    /// This addresses "Missing ... row" errors.
    ///
    /// The ids are added to the "garbage" table to indicate the files need to
    /// be deleted. Garbage is collected on normal startup.
    #[structopt(long)]
    trash_orphan_sample_files: bool,

    /// Delete recording rows in the database without matching sample files.
    /// This addresses "Recording ... missing file" errors.
    #[structopt(long)]
    delete_orphan_rows: bool,

    /// Trash recordings when their database rows appear corrupt.
    /// This addresses "bad video_index" errors.
    ///
    /// The ids are added to the "garbage" table to indicate their files need to
    /// be deleted. Garbage is collected on normal startup.
    #[structopt(long)]
    trash_corrupt_rows: bool,
}

pub fn run(args: &Args) -> Result<i32, Error> {
    let (_db_dir, mut conn) = super::open_conn(&args.db_dir, super::OpenMode::ReadWrite)?;
    check::run(&mut conn, &check::Options {
        compare_lens: args.compare_lens,
        trash_orphan_sample_files: args.trash_orphan_sample_files,
        delete_orphan_rows: args.delete_orphan_rows,
        trash_corrupt_rows: args.trash_corrupt_rows,
    })
}
