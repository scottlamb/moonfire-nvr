// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use crate::stream::{self, Opener, Stream};
use base::strutil::{decode_size, encode_size};
use cursive::traits::{Boxable, Finder, Identifiable};
use cursive::views;
use cursive::Cursive;
use db::writer;
use failure::Error;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use url::Url;

/// Builds a `CameraChange` from an active `edit_camera_dialog`.
fn get_change(siv: &mut Cursive) -> db::CameraChange {
    // Note: these find_name calls are separate statements, which seems to be important:
    // https://github.com/gyscos/Cursive/issues/144
    let sn = siv
        .find_name::<views::EditView>("short_name")
        .unwrap()
        .get_content()
        .as_str()
        .into();
    let d = siv
        .find_name::<views::TextArea>("description")
        .unwrap()
        .get_content()
        .into();
    let h = siv
        .find_name::<views::EditView>("onvif_host")
        .unwrap()
        .get_content()
        .as_str()
        .into();
    let u = siv
        .find_name::<views::EditView>("username")
        .unwrap()
        .get_content()
        .as_str()
        .into();
    let p = siv
        .find_name::<views::EditView>("password")
        .unwrap()
        .get_content()
        .as_str()
        .into();
    let mut c = db::CameraChange {
        short_name: sn,
        description: d,
        onvif_host: h,
        username: u,
        password: p,
        streams: Default::default(),
    };
    for &t in &db::ALL_STREAM_TYPES {
        let u = siv
            .find_name::<views::EditView>(&format!("{}_rtsp_url", t.as_str()))
            .unwrap()
            .get_content()
            .as_str()
            .into();
        let r = siv
            .find_name::<views::Checkbox>(&format!("{}_record", t.as_str()))
            .unwrap()
            .is_checked();
        let f = i64::from_str(
            siv.find_name::<views::EditView>(&format!("{}_flush_if_sec", t.as_str()))
                .unwrap()
                .get_content()
                .as_str(),
        )
        .unwrap_or(0);
        let d = *siv
            .find_name::<views::SelectView<Option<i32>>>(&format!("{}_sample_file_dir", t.as_str()))
            .unwrap()
            .selection()
            .unwrap();
        c.streams[t.index()] = db::StreamChange {
            rtsp_url: u,
            sample_file_dir_id: d,
            record: r,
            flush_if_sec: f,
        };
    }
    c
}

fn press_edit(siv: &mut Cursive, db: &Arc<db::Database>, id: Option<i32>) {
    let change = get_change(siv);

    let result = {
        let mut l = db.lock();
        if let Some(id) = id {
            l.update_camera(id, change)
        } else {
            l.add_camera(change).map(|_| ())
        }
    };
    if let Err(e) = result {
        siv.add_layer(
            views::Dialog::text(format!("Unable to add camera: {}", e))
                .title("Error")
                .dismiss_button("Abort"),
        );
    } else {
        siv.pop_layer(); // get rid of the add/edit camera dialog.

        // Recreate the "Edit cameras" dialog from scratch; it's easier than adding the new entry.
        siv.pop_layer();
        top_dialog(db, siv);
    }
}

fn press_test_inner(url: &Url) -> Result<String, Error> {
    let stream = stream::FFMPEG.open(stream::Source::Rtsp {
        url: url.as_str(),
        redacted_url: url.as_str(), // don't need redaction in config UI.
    })?;
    let extra_data = stream.get_extra_data()?;
    Ok(format!(
        "{}x{} video stream",
        extra_data.entry.width, extra_data.entry.height
    ))
}

fn press_test(siv: &mut Cursive, t: db::StreamType) {
    let c = get_change(siv);
    let mut url = match Url::parse(&c.streams[t.index()].rtsp_url) {
        Ok(u) => u,
        Err(e) => {
            siv.add_layer(
                views::Dialog::text(format!("Unparseable URL: {}", e))
                    .title("Stream test failed")
                    .dismiss_button("Back"),
            );
            return;
        }
    };

    if !c.username.is_empty() {
        let _ = url.set_username(&c.username);
        let _ = url.set_password(Some(&c.password));
    }
    siv.add_layer(
        views::Dialog::text(format!(
            "Testing {} stream at {}. This may take a while \
            on timeout or if you have a long key frame interval",
            t.as_str(),
            &url
        ))
        .title("Testing"),
    );

    // Let siv have this thread for its event loop; do the work in a background thread.
    // siv.cb_sink doesn't actually wake up the event loop. Tell siv to poll, as a workaround.
    siv.set_fps(5);
    let sink = siv.cb_sink().clone();
    ::std::thread::spawn(move || {
        let r = press_test_inner(&url);
        sink.send(Box::new(move |siv: &mut Cursive| {
            // Polling is no longer necessary.
            siv.set_fps(0);
            siv.pop_layer();
            let description = match r {
                Err(ref e) => {
                    siv.add_layer(
                        views::Dialog::text(format!("{} stream at {}:\n\n{}", t.as_str(), &url, e))
                            .title("Stream test failed")
                            .dismiss_button("Back"),
                    );
                    return;
                }
                Ok(ref d) => d,
            };
            siv.add_layer(
                views::Dialog::text(format!(
                    "{} stream at {}:\n\n{}",
                    t.as_str(),
                    &url,
                    description
                ))
                .title("Stream test succeeded")
                .dismiss_button("Back"),
            );
        }))
        .unwrap();
    });
}

fn press_delete(siv: &mut Cursive, db: &Arc<db::Database>, id: i32, name: String, to_delete: i64) {
    let dialog = if to_delete > 0 {
        let prompt = format!(
            "Camera {} has recorded video. Please confirm the amount \
            of data to delete by typing it back:\n\n{}",
            name,
            encode_size(to_delete)
        );
        views::Dialog::around(
            views::LinearLayout::vertical()
                .child(views::TextView::new(prompt))
                .child(views::DummyView)
                .child(
                    views::EditView::new()
                        .on_submit({
                            let db = db.clone();
                            move |siv, _| confirm_deletion(siv, &db, id, to_delete)
                        })
                        .with_name("confirm"),
                ),
        )
        .button("Delete", {
            let db = db.clone();
            move |siv| confirm_deletion(siv, &db, id, to_delete)
        })
    } else {
        views::Dialog::text(format!(
            "Delete camera {}? This camera has no recorded video.",
            name
        ))
        .button("Delete", {
            let db = db.clone();
            move |s| actually_delete(s, &db, id)
        })
    }
    .title("Delete camera")
    .dismiss_button("Cancel");
    siv.add_layer(dialog);
}

fn confirm_deletion(siv: &mut Cursive, db: &Arc<db::Database>, id: i32, to_delete: i64) {
    let typed = siv
        .find_name::<views::EditView>("confirm")
        .unwrap()
        .get_content();
    if decode_size(typed.as_str()).ok() == Some(to_delete) {
        siv.pop_layer(); // deletion confirmation dialog

        let mut zero_limits = BTreeMap::new();
        {
            let l = db.lock();
            for (&stream_id, stream) in l.streams_by_id() {
                if stream.camera_id == id {
                    let dir_id = match stream.sample_file_dir_id {
                        Some(d) => d,
                        None => continue,
                    };
                    let l = zero_limits
                        .entry(dir_id)
                        .or_insert_with(|| Vec::with_capacity(2));
                    l.push(writer::NewLimit {
                        stream_id,
                        limit: 0,
                    });
                }
            }
        }
        if let Err(e) = lower_retention(db, zero_limits) {
            siv.add_layer(
                views::Dialog::text(format!("Unable to delete recordings: {}", e))
                    .title("Error")
                    .dismiss_button("Abort"),
            );
            return;
        }
        actually_delete(siv, db, id);
    } else {
        siv.add_layer(
            views::Dialog::text("Please confirm amount.")
                .title("Try again")
                .dismiss_button("Back"),
        );
    }
}

fn lower_retention(
    db: &Arc<db::Database>,
    zero_limits: BTreeMap<i32, Vec<writer::NewLimit>>,
) -> Result<(), Error> {
    let dirs_to_open: Vec<_> = zero_limits.keys().map(|id| *id).collect();
    db.lock().open_sample_file_dirs(&dirs_to_open[..])?;
    for (&dir_id, l) in &zero_limits {
        writer::lower_retention(db.clone(), dir_id, &l)?;
    }
    Ok(())
}

fn actually_delete(siv: &mut Cursive, db: &Arc<db::Database>, id: i32) {
    siv.pop_layer(); // get rid of the add/edit camera dialog.
    let result = {
        let mut l = db.lock();
        l.delete_camera(id)
    };
    if let Err(e) = result {
        siv.add_layer(
            views::Dialog::text(format!("Unable to delete camera: {}", e))
                .title("Error")
                .dismiss_button("Abort"),
        );
    } else {
        // Recreate the "Edit cameras" dialog from scratch; it's easier than adding the new entry.
        siv.pop_layer();
        top_dialog(db, siv);
    }
}

/// Adds or updates a camera.
/// (The former if `item` is None; the latter otherwise.)
fn edit_camera_dialog(db: &Arc<db::Database>, siv: &mut Cursive, item: &Option<i32>) {
    let camera_list = views::ListView::new()
        .child(
            "id",
            views::TextView::new(match *item {
                None => "<new>".to_string(),
                Some(id) => id.to_string(),
            }),
        )
        .child("uuid", views::TextView::new("<new>").with_name("uuid"))
        .child("short name", views::EditView::new().with_name("short_name"))
        .child("onvif_host", views::EditView::new().with_name("onvif_host"))
        .child("username", views::EditView::new().with_name("username"))
        .child("password", views::EditView::new().with_name("password"))
        .min_height(6);
    let mut layout = views::LinearLayout::vertical()
        .child(camera_list)
        .child(views::TextView::new("description"))
        .child(
            views::TextArea::new()
                .with_name("description")
                .min_height(3),
        );

    let dirs: Vec<_> = ::std::iter::once(("<none>".to_owned(), None))
        .chain(
            db.lock()
                .sample_file_dirs_by_id()
                .iter()
                .map(|(&id, d)| (d.path.as_str().to_owned(), Some(id))),
        )
        .collect();
    for &type_ in &db::ALL_STREAM_TYPES {
        let list = views::ListView::new()
            .child(
                "rtsp url",
                views::LinearLayout::horizontal()
                    .child(
                        views::EditView::new()
                            .with_name(format!("{}_rtsp_url", type_.as_str()))
                            .full_width(),
                    )
                    .child(views::DummyView)
                    .child(views::Button::new("Test", move |siv| {
                        press_test(siv, type_)
                    })),
            )
            .child(
                "sample file dir",
                views::SelectView::<Option<i32>>::new()
                    .with_all(dirs.iter().map(|d| d.clone()))
                    .popup()
                    .with_name(format!("{}_sample_file_dir", type_.as_str())),
            )
            .child(
                "record",
                views::Checkbox::new().with_name(format!("{}_record", type_.as_str())),
            )
            .child(
                "flush_if_sec",
                views::EditView::new().with_name(format!("{}_flush_if_sec", type_.as_str())),
            )
            .child(
                "usage/capacity",
                views::TextView::new("").with_name(format!("{}_usage_cap", type_.as_str())),
            )
            .min_height(5);
        layout.add_child(views::DummyView);
        layout.add_child(views::TextView::new(format!("{} stream", type_.as_str())));
        layout.add_child(list);
    }

    let mut dialog = views::Dialog::around(layout);
    let dialog = if let Some(camera_id) = *item {
        let l = db.lock();
        let camera = l.cameras_by_id().get(&camera_id).expect("missing camera");
        dialog
            .call_on_name("uuid", |v: &mut views::TextView| {
                v.set_content(camera.uuid.to_string())
            })
            .expect("missing TextView");

        let mut bytes = 0;
        for (i, sid) in camera.streams.iter().enumerate() {
            let t = db::StreamType::from_index(i).unwrap();

            // Find the index into dirs of the stored sample file dir.
            let mut selected_dir = 0;
            if let Some(s) = sid.map(|sid| l.streams_by_id().get(&sid).unwrap()) {
                if let Some(id) = s.sample_file_dir_id {
                    for (i, &(_, d_id)) in dirs.iter().skip(1).enumerate() {
                        if Some(id) == d_id {
                            selected_dir = i + 1;
                            break;
                        }
                    }
                }
                bytes += s.sample_file_bytes;
                let u = if s.retain_bytes == 0 {
                    "0 / 0 (0.0%)".to_owned()
                } else {
                    format!(
                        "{} / {} ({:.1}%)",
                        s.fs_bytes,
                        s.retain_bytes,
                        100. * s.fs_bytes as f32 / s.retain_bytes as f32
                    )
                };
                dialog.call_on_name(
                    &format!("{}_rtsp_url", t.as_str()),
                    |v: &mut views::EditView| v.set_content(s.rtsp_url.to_owned()),
                );
                dialog.call_on_name(
                    &format!("{}_usage_cap", t.as_str()),
                    |v: &mut views::TextView| v.set_content(u),
                );
                dialog.call_on_name(
                    &format!("{}_record", t.as_str()),
                    |v: &mut views::Checkbox| v.set_checked(s.record),
                );
                dialog.call_on_name(
                    &format!("{}_flush_if_sec", t.as_str()),
                    |v: &mut views::EditView| v.set_content(s.flush_if_sec.to_string()),
                );
            }
            dialog.call_on_name(
                &format!("{}_sample_file_dir", t.as_str()),
                |v: &mut views::SelectView<Option<i32>>| v.set_selection(selected_dir),
            );
        }
        let name = camera.short_name.clone();
        for &(view_id, content) in &[
            ("short_name", &*camera.short_name),
            ("onvif_host", &*camera.onvif_host),
            ("username", &*camera.username),
            ("password", &*camera.password),
        ] {
            dialog
                .call_on_name(view_id, |v: &mut views::EditView| {
                    v.set_content(content.to_string())
                })
                .expect("missing EditView");
        }
        dialog
            .call_on_name("description", |v: &mut views::TextArea| {
                v.set_content(camera.description.to_string())
            })
            .expect("missing TextArea");
        dialog
            .title("Edit camera")
            .button("Edit", {
                let db = db.clone();
                move |s| press_edit(s, &db, Some(camera_id))
            })
            .button("Delete", {
                let db = db.clone();
                move |s| press_delete(s, &db, camera_id, name.clone(), bytes)
            })
    } else {
        for t in &db::ALL_STREAM_TYPES {
            dialog.call_on_name(
                &format!("{}_usage_cap", t.as_str()),
                |v: &mut views::TextView| v.set_content("<new>"),
            );
        }
        dialog.title("Add camera").button("Add", {
            let db = db.clone();
            move |s| press_edit(s, &db, None)
        })
    };
    siv.add_layer(dialog.dismiss_button("Cancel"));
}

pub fn top_dialog(db: &Arc<db::Database>, siv: &mut Cursive) {
    siv.add_layer(
        views::Dialog::around(
            views::SelectView::new()
                .on_submit({
                    let db = db.clone();
                    move |siv, item| edit_camera_dialog(&db, siv, item)
                })
                .item("<new camera>".to_string(), None)
                .with_all(
                    db.lock()
                        .cameras_by_id()
                        .iter()
                        .map(|(&id, camera)| (format!("{}: {}", id, camera.short_name), Some(id))),
                )
                .full_width(),
        )
        .dismiss_button("Done")
        .title("Edit cameras"),
    );
}
