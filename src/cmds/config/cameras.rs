// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2017 Scott Lamb <slamb@slamb.org>
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

extern crate cursive;

use self::cursive::Cursive;
use self::cursive::traits::{Boxable, Identifiable, Finder};
use self::cursive::views;
use db::{self, writer};
use failure::Error;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use stream::{self, Opener, Stream};
use super::{decode_size, encode_size};

/// Builds a `CameraChange` from an active `edit_camera_dialog`.
fn get_change(siv: &mut Cursive) -> db::CameraChange {
    // Note: these find_id calls are separate statements, which seems to be important:
    // https://github.com/gyscos/Cursive/issues/144
    let sn = siv.find_id::<views::EditView>("short_name").unwrap().get_content().as_str().into();
    let d = siv.find_id::<views::TextArea>("description").unwrap().get_content().into();
    let h = siv.find_id::<views::EditView>("host").unwrap().get_content().as_str().into();
    let u = siv.find_id::<views::EditView>("username").unwrap().get_content().as_str().into();
    let p = siv.find_id::<views::EditView>("password").unwrap().get_content().as_str().into();
    let mut c = db::CameraChange {
        short_name: sn,
        description: d,
        host: h,
        username: u,
        password: p,
        streams: Default::default(),
    };
    for &t in &db::ALL_STREAM_TYPES {
        let p = siv.find_id::<views::EditView>(&format!("{}_rtsp_path", t.as_str()))
                .unwrap().get_content().as_str().into();
        let r = siv.find_id::<views::Checkbox>(&format!("{}_record", t.as_str()))
                .unwrap().is_checked();
        let f = i64::from_str(siv.find_id::<views::EditView>(
                &format!("{}_flush_if_sec", t.as_str())).unwrap().get_content().as_str())
                .unwrap_or(0);
        let d = *siv.find_id::<views::SelectView<Option<i32>>>(
            &format!("{}_sample_file_dir", t.as_str()))
            .unwrap().selection();
        c.streams[t.index()] = db::StreamChange {
            rtsp_path: p,
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
        siv.add_layer(views::Dialog::text(format!("Unable to add camera: {}", e))
                      .title("Error")
                      .dismiss_button("Abort"));
    } else {
        siv.pop_layer();  // get rid of the add/edit camera dialog.

        // Recreate the "Edit cameras" dialog from scratch; it's easier than adding the new entry.
        siv.pop_layer();
        top_dialog(db, siv);
    }
}

fn press_test_inner(url: &str) -> Result<String, Error> {
    let stream = stream::FFMPEG.open(stream::Source::Rtsp(url))?;
    let extra_data = stream.get_extra_data()?;
    Ok(format!("{}x{} video stream", extra_data.width, extra_data.height))
}

fn press_test(siv: &mut Cursive, t: db::StreamType) {
    let c = get_change(siv);
    let url = format!("rtsp://{}:{}@{}{}", c.username, c.password, c.host,
                      c.streams[t.index()].rtsp_path);
    siv.add_layer(views::Dialog::text(format!("Testing {} stream at {}. This may take a while \
                                               on timeout or if you have a long key frame interval",
                                              t.as_str(), url))
                  .title("Testing"));

    // Let siv have this thread for its event loop; do the work in a background thread.
    // siv.cb_sink doesn't actually wake up the event loop. Tell siv to poll, as a workaround.
    siv.set_fps(5);
    let sink = siv.cb_sink().clone();
    ::std::thread::spawn(move || {
        let r = press_test_inner(&url);
        sink.send(Box::new(move |siv| {
            // Polling is no longer necessary.
            siv.set_fps(0);
            siv.pop_layer();
            let description = match r {
                Err(ref e) => {
                    siv.add_layer(
                        views::Dialog::text(format!("{} stream at {}:\n\n{}", t.as_str(), url, e))
                        .title("Stream test failed")
                        .dismiss_button("Back"));
                    return;
                },
                Ok(ref d) => d,
            };
            siv.add_layer(views::Dialog::text(
                    format!("{} stream at {}:\n\n{}", t.as_str(), url, description))
                    .title("Stream test succeeded")
                    .dismiss_button("Back"));
        })).unwrap();
    });
}

fn press_delete(siv: &mut Cursive, db: &Arc<db::Database>, id: i32, name: String, to_delete: i64) {
    let dialog = if to_delete > 0 {
        let prompt = format!("Camera {} has recorded video. Please confirm the amount \
                              of data to delete by typing it back:\n\n{}", name,
                              encode_size(to_delete));
        views::Dialog::around(
            views::LinearLayout::vertical()
            .child(views::TextView::new(prompt))
            .child(views::DummyView)
            .child(views::EditView::new().on_submit({
                let db = db.clone();
                move |siv, _| confirm_deletion(siv, &db, id, to_delete)
            }).with_id("confirm")))
        .button("Delete", {
            let db = db.clone();
            move |siv| confirm_deletion(siv, &db, id, to_delete)
        })
    } else {
        views::Dialog::text(format!("Delete camera {}? This camera has no recorded video.", name))
        .button("Delete", {
            let db = db.clone();
            move |s| actually_delete(s, &db, id)
        })
    }.title("Delete camera").dismiss_button("Cancel");
    siv.add_layer(dialog);
}

fn confirm_deletion(siv: &mut Cursive, db: &Arc<db::Database>, id: i32, to_delete: i64) {
    let typed = siv.find_id::<views::EditView>("confirm").unwrap().get_content();
    if decode_size(typed.as_str()).ok() == Some(to_delete) {
        siv.pop_layer();  // deletion confirmation dialog

        let mut zero_limits = BTreeMap::new();
        {
            let l = db.lock();
            for (&stream_id, stream) in l.streams_by_id() {
                if stream.camera_id == id {
                    let dir_id = match stream.sample_file_dir_id {
                        Some(d) => d,
                        None => continue,
                    };
                    let l = zero_limits.entry(dir_id).or_insert_with(|| Vec::with_capacity(2));
                    l.push(writer::NewLimit {
                        stream_id,
                        limit: 0,
                    });
                }
            }
        }
        if let Err(e) = lower_retention(db, zero_limits) {
            siv.add_layer(views::Dialog::text(format!("Unable to delete recordings: {}", e))
                          .title("Error")
                          .dismiss_button("Abort"));
            return;
        }
        actually_delete(siv, db, id);
    } else {
        siv.add_layer(views::Dialog::text("Please confirm amount.")
                      .title("Try again")
                      .dismiss_button("Back"));
    }
}

fn lower_retention(db: &Arc<db::Database>, zero_limits: BTreeMap<i32, Vec<writer::NewLimit>>)
                   -> Result<(), Error> {
    let dirs_to_open: Vec<_> = zero_limits.keys().map(|id| *id).collect();
    db.lock().open_sample_file_dirs(&dirs_to_open[..])?;
    for (&dir_id, l) in &zero_limits {
        writer::lower_retention(db.clone(), dir_id, &l)?;
    }
    Ok(())
}

fn actually_delete(siv: &mut Cursive, db: &Arc<db::Database>, id: i32) {
    siv.pop_layer();  // get rid of the add/edit camera dialog.
    let result = {
        let mut l = db.lock();
        l.delete_camera(id)
    };
    if let Err(e) = result {
        siv.add_layer(views::Dialog::text(format!("Unable to delete camera: {}", e))
                      .title("Error")
                      .dismiss_button("Abort"));
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
        .child("id", views::TextView::new(match *item {
            None => "<new>".to_string(),
            Some(id) => id.to_string(),
        }))
        .child("uuid", views::TextView::new("<new>").with_id("uuid"))
        .child("short name", views::EditView::new().with_id("short_name"))
        .child("host", views::EditView::new().with_id("host"))
        .child("username", views::EditView::new().with_id("username"))
        .child("password", views::EditView::new().with_id("password"))
        .min_height(6);
    let mut layout = views::LinearLayout::vertical()
        .child(camera_list)
        .child(views::TextView::new("description"))
        .child(views::TextArea::new().with_id("description").min_height(3));

    let dirs: Vec<_> = ::std::iter::once(("<none>".to_owned(), None))
                       .chain(db.lock()
                                .sample_file_dirs_by_id()
                                .iter()
                                .map(|(&id, d)| (d.path.as_str().to_owned(), Some(id))))
                       .collect();
    for &type_ in &db::ALL_STREAM_TYPES {
        let list = views::ListView::new()
            .child("rtsp path", views::LinearLayout::horizontal()
                .child(views::EditView::new()
                       .with_id(format!("{}_rtsp_path", type_.as_str()))
                       .full_width())
                .child(views::DummyView)
                .child(views::Button::new("Test", move |siv| press_test(siv, type_))))
            .child("sample file dir",
                   views::SelectView::<Option<i32>>::new()
                   .with_all(dirs.iter().map(|d| d.clone()))
                   .popup()
                   .with_id(format!("{}_sample_file_dir", type_.as_str())))
            .child("record", views::Checkbox::new().with_id(format!("{}_record", type_.as_str())))
            .child("flush_if_sec", views::EditView::new()
                   .with_id(format!("{}_flush_if_sec", type_.as_str())))
            .child("usage/capacity",
                   views::TextView::new("").with_id(format!("{}_usage_cap", type_.as_str())))
            .min_height(5);
        layout.add_child(views::DummyView);
        layout.add_child(views::TextView::new(format!("{} stream", type_.as_str())));
        layout.add_child(list);
    }

    let mut dialog = views::Dialog::around(layout);
    let dialog = if let Some(camera_id) = *item {
        let l = db.lock();
        let camera = l.cameras_by_id().get(&camera_id).expect("missing camera");
        dialog.find_id("uuid", |v: &mut views::TextView| v.set_content(camera.uuid.to_string()))
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
                    format!("{} / {} ({:.1}%)", s.sample_file_bytes, s.retain_bytes,
                                100. * s.sample_file_bytes as f32 / s.retain_bytes as f32)
                };
                dialog.find_id(&format!("{}_rtsp_path", t.as_str()),
                               |v: &mut views::EditView| v.set_content(s.rtsp_path.to_owned()));
                dialog.find_id(&format!("{}_usage_cap", t.as_str()),
                               |v: &mut views::TextView| v.set_content(u));
                dialog.find_id(&format!("{}_record", t.as_str()),
                               |v: &mut views::Checkbox| v.set_checked(s.record));
                dialog.find_id(&format!("{}_flush_if_sec", t.as_str()),
                               |v: &mut views::EditView| v.set_content(s.flush_if_sec.to_string()));
            }
            dialog.find_id(&format!("{}_sample_file_dir", t.as_str()),
                           |v: &mut views::SelectView<Option<i32>>| v.set_selection(selected_dir));
        }
        let name = camera.short_name.clone();
        for &(view_id, content) in &[("short_name", &*camera.short_name),
                                     ("host", &*camera.host),
                                     ("username", &*camera.username),
                                     ("password", &*camera.password)] {
            dialog.find_id(view_id, |v: &mut views::EditView| v.set_content(content.to_string()))
                  .expect("missing EditView");
        }
        dialog.find_id("description",
                       |v: &mut views::TextArea| v.set_content(camera.description.to_string()))
              .expect("missing TextArea");
        dialog.title("Edit camera")
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
            dialog.find_id(&format!("{}_usage_cap", t.as_str()),
                           |v: &mut views::TextView| v.set_content("<new>"));
        }
        dialog.title("Add camera")
              .button("Add", {
                  let db = db.clone();
                  move |s| press_edit(s, &db, None)
              })
    };
    siv.add_layer(dialog.dismiss_button("Cancel"));
}

pub fn top_dialog(db: &Arc<db::Database>, siv: &mut Cursive) {
    siv.add_layer(views::Dialog::around(
        views::SelectView::new()
            .on_submit({
                let db = db.clone();
                move |siv, item| edit_camera_dialog(&db, siv, item)
            })
            .item("<new camera>".to_string(), None)
            .with_all(db.lock()
                        .cameras_by_id()
                        .iter()
                        .map(|(&id, camera)| (format!("{}: {}", id, camera.short_name), Some(id))))
            .full_width())
        .dismiss_button("Done")
        .title("Edit cameras"));
}
