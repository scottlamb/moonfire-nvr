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
use self::cursive::traits::{Boxable, Identifiable};
use self::cursive::views;
use db;
use dir;
use error::Error;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use super::{decode_size, encode_size};

struct Stream {
    label: String,
    used: i64,
    record: bool,
    retain: Option<i64>,  // None if unparseable
}

struct Model {
    db: Arc<db::Database>,
    dir: Arc<dir::SampleFileDir>,
    fs_capacity: i64,
    total_used: i64,
    total_retain: i64,
    errors: isize,
    streams: BTreeMap<i32, Stream>,
}

/// Updates the limits in the database. Doesn't delete excess data (if any).
fn update_limits_inner(model: &Model) -> Result<(), Error> {
    let mut db = model.db.lock();
    let mut tx = db.tx()?;
    for (&id, stream) in &model.streams {
        tx.update_retention(id, stream.record, stream.retain.unwrap())?;
    }
    tx.commit()
}

fn update_limits(model: &Model, siv: &mut Cursive) {
    if let Err(e) = update_limits_inner(model) {
        siv.add_layer(views::Dialog::text(format!("Unable to update limits: {}", e))
                      .dismiss_button("Back")
                      .title("Error"));
    }
}

fn edit_limit(model: &RefCell<Model>, siv: &mut Cursive, id: i32, content: &str) {
    debug!("on_edit called for id {}", id);
    let mut model = model.borrow_mut();
    let model: &mut Model = &mut *model;
    let stream = model.streams.get_mut(&id).unwrap();
    let new_value = decode_size(content).ok();
    let delta = new_value.unwrap_or(0) - stream.retain.unwrap_or(0);
    let old_errors = model.errors;
    if delta != 0 {
        let prev_over = model.total_retain > model.fs_capacity;
        model.total_retain += delta;
        siv.find_id::<views::TextView>("total_retain")
            .unwrap()
            .set_content(encode_size(model.total_retain));
        let now_over = model.total_retain > model.fs_capacity;
        if now_over != prev_over {
            model.errors += if now_over { 1 } else { -1 };
            siv.find_id::<views::TextView>("total_ok")
                .unwrap()
                .set_content(if now_over { "*" } else { " " });
        }
    }
    if new_value.is_none() != stream.retain.is_none() {
        model.errors += if new_value.is_none() { 1 } else { -1 };
        siv.find_id::<views::TextView>(&format!("{}_ok", id))
            .unwrap()
            .set_content(if new_value.is_none() { "*" } else { " " });
    }
    stream.retain = new_value;
    info!("model.errors = {}", model.errors);
    if (model.errors == 0) != (old_errors == 0) {
        info!("toggling change state: errors={}", model.errors);
        siv.find_id::<views::Button>("change")
           .unwrap()
           .set_enabled(model.errors == 0);
    }
}

fn edit_record(model: &RefCell<Model>, id: i32, record: bool) {
    let mut model = model.borrow_mut();
    let model: &mut Model = &mut *model;
    let stream = model.streams.get_mut(&id).unwrap();
    stream.record = record;
}

fn confirm_deletion(model: &RefCell<Model>, siv: &mut Cursive, to_delete: i64) {
    let typed = siv.find_id::<views::EditView>("confirm")
                   .unwrap()
                   .get_content();
    debug!("confirm, typed: {} vs expected: {}", typed.as_str(), to_delete);
    if decode_size(typed.as_str()).ok() == Some(to_delete) {
        actually_delete(model, siv);
    } else {
        siv.add_layer(views::Dialog::text("Please confirm amount.")
                      .title("Try again")
                      .dismiss_button("Back"));
    }
}

fn actually_delete(model: &RefCell<Model>, siv: &mut Cursive) {
    let model = &*model.borrow();
    let new_limits: Vec<_> =
        model.streams.iter()
             .map(|(&id, s)| dir::NewLimit {stream_id: id, limit: s.retain.unwrap()})
             .collect();
    siv.pop_layer();  // deletion confirmation
    siv.pop_layer();  // retention dialog
    if let Err(e) = dir::lower_retention(model.dir.clone(), &new_limits[..]) {
        siv.add_layer(views::Dialog::text(format!("Unable to delete excess video: {}", e))
                      .title("Error")
                      .dismiss_button("Abort"));
    } else {
        update_limits(model, siv);
    }
}

fn press_change(model: &Rc<RefCell<Model>>, siv: &mut Cursive) {
    if model.borrow().errors > 0 {
        return;
    }
    let to_delete = model.borrow().streams.values().map(
        |s| ::std::cmp::max(s.used - s.retain.unwrap(), 0)).sum();
    debug!("change press, to_delete={}", to_delete);
    if to_delete > 0 {
        let prompt = format!("Some streams' usage exceeds new limit. Please confirm the amount \
                              of data to delete by typing it back:\n\n{}", encode_size(to_delete));
        let dialog = views::Dialog::around(
                views::LinearLayout::vertical()
                .child(views::TextView::new(prompt))
                .child(views::DummyView)
                .child(views::EditView::new().on_submit({
                    let model = model.clone();
                    move |siv, _| confirm_deletion(&model, siv, to_delete)
                }).with_id("confirm")))
            .button("Confirm", {
                let model = model.clone();
                move |siv| confirm_deletion(&model, siv, to_delete)
            })
            .dismiss_button("Cancel")
            .title("Confirm deletion");
        siv.add_layer(dialog);
    } else {
        siv.screen_mut().pop_layer();
        update_limits(&model.borrow(), siv);
    }
}

pub fn add_dialog(db: &Arc<db::Database>, dir: &Arc<dir::SampleFileDir>, siv: &mut Cursive) {
    let model = {
        let mut streams = BTreeMap::new();
        let mut total_used = 0;
        let mut total_retain = 0;
        {
            let db = db.lock();
            for (&id, s) in db.streams_by_id() {
                let c = db.cameras_by_id().get(&s.camera_id).expect("stream without camera");
                streams.insert(id, Stream {
                    label: format!("{}: {}: {}", id, c.short_name, s.type_.as_str()),
                    used: s.sample_file_bytes,
                    record: s.record,
                    retain: Some(s.retain_bytes),
                });
                total_used += s.sample_file_bytes;
                total_retain += s.retain_bytes;
            }
        }
        let stat = dir.statfs().unwrap();
        let fs_capacity = stat.f_bsize as i64 * stat.f_bavail as i64 + total_used;
        Rc::new(RefCell::new(Model{
            dir: dir.clone(),
            db: db.clone(),
            fs_capacity,
            total_used,
            total_retain,
            errors: (total_retain > fs_capacity) as isize,
            streams,
        }))
    };

    const RECORD_WIDTH: usize = 8;
    const BYTES_WIDTH: usize = 20;

    let mut list = views::ListView::new();
    list.add_child(
        "stream",
        views::LinearLayout::horizontal()
            .child(views::TextView::new("record").fixed_width(RECORD_WIDTH))
            .child(views::TextView::new("usage").fixed_width(BYTES_WIDTH))
            .child(views::TextView::new("limit").fixed_width(BYTES_WIDTH)));
    for (&id, stream) in &model.borrow().streams {
        let mut record_cb = views::Checkbox::new();
        record_cb.set_checked(stream.record);
        record_cb.set_on_change({
            let model = model.clone();
            move |_siv, record| edit_record(&model, id, record)
        });
        list.add_child(
            &stream.label,
            views::LinearLayout::horizontal()
                .child(record_cb.fixed_width(RECORD_WIDTH))
                .child(views::TextView::new(encode_size(stream.used)).fixed_width(BYTES_WIDTH))
                .child(views::EditView::new()
                    .content(encode_size(stream.retain.unwrap()))
                    .on_edit({
                        let model = model.clone();
                        move |siv, content, _pos| edit_limit(&model, siv, id, content)
                    })
                    .on_submit({
                        let model = model.clone();
                        move |siv, _| press_change(&model, siv)
                    })
                    .fixed_width(20))
                .child(views::TextView::new("").with_id(format!("{}_ok", id)).fixed_width(1)));
    }
    let over = model.borrow().total_retain > model.borrow().fs_capacity;
    list.add_child(
        "total",
        views::LinearLayout::horizontal()
            .child(views::DummyView{}.fixed_width(RECORD_WIDTH))
            .child(views::TextView::new(encode_size(model.borrow().total_used))
                   .fixed_width(BYTES_WIDTH))
            .child(views::TextView::new(encode_size(model.borrow().total_retain))
                   .with_id("total_retain").fixed_width(BYTES_WIDTH))
            .child(views::TextView::new(if over { "*" } else { " " }).with_id("total_ok")));
    list.add_child(
        "filesystem",
        views::LinearLayout::horizontal()
            .child(views::DummyView{}.fixed_width(3))
            .child(views::DummyView{}.fixed_width(20))
            .child(views::TextView::new(encode_size(model.borrow().fs_capacity)).fixed_width(25)));
    let mut change_button = views::Button::new("Change", {
        let model = model.clone();
        move |siv| press_change(&model, siv)
    });
    change_button.set_enabled(!over);
    let mut buttons = views::LinearLayout::horizontal()
        .child(views::DummyView.full_width());
    buttons.add_child(change_button.with_id("change"));
    buttons.add_child(views::DummyView);
    buttons.add_child(views::Button::new("Cancel", |siv| siv.screen_mut().pop_layer()));
    siv.add_layer(
        views::Dialog::around(
            views::LinearLayout::vertical()
                .child(list)
                .child(views::DummyView)
                .child(buttons))
        .title("Edit retention"));
}
