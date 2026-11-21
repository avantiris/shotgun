//! Buttons and menu entries for what the keys do (feedback F4.1): every
//! one runs the same action as its key and shows that key, so that the
//! keys are learnt by using the buttons.

use egui::RichText;

use crate::keymap::{self, Action};
use crate::theme;

fn widget(action: Action, label: &str) -> egui::Button<'static> {
    let button = egui::Button::new(label.to_owned());
    match keymap::hint(action) {
        key if key.is_empty() => button,
        key => button.shortcut_text(RichText::new(key).size(10.5).color(theme::DIM)),
    }
}

/// What the action does, as a sentence for a tooltip.
fn tip(action: Action) -> Option<String> {
    let (_, what) = keymap::describe(action)?;
    let mut text = sentence(what);
    let key = keymap::hint(action);
    if !key.is_empty() {
        text.push_str(&format!(" ({key})"));
    }
    Some(text)
}

/// The descriptions of the key reference start in lower case.
fn sentence(what: &str) -> String {
    let mut letters = what.chars();
    letters.next().map(|first| first.to_uppercase().chain(letters).collect()).unwrap_or_default()
}

/// A button for an action, with its key on it. Returns whether it was clicked.
pub fn button(ui: &mut egui::Ui, action: Action, label: &str) -> bool {
    enabled_button(ui, true, action, label)
}

/// The same, grey while the action has nothing to act on.
pub fn enabled_button(ui: &mut egui::Ui, enabled: bool, action: Action, label: &str) -> bool {
    let response = ui.add_enabled(enabled, widget(action, label));
    // Named by what it does alone, without the key drawn beside it.
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    match tip(action) {
        Some(tip) => response.on_hover_text(tip).clicked(),
        None => response.clicked(),
    }
}

/// The same in a menu: choosing it closes the menu.
pub fn entry(ui: &mut egui::Ui, action: Action, label: &str) -> bool {
    enabled_entry(ui, true, action, label)
}

/// A menu entry that is grey while the action has nothing to act on.
pub fn enabled_entry(ui: &mut egui::Ui, enabled: bool, action: Action, label: &str) -> bool {
    let response = ui.add_enabled(enabled, widget(action, label));
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    let response = match tip(action) {
        Some(tip) => response.on_hover_text(tip),
        None => response,
    };
    let clicked = response.clicked();
    if clicked {
        ui.close();
    }
    clicked
}
