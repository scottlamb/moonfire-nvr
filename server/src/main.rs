// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

#![cfg_attr(all(feature = "nightly", test), feature(test))]

use bpaf::{Bpaf, Parser};
use log::{debug, error};
use std::fmt::Write;
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

/// The program name, taken from the OS-provided arguments if available.
///
/// E.g. if invoked as `target/debug/nvr`, should return `nvr`.
static PROGNAME: once_cell::sync::Lazy<&'static str> = once_cell::sync::Lazy::new(|| {
    std::env::args_os()
        .next()
        .and_then(|p| {
            let p = std::path::PathBuf::from(p);
            p.file_name().and_then(|f| {
                f.to_str()
                    .map(|s| &*Box::leak(s.to_owned().into_boxed_str()))
            })
        })
        .unwrap_or(env!("CARGO_PKG_NAME"))
});

fn subcommand<T: 'static>(
    parser: bpaf::OptionParser<T>,
    cmd: &'static str,
) -> impl bpaf::Parser<T> {
    let usage = format!("Usage: {progname} {cmd} {{usage}}", progname = *PROGNAME);
    parser.usage(Box::leak(usage.into_boxed_str())).command(cmd)
}

const DEFAULT_DB_DIR: &str = "/var/lib/moonfire-nvr/db";

/// Moonfire NVR: security camera network video recorder.
#[derive(Bpaf, Debug)]
#[bpaf(options, version)]
enum Args {
    // See docstrings of `cmds::*::Args` structs for a description of the respective subcommands.
    Check(#[bpaf(external(cmds::check::subcommand))] cmds::check::Args),
    Config(#[bpaf(external(cmds::config::subcommand))] cmds::config::Args),
    Init(#[bpaf(external(cmds::init::subcommand))] cmds::init::Args),
    Login(#[bpaf(external(cmds::login::subcommand))] cmds::login::Args),
    Run(#[bpaf(external(cmds::run::subcommand))] cmds::run::Args),
    Sql(#[bpaf(external(cmds::sql::subcommand))] cmds::sql::Args),
    Ts(#[bpaf(external(cmds::ts::subcommand))] cmds::ts::Args),
    Upgrade(#[bpaf(external(cmds::upgrade::subcommand))] cmds::upgrade::Args),
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

fn parse_db_dir() -> impl Parser<std::path::PathBuf> {
    bpaf::long("db-dir")
        .help(format!(
            "Directory holding the SQLite3 index database.\nDefault: `{}`",
            DEFAULT_DB_DIR
        ))
        .argument::<std::path::PathBuf>("PATH")
        .fallback_with(|| Ok::<_, std::convert::Infallible>(DEFAULT_DB_DIR.into()))
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

    let args = args().usage(Box::leak(
        format!("Usage: {progname} {{usage}}", progname = *PROGNAME).into_boxed_str(),
    ));

    // TODO: remove this when bpaf adds more direct support for defaulting to `--help`.
    // See discussion: <https://github.com/pacak/bpaf/discussions/165>.
    if std::env::args_os().len() < 2 {
        std::process::exit(
            args.run_inner(bpaf::Args::from(&["--help"]))
                .unwrap_err()
                .exit_code(),
        );
    }

    let use_panic_hook = ::std::env::var("MOONFIRE_PANIC_HOOK")
        .map(|s| s != "false" && s != "0")
        .unwrap_or(true);
    if use_panic_hook {
        std::panic::set_hook(Box::new(&panic_hook));
    }

    let args = args.run();
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
