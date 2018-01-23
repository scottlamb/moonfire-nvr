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
use db;
use dir;
use error::Error;
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
    let m = siv.find_id::<views::EditView>("main_rtsp_path").unwrap().get_content().as_str().into();
    let s = siv.find_id::<views::EditView>("sub_rtsp_path").unwrap().get_content().as_str().into();
    db::CameraChange {
        short_name: sn,
        description: d,
        host: h,
        username: u,
        password: p,
        rtsp_paths: [m, s],
    }
}

fn press_edit(siv: &mut Cursive, db: &Arc<db::Database>, dir: &Arc<dir::SampleFileDir>,
              id: Option<i32>) {
    let change = get_change(siv);
    siv.pop_layer();  // get rid of the add/edit camera dialog.

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
        // Recreate the "Edit cameras" dialog from scratch; it's easier than adding the new entry.
        siv.pop_layer();
        add_dialog(db, dir, siv);
    }
}

fn press_test_inner(url: &str) -> Result<String, Error> {
    let stream = stream::FFMPEG.open(stream::Source::Rtsp(url))?;
    let extra_data = stream.get_extra_data()?;
    Ok(format!("{}x{} video stream", extra_data.width, extra_data.height))
}

fn press_test(siv: &mut Cursive, c: &db::CameraChange, stream: &str, path: &str) {
    let url = format!("rtsp://{}:{}@{}{}", c.username, c.password, c.host, path);
    let description = match press_test_inner(&url) {
        Err(e) => {
            siv.add_layer(
                views::Dialog::text(format!("{} stream at {}:\n\n{}", stream, url, e))
                .title("Stream test failed")
                .dismiss_button("Back"));
            return;
        },
        Ok(d) => d,
    };
    siv.add_layer(views::Dialog::text(format!("{} stream at {}:\n\n{}", stream, url, description))
                  .title("Stream test succeeded")
                  .dismiss_button("Back"));
}

fn press_delete(siv: &mut Cursive, db: &Arc<db::Database>, dir: &Arc<dir::SampleFileDir>, id: i32,
                name: String, to_delete: i64) {
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
                let dir = dir.clone();
                move |siv, _| confirm_deletion(siv, &db, &dir, id, to_delete)
            }).with_id("confirm")))
        .button("Delete", {
            let db = db.clone();
            let dir = dir.clone();
            move |siv| confirm_deletion(siv, &db, &dir, id, to_delete)
        })
    } else {
        views::Dialog::text(format!("Delete camera {}? This camera has no recorded video.", name))
        .button("Delete", {
            let db = db.clone();
            let dir = dir.clone();
            move |s| actually_delete(s, &db, &dir, id)
        })
    }.title("Delete camera").dismiss_button("Cancel");
    siv.add_layer(dialog);
}

fn confirm_deletion(siv: &mut Cursive, db: &Arc<db::Database>, dir: &Arc<dir::SampleFileDir>,
                    id: i32, to_delete: i64) {
    let typed = siv.find_id::<views::EditView>("confirm").unwrap().get_content();
    if decode_size(typed.as_str()).ok() == Some(to_delete) {
        siv.pop_layer();  // deletion confirmation dialog

        let mut zero_limits = Vec::new();
        {
            let l = db.lock();
            for (&stream_id, stream) in l.streams_by_id() {
                if stream.camera_id == id {
                    zero_limits.push(dir::NewLimit {
                        stream_id,
                        limit: 0,
                    });
                }
            }
        }
        if let Err(e) = dir::lower_retention(dir.clone(), &zero_limits) {
            siv.add_layer(views::Dialog::text(format!("Unable to delete recordings: {}", e))
                          .title("Error")
                          .dismiss_button("Abort"));
            return;
        }
        actually_delete(siv, db, dir, id);
    } else {
        siv.add_layer(views::Dialog::text("Please confirm amount.")
                      .title("Try again")
                      .dismiss_button("Back"));
    }
}

fn actually_delete(siv: &mut Cursive, db: &Arc<db::Database>, dir: &Arc<dir::SampleFileDir>,
                   id: i32) {
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
        add_dialog(db, dir, siv);
    }
}

/// Adds or updates a camera.
/// (The former if `item` is None; the latter otherwise.)
fn edit_camera_dialog(db: &Arc<db::Database>, dir: &Arc<dir::SampleFileDir>, siv: &mut Cursive,
                      item: &Option<i32>) {
    let list = views::ListView::new()
        .child("id", views::TextView::new(match *item {
            None => "<new>".to_string(),
            Some(id) => id.to_string(),
        }))
        .child("uuid", views::TextView::new("<new>").with_id("uuid"))
        .child("short name", views::EditView::new().with_id("short_name"))
        .child("host", views::EditView::new().with_id("host"))
        .child("username", views::EditView::new().with_id("username"))
        .child("password", views::EditView::new().with_id("password"))
        .child("main_rtsp_path", views::LinearLayout::horizontal()
               .child(views::EditView::new().with_id("main_rtsp_path").full_width())
               .child(views::DummyView)
               .child(views::Button::new("Test", |siv| {
                   let c = get_change(siv);
                   press_test(siv, &c, "main", &c.rtsp_paths[0])
               })))
        .child("sub_rtsp_path", views::LinearLayout::horizontal()
               .child(views::EditView::new().with_id("sub_rtsp_path").full_width())
               .child(views::DummyView)
               .child(views::Button::new("Test", |siv| {
                   let c = get_change(siv);
                   press_test(siv, &c, "sub", &c.rtsp_paths[1])
               })))
        .min_height(8);
    let layout = views::LinearLayout::vertical()
        .child(list)
        .child(views::TextView::new("description"))
        .child(views::TextArea::new().with_id("description").min_height(3))
        .full_width();
    let mut dialog = views::Dialog::around(layout);
    let dialog = if let Some(camera_id) = *item {
        let l = db.lock();
        let camera = l.cameras_by_id().get(&camera_id).expect("missing camera");
        dialog.find_id("uuid", |v: &mut views::TextView| v.set_content(camera.uuid.to_string()))
              .expect("missing TextView");
        let mut main_rtsp_path = "";
        let mut sub_rtsp_path = "";
        let mut bytes = 0;
        for (_, s) in l.streams_by_id() {
            if s.camera_id != camera_id { continue; }
            bytes += s.sample_file_bytes;
            match s.type_ {
                db::StreamType::MAIN => main_rtsp_path = &s.rtsp_path,
                db::StreamType::SUB => sub_rtsp_path = &s.rtsp_path,
            };
        }
        let name = camera.short_name.clone();
        for &(view_id, content) in &[("short_name", &*camera.short_name),
                                     ("host", &*camera.host),
                                     ("username", &*camera.username),
                                     ("password", &*camera.password),
                                     ("main_rtsp_path", main_rtsp_path),
                                     ("sub_rtsp_path", sub_rtsp_path)] {
            dialog.find_id(view_id, |v: &mut views::EditView| v.set_content(content.to_string()))
                  .expect("missing EditView");
        }
        for s in l.streams_by_id().values() {
            if s.camera_id != camera_id { continue };
            let id = match s.type_ {
                db::StreamType::MAIN => "main_rtsp_path",
                db::StreamType::SUB  => "sub_rtsp_path",
            };
            dialog.find_id(id, |v: &mut views::EditView| v.set_content(s.rtsp_path.to_string()))
                  .expect("missing EditView");
        }
        dialog.find_id("description",
                       |v: &mut views::TextArea| v.set_content(camera.description.to_string()))
              .expect("missing TextArea");
        dialog.title("Edit camera")
              .button("Edit", {
                  let db = db.clone();
                  let dir = dir.clone();
                  move |s| press_edit(s, &db, &dir, Some(camera_id))
              })
              .button("Delete", {
                  let db = db.clone();
                  let dir = dir.clone();
                  move |s| press_delete(s, &db, &dir, camera_id, name.clone(), bytes)
              })
    } else {
        dialog.title("Add camera")
              .button("Add", {
                  let db = db.clone();
                  let dir = dir.clone();
                  move |s| press_edit(s, &db, &dir, None)
              })
    };
    siv.add_layer(dialog.dismiss_button("Cancel"));
}

pub fn add_dialog(db: &Arc<db::Database>, dir: &Arc<dir::SampleFileDir>, siv: &mut Cursive) {
    siv.add_layer(views::Dialog::around(
        views::SelectView::new()
            .on_submit({
                let db = db.clone();
                let dir = dir.clone();
                move |siv, item| edit_camera_dialog(&db, &dir, siv, item)
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
