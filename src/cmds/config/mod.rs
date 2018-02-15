// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2017 Scott Lamb <slamb@slamb.org>
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

//! Text-based configuration interface.
//!
//! This code is a bit messy, but it's essentially a prototype. Eventually Moonfire NVR's
//! configuration will likely be almost entirely done through a web-based UI.

extern crate cursive;

use self::cursive::Cursive;
use self::cursive::views;
use db;
use error::Error;
use regex::Regex;
use std::sync::Arc;
use std::fmt::Write;
use std::str::FromStr;

mod cameras;
mod dirs;

static USAGE: &'static str = r#"
Interactive configuration editor.

Usage:

    moonfire-nvr config [options]
    moonfire-nvr config --help

Options:

    --db-dir=DIR           Set the directory holding the SQLite3 index database.
                           This is typically on a flash device.
                           [default: /var/lib/moonfire-nvr/db]
"#;

static MULTIPLIERS: [(char, u64); 4] = [
    // (suffix character, power of 2)
    ('T', 40),
    ('G', 30),
    ('M', 20),
    ('K', 10),
];

fn encode_size(mut raw: i64) -> String {
    let mut encoded = String::new();
    for &(c, n) in &MULTIPLIERS {
        if raw >= 1i64<<n {
            write!(&mut encoded, "{}{} ", raw >> n, c).unwrap();
            raw &= (1i64 << n) - 1;
        }
    }
    if raw > 0 || encoded.len() == 0 {
        write!(&mut encoded, "{}", raw).unwrap();
    } else {
        encoded.pop();  // remove trailing space.
    }
    encoded
}

fn decode_size(encoded: &str) -> Result<i64, ()> {
    let mut decoded = 0i64;
    lazy_static! {
        static ref RE: Regex = Regex::new(r"\s*([0-9]+)([TGMK])?,?\s*").unwrap();
    }
    let mut last_pos = 0;
    for cap in RE.captures_iter(encoded) {
        let whole_cap = cap.get(0).unwrap();
        if whole_cap.start() > last_pos {
            return Err(());
        }
        last_pos = whole_cap.end();
        let mut piece = i64::from_str(&cap[1]).map_err(|_| ())?;
        if let Some(m) = cap.get(2) {
            let m = m.as_str().as_bytes()[0] as char;
            for &(some_m, n) in &MULTIPLIERS {
                if some_m == m {
                    piece *= 1i64<<n;
                    break;
                }
            }
        }
        decoded += piece;
    }
    if last_pos < encoded.len() {
        return Err(());
    }
    Ok(decoded)
}

#[derive(Debug, Deserialize)]
struct Args {
    flag_db_dir: String,
}

pub fn run() -> Result<(), Error> {
    let args: Args = super::parse_args(USAGE)?;
    let (_db_dir, conn) = super::open_conn(&args.flag_db_dir, super::OpenMode::ReadWrite)?;
    let db = Arc::new(db::Database::new(conn, true)?);

    let mut siv = Cursive::new();
    //siv.add_global_callback('q', |s| s.quit());

    siv.add_layer(views::Dialog::around(
        views::SelectView::<fn(&Arc<db::Database>, &mut Cursive)>::new()
            .on_submit({
                let db = db.clone();
                move |siv, item| item(&db, siv)
            })
            .item("Directories and retention".to_string(), dirs::top_dialog)
            .item("Cameras and streams".to_string(), cameras::top_dialog)
            )
        .button("Quit", |siv| siv.quit())
        .title("Main menu"));

    siv.run();

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_decode() {
        assert_eq!(super::decode_size("100M").unwrap(), 100i64 << 20);
    }
}
