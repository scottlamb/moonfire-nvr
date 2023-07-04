// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

#![cfg_attr(all(feature = "nightly", test), feature(test))]

use bpaf::{Bpaf, Parser};
use log::{debug, error};
use std::ffi::OsStr;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

mod body;
mod cmds;
mod h264;
mod json;
mod mp4;
mod slices;
mod stream;
mod streamer;
mod web;

const DEFAULT_DB_DIR: &str = "/var/lib/moonfire-nvr/db";

/// Moonfire NVR: security camera network video recorder.
#[derive(Bpaf, Debug)]
#[bpaf(options, version)]
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
    fn run(self) -> Result<i32, failure::Error> {
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

/// Custom panic hook that logs instead of directly writing to stderr.
///
/// This means it includes a timestamp and is more recognizable as a serious
/// error (including console color coding by default, a format `lnav` will
/// recognize, etc.).
fn panic_hook(p: &std::panic::PanicInfo) {
    let mut msg;
    if let Some(l) = p.location() {
        msg = format!("panic at '{l}'");
    } else {
        msg = "panic".to_owned();
    }
    if let Some(s) = p.payload().downcast_ref::<&str>() {
        write!(&mut msg, ": {s}").unwrap();
    } else if let Some(s) = p.payload().downcast_ref::<String>() {
        write!(&mut msg, ": {s}").unwrap();
    }
    let b = failure::Backtrace::new();
    if b.is_empty() {
        write!(
            &mut msg,
            "\n\n(set environment variable RUST_BACKTRACE=1 to see backtraces)"
        )
        .unwrap();
    } else {
        write!(&mut msg, "\n\nBacktrace:\n{b}").unwrap();
    }
    error!("{}", msg);
}

fn main() {
    if let Err(e) = nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC) {
        eprintln!(
            "clock_gettime failed: {e}\n\n\
             This indicates a broken environment. See the troubleshooting guide."
        );
        std::process::exit(1);
    }

    let mut h = mylog::Builder::new()
        .set_format(
            ::std::env::var("MOONFIRE_FORMAT")
                .map_err(|_| ())
                .and_then(|s| mylog::Format::from_str(&s))
                .unwrap_or(mylog::Format::Google),
        )
        .set_color(
            ::std::env::var("MOONFIRE_COLOR")
                .map_err(|_| ())
                .and_then(|s| mylog::ColorMode::from_str(&s))
                .unwrap_or(mylog::ColorMode::Auto),
        )
        .set_spec(&::std::env::var("MOONFIRE_LOG").unwrap_or_else(|_| "info".to_owned()))
        .build();
    h.clone().install().unwrap();

    // Get the program name from the OS (e.g. if invoked as `target/debug/nvr`: `nvr`),
    // falling back to the crate name if conversion to a path/UTF-8 string fails.
    // `bpaf`'s default logic is similar but doesn't have the fallback.
    let progname = std::env::args_os().next().map(PathBuf::from);
    let progname = progname
        .as_deref()
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        .unwrap_or(env!("CARGO_PKG_NAME"));

    let use_panic_hook = ::std::env::var("MOONFIRE_PANIC_HOOK")
        .map(|s| s != "false" && s != "0")
        .unwrap_or(true);
    if use_panic_hook {
        std::panic::set_hook(Box::new(&panic_hook));
    }

    let args = match args()
        .fallback_to_usage()
        .run_inner(bpaf::Args::current_args().set_name(progname))
    {
        Ok(a) => a,
        Err(e) => std::process::exit(e.exit_code()),
    };
    log::trace!("Parsed command-line arguments: {args:#?}");

    let r = {
        let _a = h.async_scope();
        args.run()
    };
    match r {
        Err(e) => {
            error!("Exiting due to error: {}", base::prettify_failure(&e));
            ::std::process::exit(1);
        }
        Ok(rv) => {
            debug!("Exiting with status {}", rv);
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
