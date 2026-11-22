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
    /// Select every clip of the sequence.
    SelectAll,
    /// Make a group of the selected clips, or dissolve the group they are.
    Group,
    Copy,
    /// Copy the selected clips and take them out, leaving the gap.
    Cut,
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
    /// The same with a dip to black.
    Blackout,
    /// Move the selected clips by this many frames (in the piano roll:
    /// the selected notes by a sixteenth, or by a beat for ten).
    Nudge(i8),
    /// Move the selected notes of the piano roll up or down by this many semitones.
    Pitch(i8),
    /// Show the whole sequence in the timeline.
    FitTimeline,
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
    /// Play and record sound from the input onto the timeline; again: stop.
    RecordSound,
    /// The metronome on or off.
    Metronome,
    /// Play a stretch over and over: the open note clip, the selected
    /// clips, or what lies between the markers around the playhead.
    Loop,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    // (Ctrl+A was "to the start" until the user asked for select all, F8.1.)
    bind(Scope::Editor, true, false, Key::A, Action::SelectAll),
    bind(Scope::Editor, false, false, Key::Home, Action::ToStart),
    bind(Scope::Editor, false, false, Key::End, Action::ToEnd),
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
    bind(Scope::Editor, true, false, Key::X, Action::Cut),
    bind(Scope::Editor, false, false, Key::V, Action::PasteClips),
    bind(Scope::Editor, false, false, Key::G, Action::Marker),
    bind(Scope::Editor, false, true, Key::G, Action::Group),
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
    bind(Scope::Editor, false, true, Key::B, Action::Blackout),
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
    bind(Scope::Editor, false, true, Key::R, Action::RecordSound),
    bind(Scope::Editor, false, true, Key::K, Action::Metronome),
    bind(Scope::Editor, false, false, Key::L, Action::Loop),
    bind(Scope::Editor, false, false, Key::H, Action::FitTimeline),
    bind(Scope::Editor, false, false, Key::ArrowLeft, Action::Nudge(-1)),
    bind(Scope::Editor, false, false, Key::ArrowRight, Action::Nudge(1)),
    bind(Scope::Editor, false, true, Key::ArrowLeft, Action::Nudge(-10)),
    bind(Scope::Editor, false, true, Key::ArrowRight, Action::Nudge(10)),
    bind(Scope::Editor, false, false, Key::ArrowUp, Action::Pitch(1)),
    bind(Scope::Editor, false, false, Key::ArrowDown, Action::Pitch(-1)),
    bind(Scope::Editor, false, true, Key::ArrowUp, Action::Pitch(12)),
    bind(Scope::Editor, false, true, Key::ArrowDown, Action::Pitch(-12)),
    bind(Scope::Editor, true, false, Key::V, Action::Paste),
    bind(Scope::Editor, false, false, Key::Backspace, Action::Delete),
    bind(Scope::Editor, false, false, Key::Delete, Action::Delete),
];

/// The map in use, once the user's changes have been read: the table above
/// until then (and in tests).
static ACTIVE: std::sync::OnceLock<Changed> = std::sync::OnceLock::new();

/// The table with the user's changes applied.
pub struct Changed {
    pub bindings: Vec<Binding>,
    /// Lines of the file that were not understood, as messages.
    pub problems: Vec<String>,
    /// Where the changes were read from, for the key reference.
    pub file: String,
}

pub fn active() -> &'static [Binding] {
    ACTIVE.get().map_or(KEYMAP, |changed| &changed.bindings)
}

/// The file the changes came from and what was wrong in it, if a file was read.
pub fn source() -> Option<(&'static str, &'static [String])> {
    ACTIVE.get().map(|changed| (changed.file.as_str(), changed.problems.as_slice()))
}

/// Puts the user's map in force. Once, at start.
pub fn install(changed: Changed) {
    let _ = ACTIVE.set(changed);
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Any => "any",
        Scope::Library => "library",
        Scope::Editor => "editor",
    }
}

/// The key as the file names it: by position, with the names of the US layout.
fn chord(binding: &Binding) -> String {
    format!(
        "{}{}{}",
        if binding.ctrl { "Ctrl+" } else { "" },
        if binding.shift { "Shift+" } else { "" },
        binding.key.name()
    )
}

/// The file as it is first written: every key of the table as a comment,
/// to be changed by removing the `#` and editing the line.
pub fn default_file() -> String {
    let mut text = String::from(
        "# Shotgun keys. Read when the program starts.\n\
         #\n\
         # Below is every key as built in, switched off by the '#'. To change one,\n\
         # remove the '#' and edit the line:\n\
         #\n\
         #   <where> <key> = <action>      where: any, library or editor\n\
         #   none = <action>               the action gets no key at all\n\
         #\n\
         # An action named here has only the keys given here; a key given here is\n\
         # taken away from whatever had it. Keys are named by their place on the\n\
         # keyboard, with the names of the US layout: on the Swiss keyboard the cap\n\
         # Y is called Z and the cap Z is called Y, and the key left of 1 is\n\
         # Backtick. Modifiers: Ctrl+ and Shift+ (Alt is kept for the mouse).\n\
         # F1 in the program shows the keys in force and what this file got wrong.\n\n",
    );
    for binding in KEYMAP {
        text.push_str(&format!("# {} {} = {:?}\n", scope_name(binding.scope), chord(binding), binding.action));
    }
    text
}

/// The table with the changes of a key file applied.
pub fn with_changes(text: &str, file: &str) -> Changed {
    let mut problems = Vec::new();
    let mut added: Vec<Binding> = Vec::new();
    let mut named: Vec<Action> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut problem = |what: &str| problems.push(format!("line {}: {what}", number + 1));
        let Some((keys, action)) = line.split_once('=') else {
            problem("no '='");
            continue;
        };
        let action = action.trim();
        let Some(action) =
            KEYMAP.iter().map(|b| b.action).find(|known| format!("{known:?}").eq_ignore_ascii_case(action))
        else {
            problem(&format!("no action is called '{action}'"));
            continue;
        };
        let mut words = keys.split_whitespace();
        let (first, second) = (words.next().unwrap_or_default(), words.next());
        if first.eq_ignore_ascii_case("none") && second.is_none() {
            named.push(action);
            continue;
        }
        let scope = [Scope::Any, Scope::Library, Scope::Editor]
            .into_iter()
            .find(|s| first.eq_ignore_ascii_case(scope_name(*s)));
        let (Some(scope), Some(keys), None) = (scope, second, words.next()) else {
            problem("expected 'any', 'library' or 'editor', then one key");
            continue;
        };
        let mut parts: Vec<&str> = keys.split('+').collect();
        let name = parts.pop().unwrap_or_default();
        let ctrl = parts.iter().any(|p| p.eq_ignore_ascii_case("ctrl"));
        let shift = parts.iter().any(|p| p.eq_ignore_ascii_case("shift"));
        if parts.iter().any(|p| !p.eq_ignore_ascii_case("ctrl") && !p.eq_ignore_ascii_case("shift")) {
            problem("only Ctrl+ and Shift+ can go before a key");
            continue;
        }
        // (Names are case-sensitive in egui: "A", "F2", "Space".)
        let key = Key::from_name(name)
            .or_else(|| Key::from_name(&name.to_uppercase()))
            .or_else(|| Key::ALL.iter().copied().find(|k| k.name().eq_ignore_ascii_case(name)));
        let Some(key) = key else {
            problem(&format!("no key is called '{name}'"));
            continue;
        };
        named.push(action);
        added.push(Binding { scope, ctrl, shift, key, action });
    }
    let collide = |a: &Binding, b: &Binding| {
        (a.scope == b.scope || a.scope == Scope::Any || b.scope == Scope::Any)
            && a.key == b.key
            && a.ctrl == b.ctrl
            && a.shift == b.shift
    };
    // The file's keys first, then what is left of the table.
    let mut bindings: Vec<Binding> = Vec::new();
    for binding in added {
        match bindings.iter().find(|b| collide(b, &binding)) {
            Some(taken) => problems.push(format!(
                "{} is given to both {:?} and {:?}; the first stays",
                chord(&binding),
                taken.action,
                binding.action
            )),
            None => bindings.push(binding),
        }
    }
    let from_file = bindings.len();
    for binding in KEYMAP {
        if !named.contains(&binding.action) && !bindings[..from_file].iter().any(|b| collide(b, binding)) {
            bindings.push(*binding);
        }
    }
    Changed { bindings, problems, file: file.to_owned() }
}

/// The action bound to a key press. While a text field has focus, keys
/// without Ctrl belong to the text (except Escape).
pub fn lookup(screen: Scope, typing: bool, key: Key, mods: Modifiers) -> Option<Action> {
    if mods.alt || (typing && !mods.ctrl && key != Key::Escape) {
        return None;
    }
    active()
        .iter()
        .find(|b| {
            (b.scope == Scope::Any || b.scope == screen) && b.key == key && b.ctrl == mods.ctrl && b.shift == mods.shift
        })
        .map(|b| b.action)
        // In a text field Ctrl+C, X and V belong to the text, as everywhere.
        .filter(|action| !(typing && matches!(action, Action::Paste | Action::Copy | Action::Cut)))
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
                    // Escape also reaches a text field that has the cursor, which lets go of it.
                    typing && action == Action::Cancel
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
        Action::SelectAll => ("Cutting", "select every clip (or drag a box over empty timeline)"),
        Action::Group => ("Cutting", "group the selected clips: a click on one selects all (again: ungroup)"),
        Action::Copy => ("Cutting", "copy the selected clips"),
        Action::Cut => ("Cutting", "cut the selected clips: copy them and take them out"),
        Action::PasteClips => ("Cutting", "paste them at the playhead"),
        Action::Marker => ("Cutting", "marker at the playhead (again: remove it)"),
        Action::Nudge(1) => ("Cutting", "selected clips one frame later (left: earlier; Shift: ten frames)"),
        Action::Nudge(_) => return None,
        Action::Pitch(1) => (
            "Making",
            "in the piano roll: the selected notes a semitone up (down: lower; Shift: an octave; left and right: a sixteenth, Shift: a beat)",
        ),
        Action::Pitch(_) => return None,
        Action::Undo => ("Cutting", "undo"),
        Action::Redo => ("Cutting", "redo"),
        Action::InsertAsset => ("Making", "put the selected asset on the timeline"),
        Action::AddTitle => ("Making", "add a title"),
        Action::Transition => ("Making", "whip-pan on the cuts of the selected clips (again: remove it)"),
        Action::Blackout => {
            ("Making", "black between the selected clips: a hard cut to black and back (again: remove it)")
        }
        Action::KeyframeSet => ("Making", "keyframe for the selected clip at the playhead"),
        Action::KeyframeRemove => ("Making", "remove that keyframe"),
        Action::LiveRecord => ("Making", "record the picture's movement while playing"),
        Action::Metronome => {
            ("Making", "metronome: clicks on the beats while playing, and a bar of them before a recording")
        }
        Action::Loop => (
            "Playing",
            "loop: play the open note clip, the selected clips or the stretch between two markers over and over",
        ),
        Action::RecordSound => ("Making", "record sound (a microphone) while playing; again, or pause, ends the take"),
        Action::Angle(1) => ("Making", "cut to camera angle 1 (2 to 9 likewise)"),
        Action::Angle(_) => return None,
        Action::AddAsset => ("Project", "add an asset: link or files"),
        Action::Paste => ("Project", "paste: copied clips at the playhead; a link, file or image is imported"),
        Action::Mixer => ("Panels", "mixer"),
        Action::Colour => ("Panels", "colour: scenarios, match and look"),
        Action::ToggleLook => ("Panels", "look of the shown scenario off / on"),
        Action::ToggleMatch => ("Panels", "match of the shown scenario off / on"),
        Action::History => ("Panels", "saved states and checkpoints"),
        Action::Help => ("Panels", "this list"),
        Action::FitTimeline => ("Playing", "show the whole sequence in the timeline"),
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
        Key::Backspace => "Backspace".to_owned(),
        Key::ArrowLeft => "Left".to_owned(),
        Key::ArrowRight => "Right".to_owned(),
        other => other.name().to_owned(),
    };
    format!("{}{}{key}", if binding.ctrl { "Ctrl+" } else { "" }, if binding.shift { "Shift+" } else { "" })
}

/// The first key of an action in the map in force, as its cap reads: the
/// hint on buttons and menu entries. Empty if the action has no key.
pub fn hint(action: Action) -> String {
    active().iter().find(|b| b.action == action).map(cap).unwrap_or_default()
}

/// The key reference: every listed action once, with all its keys, by group.
pub fn reference() -> Vec<(&'static str, Vec<(String, &'static str)>)> {
    let mut groups: Vec<(&'static str, Vec<(String, &'static str)>)> = Vec::new();
    for order in ["Playing", "Cutting", "Making", "Panels", "Output", "Project"] {
        let mut lines: Vec<(String, &'static str)> = Vec::new();
        for binding in active() {
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
    fn a_key_file_changes_the_table() {
        // The file as first written changes nothing.
        let same = with_changes(&default_file(), "keys.conf");
        assert!(same.problems.is_empty(), "{:?}", same.problems);
        assert_eq!(same.bindings, KEYMAP);
        let find = |changed: &Changed, key: Key, ctrl: bool, shift: bool| {
            changed.bindings.iter().find(|b| b.key == key && b.ctrl == ctrl && b.shift == shift).map(|b| b.action)
        };
        let changed = with_changes(
            "editor x = Split\neditor Ctrl+Shift+f2 = split\nnone = Marker\n# editor G = Split\n\
             nonsense\neditor Hyper+A = Undo\neditor Blub = Undo\nany A = Fly\neditor x = Mixer\n",
            "keys.conf",
        );
        // Split has exactly the two keys of the file; S is free.
        assert_eq!(find(&changed, Key::X, false, false), Some(Action::Split));
        assert_eq!(find(&changed, Key::F2, true, true), Some(Action::Split));
        assert_eq!(find(&changed, Key::S, false, false), None);
        // X was taken from the ripple delete; Shift+X still deletes.
        assert!(!changed.bindings.iter().any(|b| b.action == Action::RippleDelete));
        assert_eq!(find(&changed, Key::X, false, true), Some(Action::Delete));
        // The marker has no key; Mixer was named but lost the fight for X, so it has none either.
        assert!(!changed.bindings.iter().any(|b| b.action == Action::Marker || b.action == Action::Mixer));
        assert_eq!(find(&changed, Key::Z, false, false), Some(Action::Undo));
        assert_eq!(changed.problems.len(), 5, "{:?}", changed.problems);
        assert!(changed.problems[0].starts_with("line 5:"));
        assert!(changed.problems[4].contains("both Split and Mixer"));
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
