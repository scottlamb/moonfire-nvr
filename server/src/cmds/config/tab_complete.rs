// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use std::sync::{Arc, Mutex};

use cursive::{
    direction::Direction,
    event::{Event, EventResult, Key},
    menu,
    view::CannotFocus,
    views::{self, EditView, MenuPopup},
    Printer, Rect, Vec2, View, With,
};

type TabCompleteFn = Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>;

pub struct TabCompleteEditView {
    edit_view: Arc<Mutex<EditView>>,
    tab_completer: Option<TabCompleteFn>,
}

impl TabCompleteEditView {
    pub fn new(edit_view: EditView) -> Self {
        Self {
            edit_view: Arc::new(Mutex::new(edit_view)),
            tab_completer: None,
        }
    }

    pub fn on_tab_complete(
        mut self,
        handler: impl Fn(&str) -> Vec<String> + Send + Sync + 'static,
    ) -> Self {
        self.tab_completer = Some(Arc::new(handler));
        self
    }

    pub fn get_content(&self) -> Arc<String> {
        self.edit_view.lock().unwrap().get_content()
    }
}

impl View for TabCompleteEditView {
    fn draw(&self, printer: &Printer) {
        self.edit_view.lock().unwrap().draw(printer)
    }

    fn layout(&mut self, size: Vec2) {
        self.edit_view.lock().unwrap().layout(size)
    }

    fn take_focus(&mut self, source: Direction) -> Result<EventResult, CannotFocus> {
        self.edit_view.lock().unwrap().take_focus(source)
    }

    fn on_event(&mut self, event: Event) -> EventResult {
        if !self.edit_view.lock().unwrap().is_enabled() {
            return EventResult::Ignored;
        }

        if let Event::Key(Key::Tab) = event {
            if let Some(tab_completer) = self.tab_completer.clone() {
                tab_complete(self.edit_view.clone(), tab_completer, true)
            } else {
                EventResult::consumed()
            }
        } else {
            self.edit_view.lock().unwrap().on_event(event)
        }
    }

    fn important_area(&self, view_size: Vec2) -> Rect {
        self.edit_view.lock().unwrap().important_area(view_size)
    }
}

fn tab_complete(
    edit_view: Arc<Mutex<EditView>>,
    tab_completer: TabCompleteFn,
    autofill_one: bool,
) -> EventResult {
    let completions = tab_completer(edit_view.lock().unwrap().get_content().as_str());
    EventResult::with_cb_once(move |siv| match *completions {
        [] => {}
        [ref completion] if autofill_one => edit_view.lock().unwrap().set_content(completion)(siv),
        [..] => {
            siv.add_layer(TabCompletePopup {
                popup: views::MenuPopup::new(Arc::new({
                    menu::Tree::new().with(|tree| {
                        for completion in completions {
                            let edit_view = edit_view.clone();
                            tree.add_leaf(completion.clone(), move |siv| {
                                edit_view.lock().unwrap().set_content(&completion)(siv)
                            })
                        }
                    })
                })),
                edit_view,
                tab_completer,
            });
        }
    })
}

struct TabCompletePopup {
    edit_view: Arc<Mutex<EditView>>,
    popup: MenuPopup,
    tab_completer: TabCompleteFn,
}
impl TabCompletePopup {
    fn forward_event_and_refresh(&self, event: Event) -> EventResult {
        let edit_view = self.edit_view.clone();
        let tab_completer = self.tab_completer.clone();
        EventResult::with_cb_once(move |s| {
            s.pop_layer();
            edit_view.lock().unwrap().on_event(event).process(s);
            tab_complete(edit_view, tab_completer, false).process(s);
        })
    }
}

impl View for TabCompletePopup {
    fn draw(&self, printer: &Printer) {
        self.popup.draw(printer)
    }

    fn required_size(&mut self, req: Vec2) -> Vec2 {
        self.popup.required_size(req)
    }

    fn on_event(&mut self, event: Event) -> EventResult {
        match self.popup.on_event(event.clone()) {
            EventResult::Ignored => match event {
                e @ (Event::Char(_) | Event::Key(Key::Backspace)) => {
                    self.forward_event_and_refresh(e)
                }
                Event::Key(Key::Tab) => self.popup.on_event(Event::Key(Key::Enter)),
                _ => EventResult::Ignored,
            },
            other => other,
        }
    }

    fn layout(&mut self, size: Vec2) {
        self.popup.layout(size)
    }

    fn important_area(&self, size: Vec2) -> Rect {
        self.popup.important_area(size)
    }
}
