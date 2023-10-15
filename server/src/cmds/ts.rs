// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use base::Error;
use bpaf::Bpaf;

/// Translates between integer and human-readable timestamps.
#[derive(Bpaf, Debug)]
#[bpaf(command("ts"))]
pub struct Args {
    /// Timestamp(s) to translate.
    ///
    /// May be either an integer or an RFC-3339-like string:
    /// `YYYY-mm-dd[THH:MM[:SS[:FFFFF]]][{Z,{+,-,}HH:MM}]`.
    ///
    /// E.g.: `142913484000000`, `2020-04-26`, `2020-04-26T12:00:00:00000-07:00`.
    #[bpaf(positional("TS"), some("must specify at least one timestamp"))]
    timestamps: Vec<String>,
}

pub fn run(args: Args) -> Result<i32, Error> {
    for timestamp in &args.timestamps {
        let t = db::recording::Time::parse(timestamp)?;
        println!("{} == {}", t, t.0);
    }
    Ok(0)
}
