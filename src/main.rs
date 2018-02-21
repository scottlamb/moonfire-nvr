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

#![cfg_attr(all(feature="nightly", test), feature(test))]

extern crate byteorder;
extern crate core;
extern crate docopt;
extern crate futures;
extern crate futures_cpupool;
#[macro_use] extern crate failure;
extern crate fnv;
extern crate http_serve;
extern crate hyper;
#[macro_use] extern crate lazy_static;
extern crate libc;
#[macro_use] extern crate log;
extern crate lru_cache;
extern crate reffers;
extern crate rusqlite;
extern crate memmap;
extern crate mime;
extern crate moonfire_ffmpeg;
extern crate mylog;
extern crate openssl;
extern crate parking_lot;
extern crate protobuf;
extern crate regex;
extern crate serde;
#[macro_use] extern crate serde_derive;
extern crate serde_json;
extern crate smallvec;
extern crate time;
extern crate tokio_core;
extern crate tokio_signal;
extern crate url;
extern crate uuid;

mod clock;
mod coding;
mod cmds;
mod db;
mod dir;
mod h264;
mod json;
mod mp4;
mod recording;
mod schema;
mod slices;
mod stream;
mod streamer;
mod strutil;
#[cfg(test)] mod testutil;
mod web;

/// Commandline usage string. This is in the particular format expected by the `docopt` crate.
/// Besides being printed on --help or argument parsing error, it's actually parsed to define the
/// allowed commandline arguments and their defaults.
const USAGE: &'static str = "
Usage: moonfire-nvr <command> [<args>...]
       moonfire-nvr (--help | --version)

Options:
    -h, --help             Show this message.
    --version              Show the version of moonfire-nvr.

Commands:
    check                  Check database integrity
    init                   Initialize a database
    run                    Run the daemon: record from cameras and serve HTTP
    shell                  Start an interactive shell to modify the database
    ts                     Translate human-readable and numeric timestamps
    upgrade                Upgrade the database to the latest schema
";

/// Commandline arguments corresponding to `USAGE`; automatically filled by the `docopt` crate.
#[derive(Debug, Deserialize)]
struct Args {
    arg_command: Option<cmds::Command>,
}

fn version() -> String {
    let major = option_env!("CARGO_PKG_VERSION_MAJOR");
    let minor = option_env!("CARGO_PKG_VERSION_MAJOR");
    let patch = option_env!("CARGO_PKG_VERSION_MAJOR");
    match (major, minor, patch) {
        (Some(major), Some(minor), Some(patch)) => format!("{}.{}.{}", major, minor, patch),
        _ => "".to_owned(),
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
    // Parse commandline arguments.
    // (Note this differs from cmds::parse_args in that it specifies options_first.)
    let args: Args = docopt::Docopt::new(USAGE)
                                    .and_then(|d| d.options_first(true)
                                                   .version(Some(version()))
                                                   .deserialize())
                                    .unwrap_or_else(|e| e.exit());

    let mut h = mylog::Builder::new()
        .set_format(::std::env::var("MOONFIRE_FORMAT")
                    .ok()
                    .and_then(parse_fmt)
                    .unwrap_or(mylog::Format::Google))
        .set_spec(&::std::env::var("MOONFIRE_LOG").unwrap_or("info".to_owned()))
        .build();
    h.clone().install().unwrap();

    if let Err(e) = { let _a = h.async(); args.arg_command.unwrap().run() } {
        error!("{}", e);
        ::std::process::exit(1);
    }
    info!("Success.");
}
