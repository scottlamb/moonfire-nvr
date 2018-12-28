// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2016 Scott Lamb <slamb@slamb.org>
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

/// Upgrades the database schema.
///
/// See `guide/schema.md` for more information.

use crate::db;
use failure::Error;

const USAGE: &'static str = r#"
Upgrade to the latest database schema.

Usage: moonfire-nvr upgrade [options]

Options:
    -h, --help             Show this message.
    --db-dir=DIR           Set the directory holding the SQLite3 index database.
                           This is typically on a flash device.
                           [default: /var/lib/moonfire-nvr/db]
    --sample-file-dir=DIR  When upgrading from schema version 1 to 2, the sample file directory.
                           This is typically on a hard drive.
    --preset-journal=MODE  Resets the SQLite journal_mode to the specified mode
                           prior to the upgrade. The default, delete, is
                           recommended. off is very dangerous but may be
                           desirable in some circumstances. See guide/schema.md
                           for more information. The journal mode will be reset
                           to wal after the upgrade.
                           [default: delete]
    --no-vacuum            Skips the normal post-upgrade vacuum operation.
"#;

#[derive(Debug, Deserialize)]
pub struct Args {
    flag_db_dir: String,
    flag_sample_file_dir: Option<String>,
    flag_preset_journal: String,
    flag_no_vacuum: bool,
}

pub fn run() -> Result<(), Error> {
    let args: Args = super::parse_args(USAGE)?;
    let (_db_dir, mut conn) = super::open_conn(&args.flag_db_dir, super::OpenMode::ReadWrite)?;

    db::upgrade::run(&db::upgrade::Args {
        flag_sample_file_dir: args.flag_sample_file_dir.as_ref().map(|s| s.as_str()),
        flag_preset_journal: &args.flag_preset_journal,
        flag_no_vacuum: args.flag_no_vacuum,
    }, &mut conn)
}
