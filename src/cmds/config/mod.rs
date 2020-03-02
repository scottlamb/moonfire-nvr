// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2017 The Moonfire NVR Authors
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

use base::clock;
use cursive::Cursive;
use cursive::views;
use db;
use failure::Error;
use serde::Deserialize;
use std::sync::Arc;

mod cameras;
mod dirs;
mod users;

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

#[derive(Debug, Deserialize)]
struct Args {
    flag_db_dir: String,
}

pub fn run() -> Result<(), Error> {
    let args: Args = super::parse_args(USAGE)?;
    let (_db_dir, conn) = super::open_conn(&args.flag_db_dir, super::OpenMode::ReadWrite)?;
    let clocks = clock::RealClocks {};
    let db = Arc::new(db::Database::new(clocks, conn, true)?);

    let mut siv = Cursive::ncurses()?;
    //siv.add_global_callback('q', |s| s.quit());

    siv.add_layer(views::Dialog::around(
        views::SelectView::<fn(&Arc<db::Database>, &mut Cursive)>::new()
            .on_submit({
                let db = db.clone();
                move |siv, item| item(&db, siv)
            })
            .item("Cameras and streams".to_string(), cameras::top_dialog)
            .item("Directories and retention".to_string(), dirs::top_dialog)
            .item("Users".to_string(), users::top_dialog)
            )
        .button("Quit", |siv| siv.quit())
        .title("Main menu"));

    siv.run();

    Ok(())
}
