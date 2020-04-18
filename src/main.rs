// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2016-2020 The Moonfire NVR Authors
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

#![cfg_attr(all(feature="nightly", test), feature(test))]

use log::{error, info};
use structopt::StructOpt;

mod body;
mod cmds;
mod h264;
mod json;
mod mp4;
mod slices;
mod stream;
mod streamer;
mod web;

#[derive(StructOpt)]
#[structopt(name="moonfire-nvr", about="security camera network video recorder")]
enum Args {
    /// Checks database integrity (like fsck).
    Check(cmds::check::Args),

    /// Interactively edits configuration.
    Config(cmds::config::Args),

    /// Initializes a database.
    Init(cmds::init::Args),

    /// Logs in a user, returning the session cookie.
    ///
    /// This is a privileged command that directly accesses the database. It doesn't check the
    /// user's password and even can be used to create sessions with permissions the user doesn't
    /// have.
    Login(cmds::login::Args),

    /// Runs the server, saving recordings and allowing web access.
    Run(cmds::run::Args),

    /// Runs a SQLite3 shell on Moonfire NVR's index database.
    ///
    /// Note this locks the database to prevent simultaneous access with a running server. The
    /// server maintains cached state which could be invalidated otherwise.
    Sql(cmds::sql::Args),

    /// Translates between integer and human-readable timestamps.
    Ts(cmds::ts::Args),

    /// Upgrades to the latest database schema.
    Upgrade(cmds::upgrade::Args),
}

impl Args {
    fn run(&self) -> Result<(), failure::Error> {
        match self {
            Args::Check(ref a) => cmds::check::run(a),
            Args::Config(ref a) => cmds::config::run(a),
            Args::Init(ref a) => cmds::init::run(a),
            Args::Login(ref a) => cmds::login::run(a),
            Args::Run(ref a) => cmds::run::run(a),
            Args::Sql(ref a) => cmds::sql::run(a),
            Args::Ts(ref a) => cmds::ts::run(a),
            Args::Upgrade(ref a) => cmds::upgrade::run(a),
        }
    }
}

fn parse_fmt<S: AsRef<str>>(fmt: S) -> Option<mylog::Format> {
    match fmt.as_ref() {
        "google" => Some(mylog::Format::Google),
        "google-systemd" => Some(mylog::Format::GoogleSystemd),
        _ => None,
    }
}

fn main() {
    let args = Args::from_args();
    let mut h = mylog::Builder::new()
        .set_format(::std::env::var("MOONFIRE_FORMAT")
                    .ok()
                    .and_then(parse_fmt)
                    .unwrap_or(mylog::Format::Google))
        .set_spec(&::std::env::var("MOONFIRE_LOG").unwrap_or("info".to_owned()))
        .build();
    h.clone().install().unwrap();

    if let Err(e) = { let _a = h.async_scope(); args.run() } {
        error!("{:?}", e);
        ::std::process::exit(1);
    }
    info!("Success.");
}
