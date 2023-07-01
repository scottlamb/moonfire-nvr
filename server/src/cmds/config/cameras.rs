// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use crate::stream::{self, Opener};
use base::strutil::{decode_size, encode_size};
use cursive::traits::{Finder, Nameable, Resizable, Scrollable};
use cursive::views::{self, Dialog, ViewRef};
use cursive::Cursive;
use db::writer;
use failure::{bail, format_err, Error, ResultExt};
use itertools::Itertools;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use url::Url;

#[derive(Debug)]
struct Camera {
    short_name: String,
    description: String,
    onvif_base_url: String,
    username: String,
    password: String,
    streams: [Stream; db::NUM_STREAM_TYPES],
}

#[derive(Debug, Default)]
struct Stream {
    url: String,
    record: bool,
    flush_if_sec: String,
    rtsp_transport: &'static str,
    sample_file_dir_id: Option<i32>,
}

/// Builds a `Camera` from an active `edit_camera_dialog`. No validation.
fn get_camera(siv: &mut Cursive) -> Camera {
    // Note: these find_name calls are separate statements, which seems to be important:
    // https://github.com/gyscos/Cursive/issues/144
    let short_name = siv
        .find_name::<views::EditView>("short_name")
        .unwrap()
        .get_content()
        .as_str()
        .into();
    let description = siv
        .find_name::<views::TextArea>("description")
        .unwrap()
        .get_content()
        .into();
    let onvif_base_url: String = siv
        .find_name::<views::EditView>("onvif_base_url")
        .unwrap()
        .get_content()
        .as_str()
        .into();
    let username = siv
        .find_name::<views::EditView>("username")
        .unwrap()
        .get_content()
        .as_str()
        .to_owned();
    let password = siv
        .find_name::<views::EditView>("password")
        .unwrap()
        .get_content()
        .as_str()
        .to_owned();
    let mut camera = Camera {
        short_name,
        description,
        onvif_base_url,
        username,
        password,
        streams: Default::default(),
    };
    for &t in &db::ALL_STREAM_TYPES {
        let url = siv
            .find_name::<views::EditView>(&format!("{}_url", t))
            .unwrap()
            .get_content()
            .as_str()
            .to_owned();
        let record = siv
            .find_name::<views::Checkbox>(&format!("{}_record", t))
            .unwrap()
            .is_checked();
        let rtsp_transport = *siv
            .find_name::<views::SelectView<&'static str>>(&format!("{}_rtsp_transport", t))
            .unwrap()
            .selection()
            .unwrap();
        let flush_if_sec = siv
            .find_name::<views::EditView>(&format!("{}_flush_if_sec", t))
            .unwrap()
            .get_content()
            .as_str()
            .to_owned();
        let sample_file_dir_id = *siv
            .find_name::<views::SelectView<Option<i32>>>(&format!("{}_sample_file_dir", t))
            .unwrap()
            .selection()
            .unwrap();
        camera.streams[t.index()] = Stream {
            url,
            record,
            flush_if_sec,
            rtsp_transport,
            sample_file_dir_id,
        };
    }
    log::trace!("camera is: {:#?}", &camera);
    camera
}

/// Attempts to parse a URL field into a sort-of-validated URL.
fn parse_url(
    field_name: &str,
    raw: &str,
    allowed_schemes: &'static [&'static str],
) -> Result<Option<Url>, Error> {
    if raw.is_empty() {
        return Ok(None);
    }
    let url = url::Url::parse(raw)
        .with_context(|_| format!("can't parse {} {:?} as URL", field_name, &raw))?;
    if !allowed_schemes.iter().any(|scheme| *scheme == url.scheme()) {
        bail!(
            "Unexpected scheme in {} {:?}; should be one of: {}",
            field_name,
            url.as_str(),
            allowed_schemes.iter().join(", ")
        );
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!(
            "Unexpected credentials in {} {:?}; use the username and password fields instead",
            field_name,
            url.as_str()
        );
    }
    Ok(Some(url))
}

fn parse_stream_url(type_: db::StreamType, raw: &str) -> Result<Option<Url>, Error> {
    parse_url(&format!("{} stream url", type_.as_str()), raw, &["rtsp"])
}

fn press_edit(siv: &mut Cursive, db: &Arc<db::Database>, id: Option<i32>) {
    let result = (|| {
        let mut l = db.lock();
        let mut change = if let Some(id) = id {
            l.null_camera_change(id)?
        } else {
            db::CameraChange::default()
        };
        let camera = get_camera(siv);
        change.short_name = camera.short_name;
        change.config.description = camera.description;
        change.config.onvif_base_url =
            parse_url("onvif_base_url", &camera.onvif_base_url, &["http", "https"])?;
        change.config.username = camera.username;
        change.config.password = camera.password;
        for (i, stream) in camera.streams.iter().enumerate() {
            let type_ = db::StreamType::from_index(i).unwrap();
            if stream.record && (stream.url.is_empty() || stream.sample_file_dir_id.is_none()) {
                bail!(
                    "Can't record {} stream without RTSP URL and sample file directory",
                    type_.as_str()
                );
            }
            let stream_change = &mut change.streams[i];
            stream_change.config.mode = (if stream.record {
                db::json::STREAM_MODE_RECORD
            } else {
                ""
            })
            .to_owned();
            stream_change.config.url = parse_stream_url(type_, &stream.url)?;
            stream_change.config.rtsp_transport = stream.rtsp_transport.to_owned();
            stream_change.sample_file_dir_id = stream.sample_file_dir_id;
            stream_change.config.flush_if_sec = if stream.flush_if_sec.is_empty() {
                0
            } else {
                stream.flush_if_sec.parse().map_err(|_| {
                    format_err!(
                        "flush_if_sec for {} must be a non-negative integer",
                        type_.as_str()
                    )
                })?
            };
        }
        if let Some(id) = id {
            l.update_camera(id, change)
        } else {
            l.add_camera(change).map(|_| ())
        }
    })();
    if let Err(e) = result {
        siv.add_layer(
            views::Dialog::text(format!(
                "Unable to {} camera: {}",
                if id.is_some() { "edit" } else { "add" },
                e
            ))
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

fn press_test_inner(
    handle: tokio::runtime::Handle,
    url: Url,
    username: String,
    password: String,
    transport: retina::client::Transport,
) -> Result<String, Error> {
    let _enter = handle.enter();
    let options = stream::Options {
        session: retina::client::SessionOptions::default().creds(if username.is_empty() {
            None
        } else {
            Some(retina::client::Credentials { username, password })
        }),
        setup: retina::client::SetupOptions::default().transport(transport),
    };
    let stream = stream::OPENER.open("test stream".to_owned(), url, options)?;
    let video_sample_entry = stream.video_sample_entry();
    Ok(format!(
        "codec: {}\n\
         dimensions: {}x{}\n\
         pixel aspect ratio: {}x{}\n\
         tool: {:?}",
        &video_sample_entry.rfc6381_codec,
        video_sample_entry.width,
        video_sample_entry.height,
        video_sample_entry.pasp_h_spacing,
        video_sample_entry.pasp_v_spacing,
        stream.tool(),
    ))
}

fn press_test(siv: &mut Cursive, t: db::StreamType) {
    let c = get_camera(siv);
    let s = &c.streams[t.index()];
    let transport = retina::client::Transport::from_str(s.rtsp_transport).unwrap_or_default();
    let url = match parse_stream_url(t, &s.url) {
        Ok(Some(u)) => u,
        _ => panic!(
            "test button should only be enabled with valid URL, not {:?}",
            &s.url
        ),
    };
    let username = c.username;
    let password = c.password;

    siv.add_layer(
        views::Dialog::text(format!(
            "Testing {} stream at {}. This may take a while \
            on timeout or if you have a long key frame interval",
            t.as_str(),
            url.as_str()
        ))
        .title("Testing"),
    );

    // Let siv have this thread for its event loop; do the work in a background thread.
    // siv.cb_sink doesn't actually wake up the event loop. Tell siv to poll, as a workaround.
    siv.set_fps(5);
    let sink = siv.cb_sink().clone();

    // Note: this expects to be called within a tokio runtime. Currently this
    // is set up by the config subcommand's run().
    let handle = tokio::runtime::Handle::current();
    ::std::thread::spawn(move || {
        let r = press_test_inner(handle, url.clone(), username, password, transport);
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
            "Delete camera {name}? This camera has no recorded video."
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
                    let Some(dir_id) = stream.sample_file_dir_id else {
                        continue
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
                views::Dialog::text(format!("Unable to delete recordings: {e}"))
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
    let dirs_to_open: Vec<_> = zero_limits.keys().copied().collect();
    db.lock().open_sample_file_dirs(&dirs_to_open[..])?;
    for (&dir_id, l) in &zero_limits {
        writer::lower_retention(db, dir_id, l)?;
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
            views::Dialog::text(format!("Unable to delete camera: {e}"))
                .title("Error")
                .dismiss_button("Abort"),
        );
    } else {
        // Recreate the "Edit cameras" dialog from scratch; it's easier than adding the new entry.
        siv.pop_layer();
        top_dialog(db, siv);
    }
}

fn edit_stream_url(type_: db::StreamType, content: &str, mut test_button: ViewRef<views::Button>) {
    let enable_test = matches!(parse_stream_url(type_, content), Ok(Some(_)));
    test_button.set_enabled(enable_test);
}

fn load_camera_values(
    db: &Arc<db::Database>,
    camera_id: i32,
    dialog: &mut Dialog,
    overwrite_uuid: bool,
) -> (String, i64) {
    let dirs: Vec<_> = ::std::iter::once(("<none>".into(), None))
        .chain(
            db.lock()
                .sample_file_dirs_by_id()
                .iter()
                .map(|(&id, d)| (d.path.to_owned(), Some(id))),
        )
        .collect();
    let l = db.lock();
    let camera = l.cameras_by_id().get(&camera_id).expect("missing camera");
    if overwrite_uuid {
        dialog
            .call_on_name("uuid", |v: &mut views::TextView| {
                v.set_content(camera.uuid.to_string())
            })
            .expect("missing TextView");
    }

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
            let u = if s.config.retain_bytes == 0 {
                "0 / 0 (0.0%)".to_owned()
            } else {
                format!(
                    "{} / {} ({:.1}%)",
                    s.fs_bytes,
                    s.config.retain_bytes,
                    100. * s.fs_bytes as f32 / s.config.retain_bytes as f32
                )
            };
            dialog.call_on_name(&format!("{}_url", t.as_str()), |v: &mut views::EditView| {
                if let Some(url) = s.config.url.as_ref() {
                    v.set_content(url.as_str().to_owned());
                }
            });
            let test_button = dialog
                .find_name::<views::Button>(&format!("{}_test", t.as_str()))
                .unwrap();
            edit_stream_url(
                t,
                s.config.url.as_ref().map(Url::as_str).unwrap_or(""),
                test_button,
            );
            dialog.call_on_name(
                &format!("{}_usage_cap", t.as_str()),
                |v: &mut views::TextView| v.set_content(u),
            );
            dialog.call_on_name(
                &format!("{}_record", t.as_str()),
                |v: &mut views::Checkbox| {
                    v.set_checked(s.config.mode == db::json::STREAM_MODE_RECORD)
                },
            );
            dialog.call_on_name(
                &format!("{}_rtsp_transport", t.as_str()),
                |v: &mut views::SelectView<&'static str>| {
                    v.set_selection(match s.config.rtsp_transport.as_str() {
                        "tcp" => 1,
                        "udp" => 2,
                        _ => 0,
                    })
                },
            );
            dialog.call_on_name(&format!("{}_flush_if_sec", t), |v: &mut views::EditView| {
                v.set_content(s.config.flush_if_sec.to_string())
            });
        }
        log::debug!("setting {} dir to {}", t.as_str(), selected_dir);
        dialog.call_on_name(
            &format!("{}_sample_file_dir", t),
            |v: &mut views::SelectView<Option<i32>>| v.set_selection(selected_dir),
        );
    }
    let name = camera.short_name.clone();
    for &(view_id, content) in &[
        ("short_name", &*camera.short_name),
        (
            "onvif_base_url",
            camera
                .config
                .onvif_base_url
                .as_ref()
                .map_or("", Url::as_str),
        ),
        ("username", &camera.config.username),
        ("password", &camera.config.password),
    ] {
        dialog
            .call_on_name(view_id, |v: &mut views::EditView| {
                v.set_content(content.to_string())
            })
            .expect("missing EditView");
    }
    dialog
        .call_on_name("description", |v: &mut views::TextArea| {
            v.set_content(camera.config.description.clone())
        })
        .expect("missing TextArea");
    (name, bytes)
}

/// Adds or updates a camera.
/// (The former if `item` is None; the latter otherwise.)
fn edit_camera_dialog(db: &Arc<db::Database>, siv: &mut Cursive, item: &Option<i32>) {
    let camera_list = views::ListView::new()
        .child(
            "id",
            views::TextView::new(item.map_or_else(|| "<new>".to_string(), |id| id.to_string())),
        )
        .child("uuid", views::TextView::new("<new>").with_name("uuid"))
        .child("short name", views::EditView::new().with_name("short_name"))
        .child(
            "onvif_base_url",
            views::EditView::new().with_name("onvif_base_url"),
        )
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

    let dirs: Vec<_> = ::std::iter::once(("<none>".into(), None))
        .chain(
            db.lock()
                .sample_file_dirs_by_id()
                .iter()
                .map(|(&id, d)| (d.path.to_owned(), Some(id))),
        )
        .collect();
    for &type_ in &db::ALL_STREAM_TYPES {
        let list = views::ListView::new()
            .child(
                "rtsp url",
                views::LinearLayout::horizontal()
                    .child(
                        views::EditView::new()
                            .on_edit(move |siv, content, _pos| {
                                let test_button = siv
                                    .find_name::<views::Button>(&format!("{}_test", type_))
                                    .unwrap();
                                edit_stream_url(type_, content, test_button);
                            })
                            .with_name(format!("{}_url", type_))
                            .full_width(),
                    )
                    .child(views::DummyView)
                    .child(
                        views::Button::new("Test", move |siv| press_test(siv, type_))
                            .disabled()
                            .with_name(format!("{}_test", type_)),
                    ),
            )
            .child(
                "sample file dir",
                views::SelectView::<Option<i32>>::new()
                    .with_all(dirs.iter().map(|(p, id)| (p.display().to_string(), *id)))
                    .popup()
                    .with_name(format!("{}_sample_file_dir", type_)),
            )
            .child(
                "record",
                views::Checkbox::new().with_name(format!("{}_record", type_)),
            )
            .child(
                "rtsp_transport",
                views::SelectView::<&str>::new()
                    .with_all([("(default)", ""), ("tcp", "tcp"), ("udp", "udp")])
                    .popup()
                    .with_name(format!("{}_rtsp_transport", type_)),
            )
            .child(
                "flush_if_sec",
                views::EditView::new().with_name(format!("{}_flush_if_sec", type_)),
            )
            .child(
                "usage/capacity",
                views::TextView::new("").with_name(format!("{}_usage_cap", type_)),
            )
            .min_height(5);
        layout.add_child(views::DummyView);
        layout.add_child(views::TextView::new(format!("{} stream", type_)));
        layout.add_child(list);
    }

    let mut dialog = views::Dialog::around(layout.scrollable());
    let dialog = if let Some(camera_id) = *item {
        let (name, bytes) = load_camera_values(db, camera_id, &mut dialog, true);
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
        dialog
            .title("Add camera")
            .button("Add", {
                let db = db.clone();
                move |s| press_edit(s, &db, None)
            })
            .button("Copy config", {
                let db = db.clone();
                move |s| copy_camera_dialog(s, &db)
            })
    };
    siv.add_layer(dialog.dismiss_button("Cancel"));
}

fn copy_camera_dialog(siv: &mut Cursive, db: &Arc<db::Database>) {
    siv.add_layer(
        views::Dialog::around(
            views::SelectView::new()
                .with_all(
                    db.lock()
                        .cameras_by_id()
                        .iter()
                        .map(|(&id, camera)| (format!("{}: {}", id, camera.short_name), id)),
                )
                .on_submit({
                    let db = db.clone();
                    move |siv, &camera_id| {
                        siv.pop_layer();
                        let screen = siv.screen_mut();
                        let dialog = screen.get_mut(views::LayerPosition::FromFront(0)).unwrap();
                        let dialog = dialog.downcast_mut::<Dialog>().unwrap();
                        load_camera_values(&db, camera_id, dialog, false);
                    }
                })
                .full_width(),
        )
        .dismiss_button("Cancel")
        .title("Select camera to copy"),
    );
}

pub fn top_dialog(db: &Arc<db::Database>, siv: &mut Cursive) {
    siv.add_layer(
        views::Dialog::around(
            views::SelectView::new()
                .on_submit({
                    let db = db.clone();
                    move |siv, item| edit_camera_dialog(&db, siv, item)
                })
                .item("<new camera>", None)
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
