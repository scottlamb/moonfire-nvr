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

use db;
use error::Error;
use rusqlite;

mod v0_to_v1;

const USAGE: &'static str = r#"
Upgrade to the latest database schema.

Usage: moonfire-nvr upgrade [options]

Options:
    -h, --help             Show this message.
    --db-dir=DIR           Set the directory holding the SQLite3 index database.
                           This is typically on a flash device.
                           [default: /var/lib/moonfire-nvr/db]
    --sample-file-dir=DIR  Set the directory holding video data.
                           This is typically on a hard drive.
                           [default: /var/lib/moonfire-nvr/sample]
    --preset-journal=MODE  Resets the SQLite journal_mode to the specified mode
                           prior to the upgrade. The default, delete, is
                           recommended. off is very dangerous but may be
                           desirable in some circumstances. See guide/schema.md
                           for more information. The journal mode will be reset
                           to wal after the upgrade.
                           [default: delete]
    --no-vacuum            Skips the normal post-upgrade vacuum operation.
"#;

const UPGRADE_NOTES: &'static str =
    concat!("upgraded using moonfire-nvr ", env!("CARGO_PKG_VERSION"));

const UPGRADERS: [fn(&rusqlite::Transaction) -> Result<(), Error>; 1] = [
    v0_to_v1::run,
];

#[derive(Debug, RustcDecodable)]
struct Args {
    flag_db_dir: String,
    flag_sample_file_dir: String,
    flag_preset_journal: String,
    flag_no_vacuum: bool,
}

fn set_journal_mode(conn: &rusqlite::Connection, requested: &str) -> Result<(), Error> {
    assert!(!requested.contains(';'));  // quick check for accidental sql injection.
    let actual = conn.query_row(&format!("pragma journal_mode = {}", requested), &[],
                                |row| row.get_checked::<_, String>(0))??;
    info!("...database now in journal_mode {} (requested {}).", actual, requested);
    Ok(())
}

pub fn run() -> Result<(), Error> {
    let args: Args = super::parse_args(USAGE)?;
    let (_db_dir, mut conn) = super::open_conn(&args.flag_db_dir, super::OpenMode::ReadWrite)?;

    {
        assert_eq!(UPGRADERS.len(), db::EXPECTED_VERSION as usize);
        let old_ver =
            conn.query_row("select max(id) from version", &[], |row| row.get_checked(0))??;
        if old_ver > db::EXPECTED_VERSION {
            return Err(Error::new(format!("Database is at version {}, later than expected {}",
                                          old_ver, db::EXPECTED_VERSION)))?;
        } else if old_ver < 0 {
            return Err(Error::new(format!("Database is at negative version {}!", old_ver)));
        }
        info!("Upgrading database from version {} to version {}...", old_ver, db::EXPECTED_VERSION);
        set_journal_mode(&conn, &args.flag_preset_journal).unwrap();
        for ver in old_ver .. db::EXPECTED_VERSION {
            info!("...from version {} to version {}", ver, ver + 1);
            let tx = conn.transaction()?;
            UPGRADERS[ver as usize](&tx)?;
            tx.execute(r#"
                insert into version (id, unix_time, notes)
                             values (?, cast(strftime('%s', 'now') as int32), ?)
            "#, &[&(ver + 1), &UPGRADE_NOTES])?;
            tx.commit()?;
        }
    }

    // WAL is the preferred journal mode for normal operation; it reduces the number of syncs
    // without compromising safety.
    set_journal_mode(&conn, "wal").unwrap();
    if !args.flag_no_vacuum {
        info!("...vacuuming database after upgrade.");
        conn.execute_batch(r#"
            pragma page_size = 16384;
            vacuum;
        "#).unwrap();
    }
    info!("...done.");
    Ok(())
}
