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

use db;
use error::Error;

static USAGE: &'static str = r#"
Initializes a database.

Usage:

    moonfire-nvr init [options]
    moonfire-nvr init --help

Options:

    --db-dir=DIR           Set the directory holding the SQLite3 index database.
                           This is typically on a flash device.
                           [default: /var/lib/moonfire-nvr/db]
"#;

#[derive(Debug, RustcDecodable)]
struct Args {
    flag_db_dir: String,
}

pub fn run() -> Result<(), Error> {
    let args: Args = super::parse_args(USAGE)?;
    super::install_logger(false);
    let (_db_dir, mut conn) = super::open_conn(&args.flag_db_dir, super::OpenMode::Create)?;

    // Check if the database has already been initialized.
    let cur_ver = db::get_schema_version(&conn)?;
    if let Some(v) = cur_ver {
        info!("Database is already initialized with schema version {}.", v);
        return Ok(());
    }

    conn.execute_batch(r#"
        pragma journal_mode = wal;
        pragma page_size = 16384;
    "#)?;
    let tx = conn.transaction()?;
    tx.execute_batch(include_str!("../schema.sql"))?;
    tx.commit()?;
    info!("Database initialized.");
    Ok(())
}
