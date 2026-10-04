//! The example plugin: the word count of the file in front, in the status
//! bar, and a command that writes it at the cursor.
//!
//!     cargo build --release --target wasm32-unknown-unknown
//!
//! then copy `plugin.json` and the built `word_count.wasm` (as `plugin.wasm`)
//! into a folder under Solder's `plugins` folder.

use solder_plugin::{Event, Plugin};

#[derive(Default)]
struct WordCount;

fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

fn label(count: usize) -> String {
    match count {
        1 => "1 word".into(),
        n => format!("{n} words"),
    }
}

impl Plugin for WordCount {
    fn event(&mut self, event: Event) {
        let Ok(editor) = solder_plugin::editor() else {
            return;
        };
        let count = label(words(&editor.text));
        match event {
            Event::Command { id } if id == "insert" => {
                if let Some(path) = editor.path {
                    let _ = solder_plugin::edit(
                        path,
                        editor.selection_start..editor.selection_end,
                        count,
                    );
                }
            }
            _ => solder_plugin::status(count),
        }
    }
}

solder_plugin::register!(WordCount);
