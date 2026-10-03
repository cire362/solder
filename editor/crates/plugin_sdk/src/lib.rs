//! Write a Solder plugin in Rust.
//!
//! A plugin is a `cdylib` built for `wasm32-unknown-unknown`, next to a
//! `plugin.json` that names it and lists the permissions it needs:
//!
//! ```ignore
//! use solder_plugin::{Event, Plugin};
//!
//! #[derive(Default)]
//! struct WordCount;
//!
//! impl Plugin for WordCount {
//!     fn event(&mut self, event: Event) {
//!         if let Ok(editor) = solder_plugin::editor() {
//!             let words = editor.text.split_whitespace().count();
//!             solder_plugin::status(format!("{words} words"));
//!         }
//!     }
//! }
//!
//! solder_plugin::register!(WordCount);
//! ```
//!
//! The editor runs each plugin in its own WebAssembly instance on its own
//! thread, hands it events, and answers its requests if the manifest
//! declared the permission and the user approved it.

pub mod protocol;

pub use protocol::{EditorText, Event, HttpRequest, HttpResponse, Reply, Request};

/// A plugin: one value that lives as long as the plugin runs.
pub trait Plugin: Default {
    fn event(&mut self, event: Event);
}

/// Shows `text` in the status bar; empty text removes it. Needs `statusBar`.
pub fn status(text: impl Into<String>) {
    let _ = host::call(&Request::Status { text: text.into() });
}

/// Adds a line to the plugin's log, shown in the Plugins window.
pub fn log(text: impl Into<String>) {
    let _ = host::call(&Request::Log { text: text.into() });
}

/// The file in front. Needs `editor:read`.
pub fn editor() -> Result<EditorText, String> {
    reply(&Request::EditorText)
}

/// Replaces bytes `range` of the file in front, which must still be `path`.
/// Needs `editor:write`.
pub fn edit(
    path: impl Into<String>,
    range: std::ops::Range<usize>,
    text: impl Into<String>,
) -> Result<(), String> {
    host::call(&Request::Edit {
        path: path.into(),
        start: range.start,
        end: range.end,
        text: text.into(),
    })
    .map(|_| ())
}

/// A file of the project, by its path from the project root. Needs `fs:read`.
pub fn read_file(path: impl Into<String>) -> Result<String, String> {
    reply(&Request::ReadFile { path: path.into() })
}

/// Sends a request. Needs `http:<host>` for the URL's host.
pub fn http(request: HttpRequest) -> Result<HttpResponse, String> {
    reply(&Request::Http(request))
}

/// `GET url`. Needs `http:<host>` for the URL's host.
pub fn get(url: impl Into<String>) -> Result<HttpResponse, String> {
    http(HttpRequest {
        method: "GET".into(),
        url: url.into(),
        ..Default::default()
    })
}

fn reply<T: serde::de::DeserializeOwned>(request: &Request) -> Result<T, String> {
    serde_json::from_value(host::call(request)?).map_err(|e| e.to_string())
}

/// Defines the plugin's entry point for the type that implements [`Plugin`].
#[macro_export]
macro_rules! register {
    ($plugin:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn solder_event(len: u32) {
            thread_local! {
                static PLUGIN: ::std::cell::RefCell<$plugin> = ::std::default::Default::default();
            }
            if let Some(event) = $crate::host::take_event(len) {
                PLUGIN.with(|plugin| $crate::Plugin::event(&mut *plugin.borrow_mut(), event));
            }
        }
    };
}

/// The two functions the editor gives a plugin: `call` passes a request and
/// returns the length of the answer, `read` copies what is waiting (an event
/// or an answer) into the plugin's memory.
#[doc(hidden)]
pub mod host {
    use super::{Event, Reply, Request};

    #[cfg(target_arch = "wasm32")]
    mod sys {
        #[link(wasm_import_module = "solder")]
        unsafe extern "C" {
            pub fn read(ptr: *mut u8, len: u32);
            pub fn call(ptr: *const u8, len: u32) -> u32;
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn take(len: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; len as usize];
        // The editor copies exactly `len` bytes into the buffer.
        unsafe { sys::read(bytes.as_mut_ptr(), len) };
        bytes
    }

    #[cfg(target_arch = "wasm32")]
    pub fn take_event(len: u32) -> Option<Event> {
        serde_json::from_slice(&take(len)).ok()
    }

    #[cfg(target_arch = "wasm32")]
    pub fn call(request: &Request) -> Reply {
        let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        let len = unsafe { sys::call(bytes.as_ptr(), bytes.len() as u32) };
        serde_json::from_slice::<Reply>(&take(len)).map_err(|e| e.to_string())?
    }

    // Outside the editor (a plugin's own unit tests) nothing answers.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn take_event(_len: u32) -> Option<Event> {
        None
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn call(_request: &Request) -> Reply {
        Err("Not running in Solder".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_keep_their_wire_names() {
        let event = Event::Open {
            path: "src/a.rs".into(),
            language: Some("Rust".into()),
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"event":"open","path":"src/a.rs","language":"Rust"}"#
        );
        assert_eq!(event.kind(), "open");
        let request: Request =
            serde_json::from_str(r#"{"call":"http","method":"GET","url":"https://x/y"}"#).unwrap();
        assert_eq!(
            request,
            Request::Http(HttpRequest {
                method: "GET".into(),
                url: "https://x/y".into(),
                ..Default::default()
            })
        );
        assert_eq!(
            serde_json::to_string(&Request::EditorText).unwrap(),
            r#"{"call":"editor_text"}"#
        );
        // A plugin run outside the editor gets refusals, not a crash.
        assert!(editor().is_err());
    }
}
