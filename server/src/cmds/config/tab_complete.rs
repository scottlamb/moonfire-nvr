use std::{cell::RefCell, rc::Rc};

use cursive::{
    direction::Direction,
    event::{Event, EventResult, Key},
    menu,
    view::CannotFocus,
    views::{self, EditView, MenuPopup},
    Printer, Rect, Vec2, View, With,
};

type TabCompleteFn = Rc<dyn Fn(&str) -> Vec<String>>;

pub struct TabCompleteEditView {
    edit_view: Rc<RefCell<EditView>>,
    tab_completer: Option<TabCompleteFn>,
}

impl TabCompleteEditView {
    pub fn new(edit_view: EditView) -> Self {
        Self {
            edit_view: Rc::new(RefCell::new(edit_view)),
            tab_completer: None,
        }
    }

    pub fn on_tab_complete(mut self, handler: impl Fn(&str) -> Vec<String> + 'static) -> Self {
        self.tab_completer = Some(Rc::new(handler));
        self
    }
}

impl View for TabCompleteEditView {
    fn draw(&self, printer: &Printer) {
        self.edit_view.borrow().draw(printer)
    }

    fn layout(&mut self, size: Vec2) {
        self.edit_view.borrow_mut().layout(size)
    }

    fn take_focus(&mut self, source: Direction) -> Result<EventResult, CannotFocus> {
        self.edit_view.borrow_mut().take_focus(source)
    }

    fn on_event(&mut self, event: Event) -> EventResult {
        if !self.edit_view.borrow().is_enabled() {
            return EventResult::Ignored;
        }

        if let Event::Key(Key::Tab) = event {
            if let Some(tab_completer) = self.tab_completer.clone() {
                tab_complete(self.edit_view.clone(), tab_completer, true)
            } else {
                EventResult::consumed()
            }
        } else {
            self.edit_view.borrow_mut().on_event(event)
        }
    }

    fn important_area(&self, view_size: Vec2) -> Rect {
        self.edit_view.borrow().important_area(view_size)
    }
}

fn tab_complete(
    edit_view: Rc<RefCell<EditView>>,
    tab_completer: TabCompleteFn,
    autofill_one: bool,
) -> EventResult {
    let completions = tab_completer(edit_view.borrow().get_content().as_str());
    EventResult::with_cb_once(move |siv| match *completions {
        [] => {}
        [ref completion] if autofill_one => edit_view.borrow_mut().set_content(completion)(siv),
        [..] => {
            siv.add_layer(TabCompletePopup {
                popup: views::MenuPopup::new(Rc::new({
                    menu::Tree::new().with(|tree| {
                        for completion in completions {
                            let edit_view = edit_view.clone();
                            tree.add_leaf(&completion.clone(), move |siv| 
                                edit_view.borrow_mut().set_content(&completion)(siv)
                            )
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
    edit_view: Rc<RefCell<EditView>>,
    popup: MenuPopup,
    tab_completer: TabCompleteFn,
}
impl TabCompletePopup {
    fn forward_event_and_refresh(&self, event: Event) -> EventResult {
        let edit_view = self.edit_view.clone();
        let tab_completer = self.tab_completer.clone();
        EventResult::with_cb_once(move |s| {
            s.pop_layer();
            edit_view.borrow_mut().on_event(event).process(s);
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
