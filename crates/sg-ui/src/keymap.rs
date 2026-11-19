//! The key map: one table, bound by physical key position.
//!
//! `egui::Key` values here come from the event's `physical_key`, which
//! egui-winit derives from the scancode and names after the US layout. On the
//! Swiss keyboard the cap `Y` (bottom left) is therefore `Key::Z` and the cap
//! `Z` is `Key::Y`; see docs/spec/07-ui-keybinds.md.

use egui::{Event, Key, Modifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    NewProject,
    CloseProject,
    Undo,
    Redo,
    Rename,
    AddAsset,
    PlayPause,
    StepBack,
    StepForward,
    ToStart,
    ToEnd,
    Split,
    /// Remove from the clip start to the playhead and close the gap.
    RippleStart,
    /// Remove from the playhead to the clip end and close the gap.
    RippleEnd,
    RippleDelete,
    /// Select the clip under the playhead; again for the next track up.
    SelectUnder,
    Copy,
    PasteClips,
    Marker,
    PrevCut,
    NextCut,
    /// Open the export panel.
    Export,
    ExportAgain,
    /// Export the selection or marked range as a GIF to the clipboard.
    ExportGif,
    /// Name the current state (a git tag).
    Checkpoint,
    /// Cut the programme to camera angle 1 to 9 at the playhead.
    Angle(u8),
    /// Put a transition on the nearest cut, or remove the one there.
    Transition,
    /// Show or hide the colour panel.
    Colour,
    /// A/B: switch the look chain, or the match, of the shown scenario on and off.
    ToggleLook,
    ToggleMatch,
    /// Show or hide the list of keys.
    Help,
    /// Show or hide the mixer.
    Mixer,
    /// Show saved states and checkpoints.
    History,
    /// Fix the selected clip's placement at the playhead.
    KeyframeSet,
    KeyframeRemove,
    AddTitle,
    /// Play and record drags in the viewer as motion.
    LiveRecord,
    /// Put the selected asset on the timeline.
    InsertAsset,
    /// Import what the clipboard holds.
    Paste,
    /// Remove the selected item.
    Delete,
    TogglePerf,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Any,
    Library,
    Editor,
}

pub struct Binding {
    pub scope: Scope,
    pub ctrl: bool,
    pub shift: bool,
    pub key: Key,
    pub action: Action,
}

const fn bind(scope: Scope, ctrl: bool, shift: bool, key: Key, action: Action) -> Binding {
    Binding { scope, ctrl, shift, key, action }
}

pub const KEYMAP: &[Binding] = &[
    bind(Scope::Any, false, false, Key::Escape, Action::Cancel),
    bind(Scope::Any, false, false, Key::Backtick, Action::TogglePerf),
    // Also with Ctrl, so the overlay is reachable while a text field has focus.
    bind(Scope::Any, true, false, Key::Backtick, Action::TogglePerf),
    bind(Scope::Library, true, false, Key::N, Action::NewProject),
    // Swiss cap Y.
    bind(Scope::Editor, false, false, Key::Z, Action::Undo),
    bind(Scope::Editor, false, true, Key::Z, Action::Redo),
    // Swiss cap Z: the habitual Ctrl+Z.
    bind(Scope::Editor, true, false, Key::Y, Action::Undo),
    bind(Scope::Editor, true, true, Key::Y, Action::Redo),
    bind(Scope::Editor, true, false, Key::W, Action::CloseProject),
    bind(Scope::Editor, false, false, Key::F2, Action::Rename),
    bind(Scope::Editor, true, false, Key::Q, Action::AddAsset),
    bind(Scope::Editor, false, false, Key::Space, Action::PlayPause),
    bind(Scope::Editor, false, false, Key::A, Action::StepBack),
    bind(Scope::Editor, false, false, Key::D, Action::StepForward),
    bind(Scope::Editor, true, false, Key::A, Action::ToStart),
    bind(Scope::Editor, true, false, Key::D, Action::ToEnd),
    bind(Scope::Editor, false, false, Key::Enter, Action::InsertAsset),
    bind(Scope::Editor, false, false, Key::S, Action::Split),
    bind(Scope::Editor, false, false, Key::Q, Action::RippleStart),
    bind(Scope::Editor, false, false, Key::E, Action::RippleEnd),
    bind(Scope::Editor, false, false, Key::X, Action::RippleDelete),
    bind(Scope::Editor, false, true, Key::X, Action::Delete),
    bind(Scope::Editor, false, false, Key::W, Action::SelectUnder),
    bind(Scope::Editor, false, false, Key::C, Action::Copy),
    bind(Scope::Editor, true, false, Key::C, Action::Copy),
    bind(Scope::Editor, false, false, Key::V, Action::PasteClips),
    bind(Scope::Editor, false, false, Key::G, Action::Marker),
    bind(Scope::Editor, false, true, Key::A, Action::PrevCut),
    bind(Scope::Editor, false, true, Key::D, Action::NextCut),
    bind(Scope::Editor, true, false, Key::S, Action::Checkpoint),
    bind(Scope::Editor, true, false, Key::H, Action::History),
    bind(Scope::Editor, true, false, Key::E, Action::Export),
    bind(Scope::Editor, true, true, Key::E, Action::ExportAgain),
    bind(Scope::Editor, true, false, Key::G, Action::ExportGif),
    bind(Scope::Editor, false, false, Key::F, Action::KeyframeSet),
    bind(Scope::Editor, false, true, Key::F, Action::KeyframeRemove),
    bind(Scope::Editor, false, false, Key::T, Action::AddTitle),
    bind(Scope::Editor, false, false, Key::M, Action::Mixer),
    bind(Scope::Any, false, false, Key::F1, Action::Help),
    bind(Scope::Editor, false, false, Key::K, Action::Colour),
    bind(Scope::Editor, false, false, Key::J, Action::ToggleLook),
    bind(Scope::Editor, false, true, Key::J, Action::ToggleMatch),
    bind(Scope::Editor, false, false, Key::B, Action::Transition),
    bind(Scope::Editor, false, false, Key::Num1, Action::Angle(1)),
    bind(Scope::Editor, false, false, Key::Num2, Action::Angle(2)),
    bind(Scope::Editor, false, false, Key::Num3, Action::Angle(3)),
    bind(Scope::Editor, false, false, Key::Num4, Action::Angle(4)),
    bind(Scope::Editor, false, false, Key::Num5, Action::Angle(5)),
    bind(Scope::Editor, false, false, Key::Num6, Action::Angle(6)),
    bind(Scope::Editor, false, false, Key::Num7, Action::Angle(7)),
    bind(Scope::Editor, false, false, Key::Num8, Action::Angle(8)),
    bind(Scope::Editor, false, false, Key::Num9, Action::Angle(9)),
    bind(Scope::Editor, false, false, Key::R, Action::LiveRecord),
    bind(Scope::Editor, true, false, Key::V, Action::Paste),
    bind(Scope::Editor, false, false, Key::Delete, Action::Delete),
];

/// The action bound to a key press. While a text field has focus, keys
/// without Ctrl belong to the text (except Escape).
pub fn lookup(screen: Scope, typing: bool, key: Key, mods: Modifiers) -> Option<Action> {
    if mods.alt || (typing && !mods.ctrl && key != Key::Escape) {
        return None;
    }
    KEYMAP
        .iter()
        .find(|b| {
            (b.scope == Scope::Any || b.scope == screen) && b.key == key && b.ctrl == mods.ctrl && b.shift == mods.shift
        })
        .map(|b| b.action)
        // In a text field Ctrl+V pastes text into the field, as everywhere.
        .filter(|action| !(typing && *action == Action::Paste))
}

/// Removes bound key presses from this frame's input and returns their
/// actions, so that widgets do not handle the same key again.
pub fn take_actions(ctx: &egui::Context, screen: Scope) -> Vec<Action> {
    let typing = ctx.text_edit_focused();
    ctx.input_mut(|input| {
        let mut actions = Vec::new();
        input.events.retain(|event| {
            let Event::Key { key, physical_key, pressed: true, modifiers, .. } = event else { return true };
            match lookup(screen, typing, physical_key.unwrap_or(*key), *modifiers) {
                Some(action) => {
                    actions.push(action);
                    false
                }
                None => true,
            }
        });
        actions
    })
}

/// What an action does, in a few words, and the group it is listed under
/// in the key reference. `None`: not listed (one line stands for several).
pub fn describe(action: Action) -> Option<(&'static str, &'static str)> {
    Some(match action {
        Action::PlayPause => ("Playing", "play / pause"),
        Action::StepBack => ("Playing", "one frame back"),
        Action::StepForward => ("Playing", "one frame forward"),
        Action::PrevCut => ("Playing", "to the previous cut or marker"),
        Action::NextCut => ("Playing", "to the next cut or marker"),
        Action::ToStart => ("Playing", "to the start"),
        Action::ToEnd => ("Playing", "to the end"),
        Action::Split => ("Cutting", "split at the playhead"),
        Action::RippleStart => ("Cutting", "remove from the clip's start to the playhead"),
        Action::RippleEnd => ("Cutting", "remove from the playhead to the clip's end"),
        Action::RippleDelete => ("Cutting", "delete the selection and close the gap"),
        Action::Delete => ("Cutting", "delete, leave the gap (or remove the selected asset)"),
        Action::SelectUnder => ("Cutting", "select the clip under the playhead (again: next track)"),
        Action::Copy => ("Cutting", "copy the selected clips"),
        Action::PasteClips => ("Cutting", "paste them at the playhead"),
        Action::Marker => ("Cutting", "marker at the playhead (again: remove it)"),
        Action::Undo => ("Cutting", "undo"),
        Action::Redo => ("Cutting", "redo"),
        Action::InsertAsset => ("Making", "put the selected asset on the timeline"),
        Action::AddTitle => ("Making", "add a title"),
        Action::Transition => ("Making", "whip-pan on the nearest cut (again: remove it)"),
        Action::KeyframeSet => ("Making", "keyframe for the selected clip at the playhead"),
        Action::KeyframeRemove => ("Making", "remove that keyframe"),
        Action::LiveRecord => ("Making", "record the picture's movement while playing"),
        Action::Angle(1) => ("Making", "cut to camera angle 1 (2 to 9 likewise)"),
        Action::Angle(_) => return None,
        Action::AddAsset => ("Project", "add an asset: link or files"),
        Action::Paste => ("Project", "paste: a link, file or image is imported"),
        Action::Mixer => ("Panels", "mixer"),
        Action::Colour => ("Panels", "colour: scenarios, match and look"),
        Action::ToggleLook => ("Panels", "look of the shown scenario off / on"),
        Action::ToggleMatch => ("Panels", "match of the shown scenario off / on"),
        Action::History => ("Panels", "saved states and checkpoints"),
        Action::Help => ("Panels", "this list"),
        Action::TogglePerf => ("Panels", "performance numbers"),
        Action::Export => ("Output", "export panel"),
        Action::ExportAgain => ("Output", "the last export again"),
        Action::ExportGif => ("Output", "selection or marked range as a GIF to the clipboard"),
        Action::Checkpoint => ("Project", "name the current state"),
        Action::Rename => ("Project", "rename the project"),
        Action::CloseProject => ("Project", "close the project"),
        Action::NewProject => ("Project", "new project named as typed (in the library)"),
        Action::Cancel => ("Project", "close what is open, dismiss a message"),
    })
}

/// The key as its cap reads on the Swiss German keyboard the map is laid
/// out for (the map itself is by position: 07-ui-keybinds.md).
pub fn cap(binding: &Binding) -> String {
    let key = match binding.key {
        Key::Z => "Y".to_owned(),
        Key::Y => "Z".to_owned(),
        Key::Backtick => "\u{a7}".to_owned(),
        Key::Num1 => "1".to_owned(),
        Key::Space => "Space".to_owned(),
        Key::Escape => "Esc".to_owned(),
        Key::Delete => "Del".to_owned(),
        other => other.name().to_owned(),
    };
    format!("{}{}{key}", if binding.ctrl { "Ctrl+" } else { "" }, if binding.shift { "Shift+" } else { "" })
}

/// The key reference: every listed action once, with all its keys, by group.
pub fn reference() -> Vec<(&'static str, Vec<(String, &'static str)>)> {
    let mut groups: Vec<(&'static str, Vec<(String, &'static str)>)> = Vec::new();
    for order in ["Playing", "Cutting", "Making", "Panels", "Output", "Project"] {
        let mut lines: Vec<(String, &'static str)> = Vec::new();
        for binding in KEYMAP {
            let Some((group, what)) = describe(binding.action) else { continue };
            if group != order {
                continue;
            }
            match lines.iter_mut().find(|(_, w)| *w == what) {
                Some((keys, _)) => {
                    keys.push_str(", ");
                    keys.push_str(&cap(binding));
                }
                None => lines.push((cap(binding), what)),
            }
        }
        groups.push((order, lines));
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: Modifiers = Modifiers { alt: false, ctrl: true, shift: false, mac_cmd: false, command: true };
    const SHIFT: Modifiers = Modifiers { alt: false, ctrl: false, shift: true, mac_cmd: false, command: false };

    #[test]
    fn swiss_y_is_undo() {
        // The Swiss Y cap sends the scancode of US Z.
        assert_eq!(lookup(Scope::Editor, false, Key::Z, Modifiers::NONE), Some(Action::Undo));
        assert_eq!(lookup(Scope::Editor, false, Key::Z, SHIFT), Some(Action::Redo));
        assert_eq!(lookup(Scope::Editor, false, Key::Y, CTRL), Some(Action::Undo));
        assert_eq!(lookup(Scope::Editor, false, Key::Y, Modifiers::NONE), None);
    }

    #[test]
    fn plain_keys_go_to_the_text_field_while_typing() {
        assert_eq!(lookup(Scope::Editor, true, Key::Z, Modifiers::NONE), None);
        assert_eq!(lookup(Scope::Editor, true, Key::Backtick, Modifiers::NONE), None);
        assert_eq!(lookup(Scope::Editor, true, Key::Backtick, CTRL), Some(Action::TogglePerf));
        assert_eq!(lookup(Scope::Editor, true, Key::W, CTRL), Some(Action::CloseProject));
        assert_eq!(lookup(Scope::Library, true, Key::Escape, Modifiers::NONE), Some(Action::Cancel));
    }

    #[test]
    fn scopes_do_not_leak() {
        assert_eq!(lookup(Scope::Library, false, Key::Z, Modifiers::NONE), None);
        assert_eq!(lookup(Scope::Library, true, Key::N, CTRL), Some(Action::NewProject));
        assert_eq!(lookup(Scope::Editor, false, Key::N, CTRL), None);
    }

    #[test]
    fn no_two_bindings_collide() {
        for (i, a) in KEYMAP.iter().enumerate() {
            for b in &KEYMAP[i + 1..] {
                let overlap = a.scope == b.scope || a.scope == Scope::Any || b.scope == Scope::Any;
                assert!(
                    !(overlap && a.key == b.key && a.ctrl == b.ctrl && a.shift == b.shift),
                    "{:?} and {:?} share a key",
                    a.action,
                    b.action
                );
            }
        }
    }
}
