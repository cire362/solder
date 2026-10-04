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

pub mod catalog;
pub mod install;
pub mod manifest;
pub mod snippet;
pub mod testing;

pub use catalog::Entry;
pub use manifest::{Code, Extension, Grammar, Language, Origin, Server, SnippetFile};
pub use snippet::Snippet;
