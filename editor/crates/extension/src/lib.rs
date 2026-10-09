//! Extensions made for other editors.
//!
//! A Zed extension is a folder with `extension.toml`: themes, snippets,
//! languages (a tree-sitter grammar compiled to WebAssembly with its queries)
//! and, for a language server, a WebAssembly component that says how to get
//! and start it. A VS Code extension is a folder with `package.json`: themes,
//! snippets and language settings are data Solder reads; its code is a Node
//! program written against VS Code's API and does not run here.
//!
//! [`manifest`] reads a folder, [`catalog`] searches Zed's catalog and
//! Open VSX, [`install`] downloads an extension and puts it in place.
//! [`host`] runs the code of a Zed extension in a sandbox, and [`world`] is
//! what that code reaches outside it: npm, GitHub, downloads.

pub mod catalog;
pub mod gate;
pub mod host;
pub mod icons;
pub mod install;
pub mod manifest;
pub mod snippet;
pub mod state;
pub mod testing;
pub mod world;

pub use catalog::Entry;
pub use gate::{Event, Refusals};
pub use icons::IconTheme;
pub use manifest::{Code, Debugger, Extension, Grammar, Language, Origin, Server, SnippetFile};
pub use snippet::Snippet;
pub use state::State;
