//! What other editors keep, read into what Solder understands: settings,
//! themes and key bindings of VS Code, Cursor, Zed and the JetBrains IDEs.
//!
//! Nothing here writes. `source::detect` finds the editors, `source::plan`
//! reads what one of them has, and the app applies what the user picked.

pub mod jsonc;
pub mod keymap;
pub mod plist;
pub mod source;
pub mod theme;

pub use keymap::{Binding, Converted};
pub use source::{Hint, Kind, Plan, Roots, Setting, Source, detect, plan};
pub use theme::ThemeFile;
