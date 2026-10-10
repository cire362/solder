//! Key bindings of other editors as Solder's: the keys people know from VS
//! Code or a JetBrains IDE for the actions Solder has, and the bindings a
//! user changed there.

use std::collections::BTreeMap;

use regex::Regex;
use serde_json::Value;

/// One binding in Solder's terms: `cmd-shift-p`, `workspace::Find`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub keys: String,
    pub action: &'static str,
    pub context: Option<&'static str>,
}

/// What came over, and what could not, each with the reason.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Converted {
    pub bindings: Vec<Binding>,
    pub skipped: Vec<String>,
}

const FULL: Option<&str> = Some("Editor && mode == full");
const EDITOR: Option<&str> = Some("Editor");

/// Solder's actions by a short name, with the context each is bound in.
const ACTIONS: &[(&str, &str, Option<&str>)] = &[
    ("duplicate_line", "editor::DuplicateLine", FULL),
    ("move_line_up", "editor::MoveLineUp", FULL),
    ("move_line_down", "editor::MoveLineDown", FULL),
    ("toggle_comment", "editor::ToggleComment", FULL),
    ("select_next", "editor::SelectNextOccurrence", EDITOR),
    ("cursor_above", "editor::AddCursorAbove", FULL),
    ("cursor_below", "editor::AddCursorBelow", FULL),
    ("select_line", "editor::SelectLine", FULL),
    ("select_all", "editor::SelectAll", EDITOR),
    ("format", "editor::FormatDocument", FULL),
    ("definition", "editor::GoToDefinition", FULL),
    ("references", "editor::FindReferences", FULL),
    ("rename", "editor::RenameSymbol", FULL),
    ("code_actions", "editor::CodeActions", FULL),
    ("hover", "editor::ShowHover", FULL),
    ("completions", "editor::ShowCompletions", FULL),
    ("next_problem", "editor::NextDiagnostic", FULL),
    ("prev_problem", "editor::PrevDiagnostic", FULL),
    ("breakpoint", "editor::ToggleBreakpoint", FULL),
    ("save", "editor::Save", FULL),
    ("undo", "editor::Undo", EDITOR),
    ("redo", "editor::Redo", EDITOR),
    ("file_finder", "workspace::ToggleFileFinder", None),
    ("palette", "workspace::ToggleCommandPalette", None),
    ("go_to_line", "workspace::GoToLine", EDITOR),
    ("find", "workspace::Find", None),
    ("replace", "workspace::FindReplace", None),
    ("find_next", "workspace::FindNext", None),
    ("find_prev", "workspace::FindPrev", None),
    ("search", "workspace::ShowSearch", None),
    ("files", "workspace::ShowFiles", None),
    ("git", "workspace::ShowGit", None),
    ("services", "workspace::ShowServices", None),
    ("database", "workspace::ShowDatabase", None),
    ("plugins", "workspace::ShowPlugins", None),
    ("sidebar", "workspace::ToggleSidebar", None),
    ("new_file", "workspace::NewFile", None),
    ("save_as", "workspace::SaveAs", None),
    ("open_folder", "workspace::OpenFolder", None),
    ("settings", "workspace::OpenSettings", None),
    ("keymap", "workspace::OpenKeymap", None),
    ("split", "workspace::SplitRight", None),
    ("next_pane", "workspace::FocusNextPane", None),
    ("prev_pane", "workspace::FocusPrevPane", None),
    ("terminal", "workspace::ToggleTerminal", None),
    ("new_terminal", "workspace::NewTerminal", None),
    ("close_tab", "workspace::CloseTab", None),
    ("next_tab", "workspace::NextTab", None),
    ("prev_tab", "workspace::PrevTab", None),
    ("debug_start", "workspace::DebugStart", None),
    ("debug_stop", "workspace::DebugStop", None),
    ("step_over", "workspace::DebugStepOver", None),
    ("step_in", "workspace::DebugStepIn", None),
    ("step_out", "workspace::DebugStepOut", None),
    ("quit", "workspace::Quit", None),
];

/// Every Solder action a binding can come over to, with its context.
pub fn actions() -> impl Iterator<Item = (&'static str, Option<&'static str>)> {
    ACTIONS
        .iter()
        .map(|(_, action, context)| (*action, *context))
}

fn action(short: &str) -> Option<(&'static str, Option<&'static str>)> {
    ACTIONS
        .iter()
        .find(|(name, _, _)| *name == short)
        .map(|(_, action, context)| (*action, *context))
}

// ---------------------------------------------------------------- VS Code

/// VS Code's commands, the Solder action each is, and VS Code's own keys
/// for it on macOS and elsewhere (empty where it has none).
const VSCODE: &[(&str, &str, &str, &str)] = &[
    (
        "editor.action.copyLinesDownAction",
        "duplicate_line",
        "shift+alt+down",
        "shift+alt+down",
    ),
    (
        "editor.action.moveLinesUpAction",
        "move_line_up",
        "alt+up",
        "alt+up",
    ),
    (
        "editor.action.moveLinesDownAction",
        "move_line_down",
        "alt+down",
        "alt+down",
    ),
    (
        "editor.action.commentLine",
        "toggle_comment",
        "cmd+/",
        "ctrl+/",
    ),
    (
        "editor.action.addSelectionToNextFindMatch",
        "select_next",
        "cmd+d",
        "ctrl+d",
    ),
    (
        "editor.action.insertCursorAbove",
        "cursor_above",
        "alt+cmd+up",
        "ctrl+alt+up",
    ),
    (
        "editor.action.insertCursorBelow",
        "cursor_below",
        "alt+cmd+down",
        "ctrl+alt+down",
    ),
    ("expandLineSelection", "select_line", "cmd+l", "ctrl+l"),
    ("editor.action.selectAll", "select_all", "cmd+a", "ctrl+a"),
    (
        "editor.action.formatDocument",
        "format",
        "shift+alt+f",
        "shift+alt+f",
    ),
    ("editor.action.revealDefinition", "definition", "f12", "f12"),
    (
        "editor.action.goToReferences",
        "references",
        "shift+f12",
        "shift+f12",
    ),
    ("editor.action.rename", "rename", "f2", "f2"),
    ("editor.action.quickFix", "code_actions", "cmd+.", "ctrl+."),
    (
        "editor.action.showHover",
        "hover",
        "cmd+k cmd+i",
        "ctrl+k ctrl+i",
    ),
    (
        "editor.action.triggerSuggest",
        "completions",
        "ctrl+space",
        "ctrl+space",
    ),
    (
        "editor.action.marker.nextInFiles",
        "next_problem",
        "f8",
        "f8",
    ),
    (
        "editor.action.marker.prevInFiles",
        "prev_problem",
        "shift+f8",
        "shift+f8",
    ),
    ("editor.action.marker.next", "next_problem", "", ""),
    ("editor.action.marker.prev", "prev_problem", "", ""),
    (
        "editor.debug.action.toggleBreakpoint",
        "breakpoint",
        "f9",
        "f9",
    ),
    ("workbench.action.files.save", "save", "cmd+s", "ctrl+s"),
    ("undo", "undo", "cmd+z", "ctrl+z"),
    ("redo", "redo", "shift+cmd+z", "ctrl+y"),
    (
        "workbench.action.quickOpen",
        "file_finder",
        "cmd+p",
        "ctrl+p",
    ),
    (
        "workbench.action.showCommands",
        "palette",
        "shift+cmd+p",
        "ctrl+shift+p",
    ),
    (
        "workbench.action.gotoLine",
        "go_to_line",
        "ctrl+g",
        "ctrl+g",
    ),
    ("actions.find", "find", "cmd+f", "ctrl+f"),
    (
        "editor.action.startFindReplaceAction",
        "replace",
        "alt+cmd+f",
        "ctrl+h",
    ),
    (
        "editor.action.nextMatchFindAction",
        "find_next",
        "cmd+g",
        "f3",
    ),
    (
        "editor.action.previousMatchFindAction",
        "find_prev",
        "shift+cmd+g",
        "shift+f3",
    ),
    (
        "workbench.action.findInFiles",
        "search",
        "shift+cmd+f",
        "ctrl+shift+f",
    ),
    (
        "workbench.view.explorer",
        "files",
        "shift+cmd+e",
        "ctrl+shift+e",
    ),
    ("workbench.view.scm", "git", "ctrl+shift+g", "ctrl+shift+g"),
    (
        "workbench.view.extensions",
        "plugins",
        "shift+cmd+x",
        "ctrl+shift+x",
    ),
    (
        "workbench.action.toggleSidebarVisibility",
        "sidebar",
        "cmd+b",
        "ctrl+b",
    ),
    (
        "workbench.action.files.newUntitledFile",
        "new_file",
        "cmd+n",
        "ctrl+n",
    ),
    (
        "workbench.action.files.saveAs",
        "save_as",
        "shift+cmd+s",
        "ctrl+shift+s",
    ),
    (
        "workbench.action.files.openFolder",
        "open_folder",
        "",
        "ctrl+k ctrl+o",
    ),
    (
        "workbench.action.files.openFileFolder",
        "open_folder",
        "cmd+o",
        "",
    ),
    (
        "workbench.action.openSettings",
        "settings",
        "cmd+,",
        "ctrl+,",
    ),
    (
        "workbench.action.openGlobalKeybindings",
        "keymap",
        "cmd+k cmd+s",
        "ctrl+k ctrl+s",
    ),
    ("workbench.action.splitEditor", "split", "cmd+\\", "ctrl+\\"),
    (
        "workbench.action.focusNextGroup",
        "next_pane",
        "cmd+k cmd+right",
        "ctrl+k ctrl+right",
    ),
    (
        "workbench.action.focusPreviousGroup",
        "prev_pane",
        "cmd+k cmd+left",
        "ctrl+k ctrl+left",
    ),
    (
        "workbench.action.terminal.toggleTerminal",
        "terminal",
        "ctrl+`",
        "ctrl+`",
    ),
    (
        "workbench.action.togglePanel",
        "terminal",
        "cmd+j",
        "ctrl+j",
    ),
    (
        "workbench.action.terminal.new",
        "new_terminal",
        "ctrl+shift+`",
        "ctrl+shift+`",
    ),
    (
        "workbench.action.closeActiveEditor",
        "close_tab",
        "cmd+w",
        "ctrl+w",
    ),
    (
        "workbench.action.nextEditor",
        "next_tab",
        "alt+cmd+right",
        "ctrl+pagedown",
    ),
    (
        "workbench.action.previousEditor",
        "prev_tab",
        "alt+cmd+left",
        "ctrl+pageup",
    ),
    ("workbench.action.debug.start", "debug_start", "f5", "f5"),
    ("workbench.action.debug.continue", "debug_start", "", ""),
    (
        "workbench.action.debug.stop",
        "debug_stop",
        "shift+f5",
        "shift+f5",
    ),
    ("workbench.action.debug.stepOver", "step_over", "f10", "f10"),
    ("workbench.action.debug.stepInto", "step_in", "f11", "f11"),
    (
        "workbench.action.debug.stepOut",
        "step_out",
        "shift+f11",
        "shift+f11",
    ),
    ("workbench.action.quit", "quit", "cmd+q", "ctrl+q"),
];

/// One key with its modifiers in VS Code's writing (`shift+cmd+p`,
/// `ctrl+[BracketLeft]`), as Solder writes it.
fn vscode_stroke(stroke: &str) -> Option<String> {
    let mut modifiers = Vec::new();
    let mut key = None;
    // `+` joins, and is also a key: `ctrl++`.
    let stroke = stroke.trim().to_ascii_lowercase();
    let (parts, plus): (&str, bool) = match stroke.strip_suffix("++") {
        Some(rest) => (rest, true),
        None if stroke == "+" => ("", true),
        None => (stroke.as_str(), false),
    };
    for part in parts.split('+').filter(|p| !p.is_empty()) {
        match part {
            "ctrl" | "control" => modifiers.push("ctrl"),
            "shift" => modifiers.push("shift"),
            "alt" | "option" | "opt" => modifiers.push("alt"),
            "cmd" | "meta" | "win" | "super" | "command" => modifiers.push("cmd"),
            other if key.is_none() => key = Some(vscode_key(other)?),
            _ => return None,
        }
    }
    let key = match (key, plus) {
        (Some(key), false) => key,
        (None, true) => "+".to_string(),
        _ => return None,
    };
    modifiers.sort_by_key(|m| ["ctrl", "alt", "shift", "cmd"].iter().position(|x| x == m));
    modifiers.dedup();
    let mut out = modifiers.join("-");
    if !out.is_empty() {
        out.push('-');
    }
    out.push_str(&key);
    Some(out)
}

fn vscode_key(key: &str) -> Option<String> {
    // By position on the keyboard: `[BracketLeft]`, `[KeyA]`, `[Digit1]`.
    if let Some(code) = key.strip_prefix('[').and_then(|k| k.strip_suffix(']')) {
        let named = match code {
            "bracketleft" => "[",
            "bracketright" => "]",
            "backquote" => "`",
            "slash" => "/",
            "backslash" => "\\",
            "comma" => ",",
            "period" => ".",
            "semicolon" => ";",
            "quote" => "'",
            "minus" => "-",
            "equal" => "=",
            _ => {
                let letter = code
                    .strip_prefix("key")
                    .or_else(|| code.strip_prefix("digit"))?;
                return (letter.len() == 1).then(|| letter.to_string());
            }
        };
        return Some(named.to_string());
    }
    let named = match key {
        "escape" | "esc" => "escape",
        "enter" | "return" => "enter",
        "tab" | "space" | "backspace" | "delete" | "insert" | "home" | "end" | "up" | "down"
        | "left" | "right" | "pageup" | "pagedown" => key,
        _ if key.len() == 1 => key,
        _ if key.starts_with('f')
            && key[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) =>
        {
            key
        }
        _ => return None,
    };
    Some(named.to_string())
}

/// `cmd+k cmd+s` as `cmd-k cmd-s`.
pub fn vscode_keys(keys: &str) -> Option<String> {
    let strokes: Vec<String> = keys
        .split_whitespace()
        .map(vscode_stroke)
        .collect::<Option<_>>()?;
    (!strokes.is_empty()).then(|| strokes.join(" "))
}

/// The keys VS Code users know, for the actions Solder has.
pub fn vscode_preset(mac: bool) -> Vec<Binding> {
    VSCODE
        .iter()
        .filter_map(|(_, short, mac_keys, other_keys)| {
            let keys = vscode_keys(if mac { mac_keys } else { other_keys })?;
            let (action, context) = action(short)?;
            Some(Binding {
                keys,
                action,
                context,
            })
        })
        .collect()
}

/// The conditions under which an editor binding still means the same here.
const VSCODE_WHEN: &[&str] = &[
    "editorTextFocus",
    "editorFocus",
    "textInputFocus",
    "!editorReadonly",
    "!inQuickOpen",
];

/// The user's `keybindings.json` from VS Code or Cursor.
pub fn from_vscode(keybindings: &Value) -> Converted {
    let mut out = Converted::default();
    for entry in keybindings.as_array().into_iter().flatten() {
        let (Some(keys), Some(command)) = (entry["key"].as_str(), entry["command"].as_str()) else {
            continue;
        };
        let mut skip = |why: &str| out.skipped.push(format!("{keys}: {command} ({why})"));
        if command.starts_with('-') {
            skip("removing a default binding is not imported");
            continue;
        }
        let Some((action, context)) = VSCODE
            .iter()
            .find(|(name, ..)| *name == command)
            .and_then(|(_, short, ..)| action(short))
        else {
            skip("Solder has no such action");
            continue;
        };
        if entry.get("args").is_some_and(|a| !a.is_null()) {
            skip("it takes arguments");
            continue;
        }
        let known = entry["when"].as_str().is_none_or(|when| {
            when.split("&&")
                .all(|clause| VSCODE_WHEN.contains(&clause.trim()))
        });
        if !known {
            skip("its condition has no equivalent");
            continue;
        }
        match vscode_keys(keys) {
            Some(keys) => out.bindings.push(Binding {
                keys,
                action,
                context,
            }),
            None => skip("the key is not known"),
        }
    }
    out
}

// -------------------------------------------------------------- JetBrains

/// JetBrains action ids, the Solder action each is, and the IDE's own keys
/// on macOS and elsewhere, in VS Code's writing.
const JETBRAINS: &[(&str, &str, &str, &str)] = &[
    ("EditorDuplicate", "duplicate_line", "cmd+d", "ctrl+d"),
    ("MoveLineUp", "move_line_up", "shift+alt+up", "shift+alt+up"),
    (
        "MoveLineDown",
        "move_line_down",
        "shift+alt+down",
        "shift+alt+down",
    ),
    (
        "MoveStatementUp",
        "move_line_up",
        "shift+cmd+up",
        "ctrl+shift+up",
    ),
    (
        "MoveStatementDown",
        "move_line_down",
        "shift+cmd+down",
        "ctrl+shift+down",
    ),
    ("CommentByLineComment", "toggle_comment", "cmd+/", "ctrl+/"),
    ("SelectNextOccurrence", "select_next", "ctrl+g", "alt+j"),
    ("EditorSelectLine", "select_line", "", ""),
    ("$SelectAll", "select_all", "cmd+a", "ctrl+a"),
    ("ReformatCode", "format", "alt+cmd+l", "ctrl+alt+l"),
    ("GotoDeclaration", "definition", "cmd+b", "ctrl+b"),
    ("FindUsages", "references", "alt+f7", "alt+f7"),
    ("RenameElement", "rename", "shift+f6", "shift+f6"),
    (
        "ShowIntentionActions",
        "code_actions",
        "alt+enter",
        "alt+enter",
    ),
    ("QuickJavaDoc", "hover", "f1", "ctrl+q"),
    ("CodeCompletion", "completions", "ctrl+space", "ctrl+space"),
    ("GotoNextError", "next_problem", "f2", "f2"),
    ("GotoPreviousError", "prev_problem", "shift+f2", "shift+f2"),
    ("ToggleLineBreakpoint", "breakpoint", "cmd+f8", "ctrl+f8"),
    ("SaveAll", "save", "cmd+s", "ctrl+s"),
    ("$Undo", "undo", "cmd+z", "ctrl+z"),
    ("$Redo", "redo", "shift+cmd+z", "ctrl+shift+z"),
    ("GotoFile", "file_finder", "shift+cmd+o", "ctrl+shift+n"),
    ("GotoAction", "palette", "shift+cmd+a", "ctrl+shift+a"),
    ("GotoLine", "go_to_line", "cmd+l", "ctrl+g"),
    ("Find", "find", "cmd+f", "ctrl+f"),
    ("Replace", "replace", "cmd+r", "ctrl+r"),
    ("FindNext", "find_next", "cmd+g", "f3"),
    ("FindPrevious", "find_prev", "shift+cmd+g", "shift+f3"),
    ("FindInPath", "search", "shift+cmd+f", "ctrl+shift+f"),
    ("ActivateProjectToolWindow", "files", "cmd+1", "alt+1"),
    ("ActivateVersionControlToolWindow", "git", "cmd+9", "alt+9"),
    ("ActivateServicesToolWindow", "services", "cmd+8", "alt+8"),
    ("ActivateDatabaseToolWindow", "database", "", ""),
    (
        "ActivateTerminalToolWindow",
        "terminal",
        "alt+f12",
        "alt+f12",
    ),
    (
        "NewScratchFile",
        "new_file",
        "shift+cmd+n",
        "ctrl+alt+shift+insert",
    ),
    ("ShowSettings", "settings", "cmd+,", "ctrl+alt+s"),
    ("SplitVertically", "split", "", ""),
    ("CloseContent", "close_tab", "cmd+w", "ctrl+f4"),
    ("NextTab", "next_tab", "shift+cmd+]", "alt+right"),
    ("PreviousTab", "prev_tab", "shift+cmd+[", "alt+left"),
    ("Debug", "debug_start", "ctrl+d", "shift+f9"),
    ("Resume", "debug_start", "alt+cmd+r", "f9"),
    ("Stop", "debug_stop", "cmd+f2", "ctrl+f2"),
    ("StepOver", "step_over", "f8", "f8"),
    ("StepInto", "step_in", "f7", "f7"),
    ("StepOut", "step_out", "shift+f8", "shift+f8"),
    ("Exit", "quit", "cmd+q", ""),
];

/// The keys JetBrains users know, for the actions Solder has.
pub fn jetbrains_preset(mac: bool) -> Vec<Binding> {
    JETBRAINS
        .iter()
        .filter_map(|(_, short, mac_keys, other_keys)| {
            let keys = vscode_keys(if mac { mac_keys } else { other_keys })?;
            let (action, context) = action(short)?;
            Some(Binding {
                keys,
                action,
                context,
            })
        })
        .collect()
}

/// `shift meta OPEN_BRACKET`, as a keymap file writes a keystroke.
fn jetbrains_stroke(stroke: &str) -> Option<String> {
    let mut parts = Vec::new();
    for word in stroke.split_whitespace() {
        let part = match word.to_ascii_lowercase().as_str() {
            "control" | "ctrl" => "ctrl".to_string(),
            "meta" => "cmd".to_string(),
            "alt" | "shift" => word.to_ascii_lowercase(),
            "back_space" => "backspace".into(),
            "page_up" => "pageup".into(),
            "page_down" => "pagedown".into(),
            "slash" | "divide" => "/".into(),
            "back_slash" => "\\".into(),
            "open_bracket" => "[".into(),
            "close_bracket" => "]".into(),
            "back_quote" => "`".into(),
            "minus" | "subtract" => "-".into(),
            "equals" => "=".into(),
            "comma" => ",".into(),
            "period" => ".".into(),
            "semicolon" => ";".into(),
            "quote" => "'".into(),
            other => other.to_string(),
        };
        parts.push(part);
    }
    vscode_stroke(&parts.join("+"))
}

/// A keymap file of a JetBrains IDE (`keymaps/*.xml`): the bindings the
/// user changed on top of the keymap it names as its parent.
pub fn from_jetbrains(xml: &str) -> Converted {
    let mut out = Converted::default();
    let action_re = Regex::new(r#"(?s)<action\s+id="([^"]+)"\s*(?:/>|>(.*?)</action>)"#).unwrap();
    let stroke_re = Regex::new(
        r#"<keyboard-shortcut\s+first-keystroke="([^"]+)"(?:\s+second-keystroke="([^"]+)")?"#,
    )
    .unwrap();
    for found in action_re.captures_iter(xml) {
        let id = &found[1];
        let body = found.get(2).map_or("", |m| m.as_str());
        let Some((action, context)) = JETBRAINS
            .iter()
            .find(|(name, ..)| *name == id)
            .and_then(|(_, short, ..)| action(short))
        else {
            if stroke_re.is_match(body) {
                out.skipped
                    .push(format!("{id} (Solder has no such action)"));
            }
            continue;
        };
        for stroke in stroke_re.captures_iter(body) {
            let first = jetbrains_stroke(&stroke[1]);
            let second = stroke.get(2).map(|m| jetbrains_stroke(m.as_str()));
            let keys = match (first, second) {
                (Some(first), None) => Some(first),
                (Some(first), Some(Some(second))) => Some(format!("{first} {second}")),
                _ => None,
            };
            match keys {
                Some(keys) => out.bindings.push(Binding {
                    keys,
                    action,
                    context,
                }),
                None => out
                    .skipped
                    .push(format!("{}: {id} (the key is not known)", &stroke[1])),
            }
        }
    }
    out
}

// -------------------------------------------------------------------- Zed

/// Zed's actions and the Solder action each is.
const ZED: &[(&str, &str)] = &[
    ("editor::DuplicateLineDown", "duplicate_line"),
    ("editor::DuplicateLine", "duplicate_line"),
    ("editor::MoveLineUp", "move_line_up"),
    ("editor::MoveLineDown", "move_line_down"),
    ("editor::ToggleComments", "toggle_comment"),
    ("editor::SelectNext", "select_next"),
    ("editor::AddSelectionAbove", "cursor_above"),
    ("editor::AddSelectionBelow", "cursor_below"),
    ("editor::SelectLine", "select_line"),
    ("editor::SelectAll", "select_all"),
    ("editor::Format", "format"),
    ("editor::GoToDefinition", "definition"),
    ("editor::FindAllReferences", "references"),
    ("editor::Rename", "rename"),
    ("editor::ToggleCodeActions", "code_actions"),
    ("editor::Hover", "hover"),
    ("editor::ShowCompletions", "completions"),
    ("editor::GoToDiagnostic", "next_problem"),
    ("editor::GoToPreviousDiagnostic", "prev_problem"),
    ("editor::GoToPrevDiagnostic", "prev_problem"),
    ("editor::ToggleBreakpoint", "breakpoint"),
    ("editor::Undo", "undo"),
    ("editor::Redo", "redo"),
    ("workspace::Save", "save"),
    ("workspace::SaveAs", "save_as"),
    ("workspace::NewFile", "new_file"),
    ("workspace::Open", "open_folder"),
    ("workspace::ToggleLeftDock", "sidebar"),
    ("workspace::ActivateNextPane", "next_pane"),
    ("workspace::ActivatePreviousPane", "prev_pane"),
    ("file_finder::Toggle", "file_finder"),
    ("command_palette::Toggle", "palette"),
    ("go_to_line::Toggle", "go_to_line"),
    ("buffer_search::Deploy", "find"),
    ("buffer_search::DeployReplace", "replace"),
    ("search::SelectNextMatch", "find_next"),
    ("search::SelectPreviousMatch", "find_prev"),
    ("pane::DeploySearch", "search"),
    ("workspace::NewSearch", "search"),
    ("project_panel::ToggleFocus", "files"),
    ("git_panel::ToggleFocus", "git"),
    ("terminal_panel::ToggleFocus", "terminal"),
    ("terminal_panel::Toggle", "terminal"),
    ("workspace::NewTerminal", "new_terminal"),
    ("zed::Extensions", "plugins"),
    ("zed::OpenSettings", "settings"),
    ("zed::OpenKeymap", "keymap"),
    ("pane::SplitRight", "split"),
    ("pane::CloseActiveItem", "close_tab"),
    ("pane::ActivateNextItem", "next_tab"),
    ("pane::ActivatePreviousItem", "prev_tab"),
    ("pane::ActivatePrevItem", "prev_tab"),
    ("debugger::Start", "debug_start"),
    ("debugger::Continue", "debug_start"),
    ("debugger::Stop", "debug_stop"),
    ("debugger::StepOver", "step_over"),
    ("debugger::StepInto", "step_in"),
    ("debugger::StepOut", "step_out"),
    ("zed::Quit", "quit"),
];

/// The user's `keymap.json` from Zed, whose keys are already written as
/// Solder writes them.
pub fn from_zed(keymap: &Value) -> Converted {
    let mut out = Converted::default();
    for section in keymap.as_array().into_iter().flatten() {
        let context = section["context"].as_str();
        // Elsewhere the same keys mean something else: a terminal, vim's
        // normal mode, a panel Solder does not have.
        let plain = matches!(
            context,
            None | Some("Workspace" | "Editor" | "Editor && mode == full" | "Pane")
        );
        for (keys, value) in section["bindings"].as_object().into_iter().flatten() {
            let name = match value {
                Value::String(name) => name.as_str(),
                Value::Array(_) => {
                    out.skipped.push(format!("{keys} (it takes arguments)"));
                    continue;
                }
                _ => continue,
            };
            let mut skip = |why: &str| out.skipped.push(format!("{keys}: {name} ({why})"));
            if !plain {
                skip("its context has no equivalent");
                continue;
            }
            match ZED
                .iter()
                .find(|(zed, _)| *zed == name)
                .and_then(|(_, short)| action(short))
            {
                Some((action, context)) => out.bindings.push(Binding {
                    keys: keys.clone(),
                    action,
                    context,
                }),
                None => skip("Solder has no such action"),
            }
        }
    }
    out
}

// ------------------------------------------------------------------ output

/// A keymap file for Solder: one section per context, later bindings of
/// the same keys replacing earlier ones.
pub fn to_json(bindings: &[Binding]) -> String {
    let mut sections: BTreeMap<Option<&str>, BTreeMap<&str, &str>> = BTreeMap::new();
    for binding in bindings {
        sections
            .entry(binding.context)
            .or_default()
            .insert(&binding.keys, binding.action);
    }
    let sections: Vec<Value> = sections
        .into_iter()
        .map(|(context, bindings)| {
            let mut section = serde_json::Map::new();
            if let Some(context) = context {
                section.insert("context".into(), context.into());
            }
            section.insert("bindings".into(), serde_json::json!(bindings));
            section.into()
        })
        .collect();
    serde_json::to_string_pretty(&sections).unwrap_or_else(|_| "[]".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn find<'a>(bindings: &'a [Binding], action: &str) -> Vec<&'a str> {
        bindings
            .iter()
            .filter(|b| b.action == action)
            .map(|b| b.keys.as_str())
            .collect()
    }

    #[test]
    fn writes_keys_as_solder_does() {
        for (from, to) in [
            ("cmd+k cmd+s", "cmd-k cmd-s"),
            ("shift+cmd+p", "shift-cmd-p"),
            ("Ctrl+Shift+Alt+F5", "ctrl-alt-shift-f5"),
            ("ctrl+`", "ctrl-`"),
            ("ctrl+[BracketLeft]", "ctrl-["),
            ("alt+[KeyK]", "alt-k"),
            ("meta+enter", "cmd-enter"),
            ("ctrl++", "ctrl-+"),
            ("cmd+\\", "cmd-\\"),
            ("escape", "escape"),
        ] {
            assert_eq!(vscode_keys(from).as_deref(), Some(to), "{from}");
        }
        for unknown in [
            "",
            "ctrl+oem_102",
            "ctrl+a+b",
            "cmd+numpad_add",
            "ctrl+shift",
        ] {
            assert_eq!(vscode_keys(unknown), None, "{unknown}");
        }
        assert_eq!(
            jetbrains_stroke("shift meta OPEN_BRACKET").as_deref(),
            Some("shift-cmd-[")
        );
        assert_eq!(
            jetbrains_stroke("ctrl alt L").as_deref(),
            Some("ctrl-alt-l")
        );
        assert_eq!(jetbrains_stroke("BACK_SPACE").as_deref(), Some("backspace"));
    }

    #[test]
    fn the_presets_bind_what_those_editors_do() {
        let mac = vscode_preset(true);
        assert_eq!(find(&mac, "workspace::SaveAs"), ["shift-cmd-s"]);
        assert_eq!(find(&mac, "workspace::OpenKeymap"), ["cmd-k cmd-s"]);
        assert_eq!(find(&mac, "workspace::ToggleTerminal"), ["ctrl-`", "cmd-j"]);
        assert_eq!(
            find(&vscode_preset(false), "workspace::FindReplace"),
            ["ctrl-h"]
        );
        // A command without a default key on a platform is left out there.
        assert_eq!(find(&mac, "workspace::OpenFolder"), ["cmd-o"]);

        let mac = jetbrains_preset(true);
        assert_eq!(find(&mac, "editor::DuplicateLine"), ["cmd-d"]);
        assert_eq!(find(&mac, "editor::GoToDefinition"), ["cmd-b"]);
        assert_eq!(find(&mac, "editor::RenameSymbol"), ["shift-f6"]);
        assert_eq!(
            find(&mac, "workspace::ToggleCommandPalette"),
            ["shift-cmd-a"]
        );
        let other = jetbrains_preset(false);
        assert_eq!(
            find(&other, "workspace::ToggleFileFinder"),
            ["ctrl-shift-n"]
        );
        assert_eq!(find(&other, "workspace::DebugStart"), ["shift-f9", "f9"]);
        // Every row names an action Solder has.
        for (_, short, ..) in VSCODE.iter().chain(JETBRAINS) {
            assert!(action(short).is_some(), "{short}");
        }
        for (_, short) in ZED {
            assert!(action(short).is_some(), "{short}");
        }
    }

    #[test]
    fn a_vscode_users_own_bindings_come_over() {
        let converted = from_vscode(&json!([
            {"key": "cmd+shift+d", "command": "editor.action.copyLinesDownAction", "when": "editorTextFocus && !editorReadonly"},
            {"key": "ctrl+alt+t", "command": "workbench.action.terminal.toggleTerminal"},
            {"key": "cmd+k z", "command": "workbench.action.toggleZenMode"},
            {"key": "cmd+d", "command": "-editor.action.addSelectionToNextFindMatch"},
            {"key": "cmd+e", "command": "workbench.action.quickOpen", "when": "terminalFocus"},
            {"key": "ctrl+oem_1", "command": "editor.action.commentLine"},
            {"key": "cmd+i", "command": "editor.action.rename", "args": {"x": 1}},
        ]));
        assert_eq!(
            converted.bindings,
            [
                Binding {
                    keys: "shift-cmd-d".into(),
                    action: "editor::DuplicateLine",
                    context: Some("Editor && mode == full"),
                },
                Binding {
                    keys: "ctrl-alt-t".into(),
                    action: "workspace::ToggleTerminal",
                    context: None,
                },
            ]
        );
        assert_eq!(
            converted.skipped,
            [
                "cmd+k z: workbench.action.toggleZenMode (Solder has no such action)",
                "cmd+d: -editor.action.addSelectionToNextFindMatch (removing a default binding is not imported)",
                "cmd+e: workbench.action.quickOpen (its condition has no equivalent)",
                "ctrl+oem_1: editor.action.commentLine (the key is not known)",
                "cmd+i: editor.action.rename (it takes arguments)",
            ]
        );
    }

    #[test]
    fn a_jetbrains_keymap_and_a_zed_keymap_come_over() {
        let converted = from_jetbrains(
            r#"<keymap version="1" name="Mine" parent="macOS">
              <action id="EditorDuplicate">
                <keyboard-shortcut first-keystroke="shift meta D" />
                <keyboard-shortcut first-keystroke="ctrl K" second-keystroke="ctrl D" />
              </action>
              <action id="GotoFile"><keyboard-shortcut first-keystroke="meta P" /></action>
              <action id="Vcs.QuickListPopupAction"><keyboard-shortcut first-keystroke="ctrl V" /></action>
              <action id="ReformatCode" />
              <action id="Find"><mouse-shortcut keystroke="button3" /></action>
            </keymap>"#,
        );
        assert_eq!(
            find(&converted.bindings, "editor::DuplicateLine"),
            ["shift-cmd-d", "ctrl-k ctrl-d"]
        );
        assert_eq!(
            find(&converted.bindings, "workspace::ToggleFileFinder"),
            ["cmd-p"]
        );
        assert_eq!(converted.bindings.len(), 3);
        assert_eq!(
            converted.skipped,
            ["Vcs.QuickListPopupAction (Solder has no such action)"]
        );

        let converted = from_zed(&json!([
            {"context": "Editor", "bindings": {
                "cmd-shift-d": "editor::DuplicateLineDown",
                "cmd-k cmd-x": "editor::Fold",
                "alt-m": ["workspace::SendKeystrokes", "x"]}},
            {"bindings": {"cmd-j": "terminal_panel::ToggleFocus"}},
            {"context": "Editor && vim_mode == normal", "bindings": {"space f": "file_finder::Toggle"}},
        ]));
        assert_eq!(
            find(&converted.bindings, "editor::DuplicateLine"),
            ["cmd-shift-d"]
        );
        assert_eq!(
            find(&converted.bindings, "workspace::ToggleTerminal"),
            ["cmd-j"]
        );
        assert_eq!(converted.bindings.len(), 2);
        assert_eq!(converted.skipped.len(), 3);
    }

    #[test]
    fn bindings_become_a_keymap_file() {
        let text = to_json(&[
            Binding {
                keys: "cmd-d".into(),
                action: "editor::DuplicateLine",
                context: Some("Editor && mode == full"),
            },
            Binding {
                keys: "cmd-j".into(),
                action: "workspace::ToggleTerminal",
                context: None,
            },
            // The later binding of the same keys is the one kept.
            Binding {
                keys: "cmd-j".into(),
                action: "workspace::ShowGit",
                context: None,
            },
        ]);
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!([
                {"bindings": {"cmd-j": "workspace::ShowGit"}},
                {"context": "Editor && mode == full", "bindings": {"cmd-d": "editor::DuplicateLine"}},
            ])
        );
    }
}
