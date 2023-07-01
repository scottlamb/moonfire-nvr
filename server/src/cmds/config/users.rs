// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2017 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use cursive::traits::{Nameable, Resizable};
use cursive::views;
use cursive::Cursive;
use log::info;
use std::sync::Arc;

/// Builds a `UserChange` from an active `edit_user_dialog`.
fn get_change(
    siv: &mut Cursive,
    db: &db::LockedDatabase,
    id: Option<i32>,
    pw: PasswordChange,
) -> db::UserChange {
    let mut change = match id {
        Some(id) => db.users_by_id().get(&id).unwrap().change(),
        None => db::UserChange::add_user(String::new()),
    };
    change.username.clear();
    change.username += siv
        .find_name::<views::EditView>("username")
        .unwrap()
        .get_content()
        .as_str();
    match pw {
        PasswordChange::Leave => {}
        PasswordChange::Set => {
            let pwd = siv
                .find_name::<views::EditView>("new_pw")
                .unwrap()
                .get_content();
            change.set_password(pwd.as_str().into());
        }
        PasswordChange::Clear => change.clear_password(),
    };
    for (id, ref mut b) in &mut [
        ("perm_view_video", &mut change.permissions.view_video),
        (
            "perm_read_camera_configs",
            &mut change.permissions.read_camera_configs,
        ),
        (
            "perm_update_signals",
            &mut change.permissions.update_signals,
        ),
    ] {
        **b = siv.find_name::<views::Checkbox>(id).unwrap().is_checked();
        info!("{}: {}", id, **b);
    }
    change
}

fn press_edit(siv: &mut Cursive, db: &Arc<db::Database>, id: Option<i32>, pw: PasswordChange) {
    let result = {
        let mut l = db.lock();
        let c = get_change(siv, &l, id, pw);
        l.apply_user_change(c).map(|_| ())
    };
    if let Err(e) = result {
        siv.add_layer(
            views::Dialog::text(format!("Unable to apply change: {e}"))
                .title("Error")
                .dismiss_button("Abort"),
        );
    } else {
        siv.pop_layer(); // get rid of the add/edit user dialog.

        // Recreate the "Edit users" dialog from scratch; it's easier than adding the new entry.
        siv.pop_layer();
        top_dialog(db, siv);
    }
}

fn press_delete(siv: &mut Cursive, db: &Arc<db::Database>, id: i32, name: String) {
    siv.add_layer(
        views::Dialog::text(format!("Delete user {name}?"))
            .button("Delete", {
                let db = db.clone();
                move |s| actually_delete(s, &db, id)
            })
            .title("Delete user")
            .dismiss_button("Cancel"),
    );
}

fn actually_delete(siv: &mut Cursive, db: &Arc<db::Database>, id: i32) {
    siv.pop_layer(); // get rid of the add/edit user dialog.
    let result = {
        let mut l = db.lock();
        l.delete_user(id)
    };
    if let Err(e) = result {
        siv.add_layer(
            views::Dialog::text(format!("Unable to delete user: {e}"))
                .title("Error")
                .dismiss_button("Abort"),
        );
    } else {
        // Recreate the "Edit users" dialog from scratch; it's easier than adding the new entry.
        siv.pop_layer();
        top_dialog(db, siv);
    }
}

#[derive(Copy, Clone)]
enum PasswordChange {
    Leave,
    Clear,
    Set,
}

fn select_set(siv: &mut Cursive) {
    siv.find_name::<views::RadioButton<PasswordChange>>("pw_set")
        .unwrap()
        .select();
}

/// Adds or updates a user.
/// (The former if `item` is None; the latter otherwise.)
fn edit_user_dialog(db: &Arc<db::Database>, siv: &mut Cursive, item: Option<i32>) {
    let (username, id_str, has_password, permissions);
    let mut pw_group = views::RadioGroup::new();
    {
        let l = db.lock();
        let u = item.map(|id| l.users_by_id().get(&id).unwrap());
        username = u.map(|u| u.username.clone()).unwrap_or_default();
        id_str = item.map_or_else(|| "<new>".to_string(), |id| id.to_string());
        has_password = u.map(|u| u.has_password()).unwrap_or(false);
        permissions = u.map(|u| u.permissions.clone()).unwrap_or_default();
    }
    let top_list = views::ListView::new()
        .child("id", views::TextView::new(id_str))
        .child(
            "username",
            views::EditView::new()
                .content(&username)
                .with_name("username"),
        );
    let mut layout = views::LinearLayout::vertical()
        .child(top_list)
        .child(views::DummyView)
        .child(views::TextView::new("password"));

    if has_password {
        layout.add_child(pw_group.button(PasswordChange::Leave, "Leave set"));
        layout.add_child(pw_group.button(PasswordChange::Clear, "Clear"));
        layout.add_child(
            views::LinearLayout::horizontal()
                .child(
                    pw_group
                        .button(PasswordChange::Set, "Set to:")
                        .with_name("pw_set"),
                )
                .child(views::DummyView)
                .child(
                    views::EditView::new()
                        .on_edit(|siv, _, _| select_set(siv))
                        .with_name("new_pw")
                        .full_width(),
                ),
        );
    } else {
        layout.add_child(pw_group.button(PasswordChange::Leave, "Leave unset"));
        layout.add_child(
            views::LinearLayout::horizontal()
                .child(
                    pw_group
                        .button(PasswordChange::Set, "Reset to:")
                        .with_name("pw_set"),
                )
                .child(views::DummyView)
                .child(
                    views::EditView::new()
                        .on_edit(|siv, _, _| select_set(siv))
                        .with_name("new_pw")
                        .full_width(),
                ),
        );
    }

    layout.add_child(views::DummyView);
    layout.add_child(views::TextView::new("permissions"));
    let mut perms = views::ListView::new();
    for (name, b) in &[
        ("view_video", permissions.view_video),
        ("read_camera_configs", permissions.read_camera_configs),
        ("update_signals", permissions.update_signals),
    ] {
        let mut checkbox = views::Checkbox::new();
        checkbox.set_checked(*b);
        perms.add_child(name, checkbox.with_name(format!("perm_{name}")));
    }
    layout.add_child(perms);

    let dialog = views::Dialog::around(layout);
    let dialog = if let Some(id) = item {
        dialog
            .title("Edit user")
            .button("Edit", {
                let db = db.clone();
                move |s| press_edit(s, &db, item, *pw_group.selection())
            })
            .button("Delete", {
                let db = db.clone();
                move |s| press_delete(s, &db, id, username.clone())
            })
    } else {
        dialog.title("Add user").button("Add", {
            let db = db.clone();
            move |s| press_edit(s, &db, item, *pw_group.selection())
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
                    move |siv, &item| edit_user_dialog(&db, siv, item)
                })
                .item("<new user>".to_string(), None)
                .with_all(
                    db.lock()
                        .users_by_id()
                        .iter()
                        .map(|(&id, user)| (format!("{}: {}", id, user.username), Some(id))),
                )
                .full_width(),
        )
        .dismiss_button("Done")
        .title("Edit users"),
    );
}
