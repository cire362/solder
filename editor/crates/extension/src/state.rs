//! What the user decided about the extensions they installed: which are
//! turned off, which stay on the version they have, and what each was
//! allowed to do outside a sandbox. Kept in `state.json` next to them.

use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};

use crate::{Extension, Origin};

const FILE: &str = "state.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Choice {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    off: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    kept: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allowed: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// By `zed/<id>` or `vscode/<id>`, the id in lowercase.
    choices: BTreeMap<String, Choice>,
}

fn name(origin: Origin, id: &str) -> String {
    format!("{}/{}", origin.folder(), id.to_lowercase())
}

impl State {
    /// Reads the file under `root`; blocking. A missing or broken file is
    /// no decisions at all, which is the careful reading: everything is on,
    /// and nothing was allowed yet.
    pub fn load(root: &Path) -> Self {
        let choices = std::fs::read_to_string(root.join(FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { choices }
    }

    /// Writes the file under `root`, whole or not at all; blocking.
    pub fn save(&self, root: &Path) -> Result<(), String> {
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let text = serde_json::to_string_pretty(&self.choices).map_err(|e| e.to_string())?;
        let fresh = root.join(format!("{FILE}.new"));
        std::fs::write(&fresh, text).map_err(|e| e.to_string())?;
        std::fs::rename(&fresh, root.join(FILE)).map_err(|e| e.to_string())
    }

    fn choice(&self, origin: Origin, id: &str) -> Option<&Choice> {
        self.choices.get(&name(origin, id))
    }

    /// Changes one extension's record, dropping it once it says nothing.
    fn change(&mut self, origin: Origin, id: &str, change: impl FnOnce(&mut Choice)) {
        let name = name(origin, id);
        let mut choice = self.choices.remove(&name).unwrap_or_default();
        change(&mut choice);
        if choice != Choice::default() {
            self.choices.insert(name, choice);
        }
    }

    pub fn is_off(&self, origin: Origin, id: &str) -> bool {
        self.choice(origin, id).is_some_and(|c| c.off)
    }

    pub fn set_off(&mut self, origin: Origin, id: &str, off: bool) {
        self.change(origin, id, |c| c.off = off);
    }

    /// Whether it stays on the version it has: no update is offered.
    pub fn is_kept(&self, origin: Origin, id: &str) -> bool {
        self.choice(origin, id).is_some_and(|c| c.kept)
    }

    pub fn set_kept(&mut self, origin: Origin, id: &str, kept: bool) {
        self.change(origin, id, |c| c.kept = kept);
    }

    /// What `extension` would do outside a sandbox that it was not allowed
    /// before. Empty means it can be installed without asking: it does
    /// nothing of the kind, or an earlier version already asked for the
    /// same.
    pub fn asks(&self, extension: &Extension) -> Vec<String> {
        let allowed = self.choice(extension.origin, &extension.id);
        extension
            .outside()
            .into_iter()
            .filter(|what| allowed.is_none_or(|c| !c.allowed.contains(what)))
            .collect()
    }

    /// Records that the user allowed what `extension` does outside a
    /// sandbox. What an older version did and this one does not is dropped.
    pub fn allow(&mut self, extension: &Extension) {
        let allowed = extension.outside();
        self.change(extension.origin, &extension.id, |c| c.allowed = allowed);
    }

    /// Drops everything about an extension that was removed.
    pub fn forget(&mut self, origin: Origin, id: &str) {
        self.choices.remove(&name(origin, id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{scratch, zed_extension};

    #[test]
    fn what_was_allowed_is_not_asked_again_but_more_is() {
        let dir = scratch("state-asks");
        zed_extension(&dir.join("demo"));
        let mut extension = crate::manifest::read(&dir.join("demo")).unwrap();
        let mut state = State::default();
        assert_eq!(
            state.asks(&extension),
            [
                "Download and start the language server Demo LS",
                "Run demo-ls --version",
            ]
        );
        state.allow(&extension);
        assert!(state.asks(&extension).is_empty());
        // The id's case does not matter: the catalogs and manifests differ.
        extension.id = "DEMO".into();
        assert!(state.asks(&extension).is_empty());

        // A later version that runs one more command asks for that one.
        extension.commands.push(("curl".into(), vec!["**".into()]));
        assert_eq!(state.asks(&extension), ["Run curl **"]);
        state.allow(&extension);
        assert!(state.asks(&extension).is_empty());

        // An extension that is only data never asks.
        extension.code = crate::Code::None;
        assert!(state.asks(&extension).is_empty());
        // Removed, it is forgotten and asks from the start.
        extension.code = crate::Code::Zed {
            api: "0.7.0".into(),
        };
        state.forget(Origin::Zed, "demo");
        assert_eq!(state.asks(&extension).len(), 3);
    }

    #[test]
    fn decisions_survive_a_restart() {
        let dir = scratch("state-file");
        zed_extension(&dir.join("demo"));
        let extension = crate::manifest::read(&dir.join("demo")).unwrap();
        // Nothing there yet.
        assert_eq!(State::load(&dir), State::default());

        let mut state = State::default();
        state.allow(&extension);
        state.set_off(Origin::Zed, "demo", true);
        state.set_kept(Origin::VsCode, "Vue.volar", true);
        state.save(&dir).unwrap();
        let read = State::load(&dir);
        assert_eq!(read, state);
        assert!(read.is_off(Origin::Zed, "Demo"));
        assert!(!read.is_off(Origin::VsCode, "demo"));
        assert!(read.is_kept(Origin::VsCode, "vue.volar"));
        assert!(read.asks(&extension).is_empty());

        // A decision taken back leaves no trace in the file.
        state.set_kept(Origin::VsCode, "Vue.volar", false);
        state.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join("state.json")).unwrap();
        assert!(!text.contains("volar"), "{text}");
        assert!(text.contains("\"off\": true"), "{text}");

        // A file that cannot be read is no decisions: on, and asked again.
        std::fs::write(dir.join("state.json"), "{ not json").unwrap();
        let broken = State::load(&dir);
        assert!(!broken.is_off(Origin::Zed, "demo"));
        assert_eq!(broken.asks(&extension).len(), 2);
    }
}
