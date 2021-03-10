// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

#![cfg_attr(all(feature = "nightly", test), feature(test))]

use log::{debug, error};
use std::str::FromStr;
use structopt::StructOpt;

#[cfg(feature = "analytics")]
mod analytics;

/// Stub implementation of analytics module when not compiled with TensorFlow Lite.
#[cfg(not(feature = "analytics"))]
mod analytics {
    use failure::{bail, Error};

    pub struct ObjectDetector;

    impl ObjectDetector {
        pub fn new() -> Result<std::sync::Arc<ObjectDetector>, Error> {
            bail!("Recompile with --features=analytics for object detection.");
        }
    }

    pub struct ObjectDetectorStream;

    impl ObjectDetectorStream {
        pub fn new(
            _par: ffmpeg::avcodec::InputCodecParameters<'_>,
            _detector: &ObjectDetector,
        ) -> Result<Self, Error> {
            unimplemented!();
        }

        pub fn process_frame(
            &mut self,
            _pkt: &ffmpeg::avcodec::Packet<'_>,
            _detector: &ObjectDetector,
        ) -> Result<(), Error> {
            unimplemented!();
        }
    }
}

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
#[structopt(
    name = "moonfire-nvr",
    about = "security camera network video recorder"
)]
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
    fn run(&self) -> Result<i32, failure::Error> {
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

fn main() {
    let args = Args::from_args();
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
        .set_spec(&::std::env::var("MOONFIRE_LOG").unwrap_or("info".to_owned()))
        .build();
    h.clone().install().unwrap();

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
