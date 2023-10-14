// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

#![cfg_attr(all(feature = "nightly", test), feature(test))]

use base::Error;
use bpaf::{Bpaf, Parser};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use tracing::{debug, error};

mod body;
mod cmds;
mod h264;
mod json;
mod mp4;
mod slices;
mod stream;
mod streamer;
mod web;

#[cfg(feature = "bundled-ui")]
mod bundled_ui;

const DEFAULT_DB_DIR: &str = "/var/lib/moonfire-nvr/db";

// This is either in the environment when `cargo` is invoked or set from within `build.rs`.
const VERSION: &str = env!("VERSION");

/// Moonfire NVR: security camera network video recorder.
#[derive(Bpaf, Debug)]
#[bpaf(options, version(VERSION))]
enum Args {
    // See docstrings of `cmds::*::Args` structs for a description of the respective subcommands.
    Check(#[bpaf(external(cmds::check::args))] cmds::check::Args),
    Config(#[bpaf(external(cmds::config::args))] cmds::config::Args),
    Init(#[bpaf(external(cmds::init::args))] cmds::init::Args),
    Login(#[bpaf(external(cmds::login::args))] cmds::login::Args),
    Run(#[bpaf(external(cmds::run::args))] cmds::run::Args),
    Sql(#[bpaf(external(cmds::sql::args))] cmds::sql::Args),
    Ts(#[bpaf(external(cmds::ts::args))] cmds::ts::Args),
    Upgrade(#[bpaf(external(cmds::upgrade::args))] cmds::upgrade::Args),
}

impl Args {
    fn run(self) -> Result<i32, Error> {
        match self {
            Args::Check(a) => cmds::check::run(a),
            Args::Config(a) => cmds::config::run(a),
            Args::Init(a) => cmds::init::run(a),
            Args::Login(a) => cmds::login::run(a),
            Args::Run(a) => cmds::run::run(a),
            Args::Sql(a) => cmds::sql::run(a),
            Args::Ts(a) => cmds::ts::run(a),
            Args::Upgrade(a) => cmds::upgrade::run(a),
        }
    }
}

fn parse_db_dir() -> impl Parser<PathBuf> {
    bpaf::long("db-dir")
        .help("Directory holding the SQLite3 index database.")
        .argument::<PathBuf>("PATH")
        .fallback(DEFAULT_DB_DIR.into())
        .debug_fallback()
}

fn main() {
    // If using the clock will fail, find out now *before* trying to log
    // anything (with timestamps...) so we can print a helpful error.
    if let Err(e) = nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC) {
        eprintln!(
            "clock_gettime failed: {e}\n\n\
             This indicates a broken environment. See the troubleshooting guide."
        );
        std::process::exit(1);
    }

    base::tracing_setup::install();

    // Get the program name from the OS (e.g. if invoked as `target/debug/nvr`: `nvr`),
    // falling back to the crate name if conversion to a path/UTF-8 string fails.
    // `bpaf`'s default logic is similar but doesn't have the fallback.
    let progname = std::env::args_os().next().map(PathBuf::from);
    let progname = progname
        .as_deref()
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        .unwrap_or(env!("CARGO_PKG_NAME"));

    let args = match args()
        .fallback_to_usage()
        .run_inner(bpaf::Args::current_args().set_name(progname))
    {
        Ok(a) => a,
        Err(e) => std::process::exit(e.exit_code()),
    };
    tracing::trace!("Parsed command-line arguments: {args:#?}");

    match args.run() {
        Err(e) => {
            error!(err = %e.chain(), "exiting due to error");
            ::std::process::exit(1);
        }
        Ok(rv) => {
            debug!("exiting with status {}", rv);
            std::process::exit(rv)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bpaf_invariants() {
        super::args().check_invariants(false);
    }
}
