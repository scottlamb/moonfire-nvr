// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

#![cfg_attr(all(feature = "nightly", test), feature(test))]

use bpaf::Bpaf;
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

/// Moonfire NVR: security camera network video recorder.
#[derive(Bpaf, Debug)]
#[bpaf(options, version)]
enum Args {
    /// Checks database integrity (like fsck).
    #[bpaf(command)]
    Check(#[bpaf(external(cmds::check::args))] cmds::check::Args),

    /// Interactively edits configuration.
    #[bpaf(command)]
    Config(#[bpaf(external(cmds::config::args))] cmds::config::Args),

    /// Initializes a database.
    #[bpaf(command)]
    Init(#[bpaf(external(cmds::init::args))] cmds::init::Args),

    /// Logs in a user, returning the session cookie.
    ///
    ///
    /// This is a privileged command that directly accesses the database. It doesn't check the
    /// user's password and even can be used to create sessions with permissions the user doesn't
    /// have.
    #[bpaf(command)]
    Login(#[bpaf(external(cmds::login::args))] cmds::login::Args),

    /// Runs the server, saving recordings and allowing web access.
    #[bpaf(command)]
    Run(#[bpaf(external(cmds::run::args))] cmds::run::Args),

    /// Runs a SQLite3 shell on Moonfire NVR's index database.
    ///
    ///
    /// Note this locks the database to prevent simultaneous access with a running server. The
    /// server maintains cached state which could be invalidated otherwise.
    #[bpaf(command)]
    Sql(#[bpaf(external(cmds::sql::args))] cmds::sql::Args),

    /// Translates between integer and human-readable timestamps.
    #[bpaf(command)]
    Ts(#[bpaf(external(cmds::ts::args))] cmds::ts::Args),

    /// Upgrades to the latest database schema.
    #[bpaf(command)]
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

/// Returns the default database dir, for use in argument parsing with `bpaf(fallback_with(...))`.
fn default_db_dir() -> Result<std::path::PathBuf, std::convert::Infallible> {
    Ok("/var/lib/moonfire-nvr/db".into())
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

    // TODO: remove this when bpaf adds more direct support for defaulting to `--help`.
    // See discussion: <https://github.com/pacak/bpaf/discussions/165>.
    if std::env::args_os().len() < 2 {
        std::process::exit(
            args()
                .run_inner(bpaf::Args::from(&["--help"]))
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

    let args = args().run();
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
