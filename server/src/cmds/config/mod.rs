// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2017 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Text-based configuration interface.
//!
//! This code is a bit messy, but it's essentially a prototype. Eventually Moonfire NVR's
//! configuration will likely be almost entirely done through a web-based UI.

use base::clock;
use base::Error;
use bpaf::Bpaf;
use cursive::views;
use cursive::Cursive;
use std::path::PathBuf;
use std::sync::Arc;

mod cameras;
mod dirs;
mod tab_complete;
mod users;

/// Interactively edits configuration.
#[derive(Bpaf, Debug)]
#[bpaf(command("config"))]
pub struct Args {
    #[bpaf(external(crate::parse_db_dir))]
    db_dir: PathBuf,
}

fn block_on<O>(f: impl std::future::Future<Output = O>) -> O {
    tokio::runtime::Handle::current().block_on(f)
}

pub fn run(args: Args) -> Result<i32, Error> {
    let (_db_dir, conn) = super::open_conn(&args.db_dir, super::OpenMode::ReadWrite)?;
    let clocks = clock::RealClocks {};
    let db = Arc::new(db::Database::new(clocks, conn, true)?);

    // This runtime is needed by the "Test" button in the camera config.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_io()
        .enable_time()
        .build()?;
    let _enter = rt.enter();

    let mut siv = cursive::default();
    //siv.add_global_callback('q', |s| s.quit());

    siv.add_layer(
        views::Dialog::around(
            views::SelectView::<fn(&Arc<db::Database>, &mut Cursive)>::new()
                .on_submit(move |siv, item| item(&db, siv))
                .item("Cameras and streams", cameras::top_dialog)
                .item("Directories and retention", dirs::top_dialog)
                .item("Users", users::top_dialog),
        )
        .button("Quit", |siv| siv.quit())
        .title("Main menu"),
    );

    siv.run();

    Ok(0)
}
