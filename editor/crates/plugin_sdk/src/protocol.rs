//! The messages a plugin and the editor exchange, as JSON. A plugin never
//! shares memory with the editor: the editor hands it an [`Event`], and the
//! plugin asks for things with a [`Request`], each answered with a [`Reply`].

use serde::{Deserialize, Serialize};

/// Something that happened in the editor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The plugin was started.
    Activate,
    /// One of the plugin's commands was run from the command palette.
    Command {
        id: String,
    },
    /// A file came to the front.
    Open {
        path: String,
        language: Option<String>,
    },
    Save {
        path: String,
    },
    /// The file's text changed. Sent while the user types, so handling it
    /// runs under the typing budget.
    Change {
        path: String,
    },
}

impl Event {
    /// The name a manifest lists under `events` to receive it.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::Activate => "activate",
            Event::Command { .. } => "command",
            Event::Open { .. } => "open",
            Event::Save { .. } => "save",
            Event::Change { .. } => "change",
        }
    }
}

/// What a plugin asks of the editor. Each needs the permission named.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "call", rename_all = "snake_case")]
pub enum Request {
    /// `statusBar`: the plugin's text in the status bar; empty removes it.
    Status { text: String },
    /// No permission: a line in the plugin's log.
    Log { text: String },
    /// `editor:read`: the file in front.
    EditorText,
    /// `editor:write`: replaces bytes `start..end` of the file in front,
    /// which must still be `path`.
    Edit {
        path: String,
        start: usize,
        end: usize,
        text: String,
    },
    /// `fs:read`: a file of the project, by its path from the project root.
    ReadFile { path: String },
    /// `http:<host>`: a request to that host.
    Http(HttpRequest),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// The file in front and where its selection is, in bytes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EditorText {
    /// Absent for a file not saved yet.
    pub path: Option<String>,
    pub language: Option<String>,
    pub text: String,
    pub selection_start: usize,
    pub selection_end: usize,
}

/// The answer to a [`Request`]: its value, or why it was refused.
pub type Reply = Result<serde_json::Value, String>;
