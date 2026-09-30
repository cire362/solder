//! Text storage for the editor core.
//!
//! Positions are byte offsets into UTF-8 text. That is what tree-sitter, the text
//! shaper and the file on disk all speak, so there is exactly one conversion
//! (bytes to chars) and it happens inside the rope.

mod buffer;
pub mod diff;
mod history;
mod movement;

pub use buffer::{Buffer, Edit, Point};
pub use history::History;
pub use movement::{Selection, SelectionGoal, TAB_SIZE};
pub use ropey::Rope;
