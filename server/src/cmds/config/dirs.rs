// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2017 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use base::strutil::{decode_size, encode_size};
use cursive::traits::{Nameable, Resizable};
use cursive::views;
use cursive::Cursive;
use db::writer;
use failure::Error;
use log::{debug, trace};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

struct Stream {
    label: String,
    used: i64,
    record: bool,
    retain: Option<i64>, // None if unparseable
}

struct Model {
    db: Arc<db::Database>,
    dir_id: i32,
    fs_capacity: i64,
    total_used: i64,
    total_retain: i64,
    errors: isize,
    streams: BTreeMap<i32, Stream>,
}

/// Updates the limits in the database. Doesn't delete excess data (if any).
fn update_limits_inner(model: &Model) -> Result<(), Error> {
    let mut changes = Vec::with_capacity(model.streams.len());
    for (&stream_id, stream) in &model.streams {
        changes.push(db::RetentionChange {
            stream_id,
            new_record: stream.record,
            new_limit: stream.retain.unwrap(),
        });
    }
    model.db.lock().update_retention(&changes)
}

fn update_limits(model: &Model, siv: &mut Cursive) {
    if let Err(e) = update_limits_inner(model) {
        siv.add_layer(
            views::Dialog::text(format!("Unable to update limits: {e}"))
                .dismiss_button("Back")
                .title("Error"),
        );
    }
}

fn edit_limit(model: &RefCell<Model>, siv: &mut Cursive, id: i32, content: &str) {
    debug!("on_edit called for id {}", id);
    let mut model = model.borrow_mut();
    let model: &mut Model = &mut model;
    let stream = model.streams.get_mut(&id).unwrap();
    let new_value = decode_size(content).ok();
    let delta = new_value.unwrap_or(0) - stream.retain.unwrap_or(0);
    let old_errors = model.errors;
    if delta != 0 {
        let prev_over = model.total_retain > model.fs_capacity;
        model.total_retain += delta;
        siv.find_name::<views::TextView>("total_retain")
            .unwrap()
            .set_content(encode_size(model.total_retain));
        let now_over = model.total_retain > model.fs_capacity;
        if now_over != prev_over {
            model.errors += if now_over { 1 } else { -1 };
            siv.find_name::<views::TextView>("total_ok")
                .unwrap()
                .set_content(if now_over { "*" } else { " " });
        }
    }
    if new_value.is_none() != stream.retain.is_none() {
        model.errors += if new_value.is_none() { 1 } else { -1 };
        siv.find_name::<views::TextView>(&format!("{id}_ok"))
            .unwrap()
            .set_content(if new_value.is_none() { "*" } else { " " });
    }
    stream.retain = new_value;
    debug!("model.errors = {}", model.errors);
    if (model.errors == 0) != (old_errors == 0) {
        trace!("toggling change state: errors={}", model.errors);
        siv.find_name::<views::Button>("change")
            .unwrap()
            .set_enabled(model.errors == 0);
    }
}

fn edit_record(model: &RefCell<Model>, id: i32, record: bool) {
    let mut model = model.borrow_mut();
    let model: &mut Model = &mut model;
    let stream = model.streams.get_mut(&id).unwrap();
    stream.record = record;
}

fn confirm_deletion(model: &RefCell<Model>, siv: &mut Cursive, to_delete: i64) {
    let typed = siv
        .find_name::<views::EditView>("confirm")
        .unwrap()
        .get_content();
    debug!(
        "confirm, typed: {} vs expected: {}",
        typed.as_str(),
        to_delete
    );
    if decode_size(typed.as_str()).ok() == Some(to_delete) {
        actually_delete(model, siv);
    } else {
        siv.add_layer(
            views::Dialog::text("Please confirm amount.")
                .title("Try again")
                .dismiss_button("Back"),
        );
    }
}

fn actually_delete(model: &RefCell<Model>, siv: &mut Cursive) {
    let model = &*model.borrow();
    let new_limits: Vec<_> = model
        .streams
        .iter()
        .map(|(&id, s)| writer::NewLimit {
            stream_id: id,
            limit: s.retain.unwrap(),
        })
        .collect();
    siv.pop_layer(); // deletion confirmation
    siv.pop_layer(); // retention dialog
    {
        let mut l = model.db.lock();
        l.open_sample_file_dirs(&[model.dir_id]).unwrap(); // TODO: don't unwrap.
    }
    if let Err(e) = writer::lower_retention(&model.db, model.dir_id, &new_limits[..]) {
        siv.add_layer(
            views::Dialog::text(format!("Unable to delete excess video: {e}"))
                .title("Error")
                .dismiss_button("Abort"),
        );
    } else {
        update_limits(model, siv);
    }
}

fn press_change(model: &Rc<RefCell<Model>>, siv: &mut Cursive) {
    if model.borrow().errors > 0 {
        return;
    }
    let to_delete = model
        .borrow()
        .streams
        .values()
        .map(|s| ::std::cmp::max(s.used - s.retain.unwrap(), 0))
        .sum();
    debug!("change press, to_delete={}", to_delete);
    if to_delete > 0 {
        let prompt = format!(
            "Some streams' usage exceeds new limit. Please confirm the amount \
            of data to delete by typing it back:\n\n{}",
            encode_size(to_delete)
        );
        let dialog = views::Dialog::around(
            views::LinearLayout::vertical()
                .child(views::TextView::new(prompt))
                .child(views::DummyView)
                .child(
                    views::EditView::new()
                        .on_submit({
                            let model = model.clone();
                            move |siv, _| confirm_deletion(&model, siv, to_delete)
                        })
                        .with_name("confirm"),
                ),
        )
        .button("Confirm", {
            let model = model.clone();
            move |siv| confirm_deletion(&model, siv, to_delete)
        })
        .dismiss_button("Cancel")
        .title("Confirm deletion");
        siv.add_layer(dialog);
    } else {
        siv.pop_layer();
        update_limits(&model.borrow(), siv);
    }
}

pub fn top_dialog(db: &Arc<db::Database>, siv: &mut Cursive) {
    siv.add_layer(
        views::Dialog::around(
            views::SelectView::new()
                .on_submit({
                    let db = db.clone();
                    move |siv, item| match *item {
                        Some(d) => edit_dir_dialog(&db, siv, d),
                        None => add_dir_dialog(&db, siv),
                    }
                })
                .item("<new sample file dir>".to_string(), None)
                .with_all(
                    db.lock()
                        .sample_file_dirs_by_id()
                        .iter()
                        .map(|(&id, d)| (d.path.display().to_string(), Some(id))),
                )
                .full_width(),
        )
        .dismiss_button("Done")
        .title("Edit sample file directories"),
    );
}

fn add_dir_dialog(db: &Arc<db::Database>, siv: &mut Cursive) {
    siv.add_layer(
        views::Dialog::around(
            views::LinearLayout::vertical()
                .child(views::TextView::new("path"))
                .child(
                    views::EditView::new()
                        .on_submit({
                            let db = db.clone();
                            move |siv, path| add_dir(&db, siv, path.as_ref())
                        })
                        .with_name("path")
                        .fixed_width(60),
                ),
        )
        .button("Add", {
            let db = db.clone();
            move |siv| {
                let path = siv
                    .find_name::<views::EditView>("path")
                    .unwrap()
                    .get_content();
                add_dir(&db, siv, path.as_ref().as_ref())
            }
        })
        .button("Cancel", |siv| {
            siv.pop_layer();
        })
        .title("Add sample file directory"),
    );
}

fn add_dir(db: &Arc<db::Database>, siv: &mut Cursive, path: &Path) {
    if let Err(e) = db.lock().add_sample_file_dir(path.to_owned()) {
        siv.add_layer(
            views::Dialog::text(format!("Unable to add path {}: {}", path.display(), e))
                .dismiss_button("Back")
                .title("Error"),
        );
        return;
    }
    siv.pop_layer();

    // Recreate the edit dialog from scratch; it's easier than adding the new entry.
    siv.pop_layer();
    top_dialog(db, siv);
}

fn delete_dir_dialog(db: &Arc<db::Database>, siv: &mut Cursive, dir_id: i32) {
    siv.add_layer(
        views::Dialog::around(views::TextView::new("Empty (no associated streams)."))
            .button("Delete", {
                let db = db.clone();
                move |siv| delete_dir(&db, siv, dir_id)
            })
            .button("Cancel", |siv| {
                siv.pop_layer();
            })
            .title("Delete sample file directory"),
    );
}

fn delete_dir(db: &Arc<db::Database>, siv: &mut Cursive, dir_id: i32) {
    if let Err(e) = db.lock().delete_sample_file_dir(dir_id) {
        siv.add_layer(
            views::Dialog::text(format!("Unable to delete dir id {dir_id}: {e}"))
                .dismiss_button("Back")
                .title("Error"),
        );
        return;
    }
    siv.pop_layer();

    // Recreate the edit dialog from scratch; it's easier than adding the new entry.
    siv.pop_layer();
    top_dialog(db, siv);
}

fn edit_dir_dialog(db: &Arc<db::Database>, siv: &mut Cursive, dir_id: i32) {
    let path;
    let model = {
        let mut streams = BTreeMap::new();
        let mut total_used = 0;
        let mut total_retain = 0;
        let fs_capacity;
        {
            let mut l = db.lock();
            for (&id, s) in l.streams_by_id() {
                let c = l
                    .cameras_by_id()
                    .get(&s.camera_id)
                    .expect("stream without camera");
                if s.sample_file_dir_id != Some(dir_id) {
                    continue;
                }
                streams.insert(
                    id,
                    Stream {
                        label: format!("{}: {}: {}", id, c.short_name, s.type_.as_str()),
                        used: s.fs_bytes,
                        record: s.config.mode == db::json::STREAM_MODE_RECORD,
                        retain: Some(s.config.retain_bytes),
                    },
                );
                total_used += s.fs_bytes;
                total_retain += s.config.retain_bytes;
            }
            if streams.is_empty() {
                return delete_dir_dialog(db, siv, dir_id);
            }
            l.open_sample_file_dirs(&[dir_id]).unwrap(); // TODO: don't unwrap.
            let dir = l.sample_file_dirs_by_id().get(&dir_id).unwrap();
            let stat = dir.get().unwrap().statfs().unwrap();
            fs_capacity = stat.block_size() as i64 * stat.blocks_available() as i64 + total_used;
            path = dir.path.clone();
        }
        Rc::new(RefCell::new(Model {
            dir_id,
            db: db.clone(),
            fs_capacity,
            total_used,
            total_retain,
            errors: (total_retain > fs_capacity) as isize,
            streams,
        }))
    };

    const RECORD_WIDTH: usize = 8;
    const BYTES_WIDTH: usize = 22;

    let mut list = views::ListView::new();
    list.add_child(
        "stream",
        views::LinearLayout::horizontal()
            .child(views::TextView::new("record").fixed_width(RECORD_WIDTH))
            .child(views::TextView::new("usage").fixed_width(BYTES_WIDTH))
            .child(views::TextView::new("limit").fixed_width(BYTES_WIDTH)),
    );
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
                .child(
                    views::EditView::new()
                        .content(encode_size(stream.retain.unwrap()))
                        .on_edit({
                            let model = model.clone();
                            move |siv, content, _pos| edit_limit(&model, siv, id, content)
                        })
                        .on_submit({
                            let model = model.clone();
                            move |siv, _| press_change(&model, siv)
                        })
                        .fixed_width(20),
                )
                .child(
                    views::TextView::new("")
                        .with_name(format!("{id}_ok"))
                        .fixed_width(1),
                ),
        );
    }
    let over = model.borrow().total_retain > model.borrow().fs_capacity;
    list.add_child(
        "total",
        views::LinearLayout::horizontal()
            .child(views::DummyView {}.fixed_width(RECORD_WIDTH))
            .child(
                views::TextView::new(encode_size(model.borrow().total_used))
                    .fixed_width(BYTES_WIDTH),
            )
            .child(
                views::TextView::new(encode_size(model.borrow().total_retain))
                    .with_name("total_retain")
                    .fixed_width(BYTES_WIDTH),
            )
            .child(views::TextView::new(if over { "*" } else { " " }).with_name("total_ok")),
    );
    list.add_child(
        "filesystem",
        views::LinearLayout::horizontal()
            .child(views::DummyView {}.fixed_width(RECORD_WIDTH))
            .child(views::DummyView {}.fixed_width(BYTES_WIDTH))
            .child(
                views::TextView::new(encode_size(model.borrow().fs_capacity))
                    .fixed_width(BYTES_WIDTH),
            ),
    );
    let mut change_button = views::Button::new("Change", move |siv| press_change(&model, siv));
    change_button.set_enabled(!over);
    let mut buttons = views::LinearLayout::horizontal().child(views::DummyView.full_width());
    buttons.add_child(change_button.with_name("change"));
    buttons.add_child(views::DummyView);
    buttons.add_child(views::Button::new("Cancel", |siv| {
        siv.pop_layer();
    }));
    siv.add_layer(
        views::Dialog::around(
            views::LinearLayout::vertical()
                .child(list)
                .child(views::DummyView)
                .child(buttons),
        )
        .title(format!("Edit retention for {}", path.display())),
    );
}
