//! The plugin host: loads a plugin's manifest and WebAssembly module and
//! runs it in a sandbox, apart from the editor's UI.
//!
//! A plugin can do nothing on its own. It computes inside its own memory and
//! asks the editor for everything else through [`Host`], and only for what
//! its manifest declares ([`Permission`]). It runs on its own thread under a
//! [`Budget`], so it cannot hold up typing.
//!
//! The interpreter is `wasmi`; nothing outside `runtime.rs` names it, so
//! another engine can take its place.

mod manifest;
mod runtime;
mod script;
pub mod testing;

pub use manifest::{Command, MANIFEST, MODULE, Manifest, Permission, SCRIPT, url_host};
pub use runtime::{Activity, Budget, Code, Host, Plugin, SLOW_AFTER, Stats};
pub use solder_plugin::{EditorText, Event, HttpRequest, HttpResponse, Reply, Request};
