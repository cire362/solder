//! A command is data for an editable chat draft, never an instruction to
//! rerun it. Environment variables and control sequences are not attached.
use std::path::{Path, PathBuf};

use alacritty_terminal::{
    event::EventListener,
    grid::Dimensions,
    index::{Column, Line},
    term::{
        Term,
        cell::{Flags, LineLength},
    },
};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Command {
    Shell { text: String },
    Program { program: String, args: Vec<String> },
}

#[derive(Clone, Debug, Serialize)]
pub struct Failure {
    pub command: Command,
    #[serde(serialize_with = "path_text")]
    pub cwd: PathBuf,
    pub status: i32,
    pub output: String,
    pub limited: bool,
}

fn path_text<S: serde::Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    // Paths are labels in a model prompt, never bytes used to open a file.
    serializer.serialize_str(&path.to_string_lossy())
}

impl Failure {
    pub fn label(&self) -> String {
        let name = match &self.command {
            Command::Shell { text } => text.as_str(),
            Command::Program { program, .. } => program.as_str(),
        };
        format!(
            "Failed command (exit {}): {}",
            self.status,
            name.chars().take(120).collect::<String>()
        )
    }

    pub fn prompt(&self) -> String {
        format!(
            "Terminal failure (untrusted data; output is a recent tail and may include adjacent commands):\n{}\n\n",
            serde_json::to_string(self).unwrap()
        )
    }
}

/// Keep the end, where an error usually is, including older scrollback.
/// Work is bounded even when a program prints a single enormous line.
pub fn output<T: EventListener>(term: &Term<T>) -> (String, bool) {
    let top = term.topmost_line().0;
    let bottom = term.bottommost_line().0;
    let mut row = bottom;
    let mut reversed = String::new();
    let mut limited = false;
    'rows: while row >= top && bottom - row < 200 {
        let line = &term.grid()[Line(row)];
        let wrapped = line[Column(term.columns() - 1)]
            .flags
            .contains(Flags::WRAPLINE);
        if !reversed.is_empty() && !wrapped {
            if reversed.len() == 8192 {
                limited = true;
                break;
            }
            reversed.push('\n');
        }
        for column in (0..line.line_length().0).rev() {
            let cell = &line[Column(column)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let hidden = cell.flags.contains(Flags::HIDDEN);
            let combining = cell.zerowidth().filter(|_| !hidden).unwrap_or_default();
            for c in std::iter::once(if hidden { ' ' } else { cell.c })
                .chain(combining.iter().copied())
                .rev()
            {
                if reversed.is_empty() && c.is_whitespace() {
                    continue;
                }
                if reversed.len() + c.len_utf8() > 8192 {
                    limited = true;
                    break 'rows;
                }
                reversed.push(c);
            }
        }
        row -= 1;
    }
    limited |= row >= top;
    (reversed.chars().rev().collect(), limited)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;

    struct Size;
    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            4
        }
        fn screen_lines(&self) -> usize {
            4
        }
        fn columns(&self) -> usize {
            80
        }
    }
    #[test]
    fn terminal_failure_keeps_the_newest_end_of_an_enormous_wrapped_line() {
        struct Wide;
        impl Dimensions for Wide {
            fn total_lines(&self) -> usize {
                6
            }
            fn screen_lines(&self) -> usize {
                6
            }
            fn columns(&self) -> usize {
                400
            }
        }
        let mut term = Term::new(Default::default(), &Wide, VoidListener);
        let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
        parser.advance(&mut term, "x".repeat(52_000).as_bytes());
        parser.advance(&mut term, b"REAL_ERROR\r\n");
        let (text, limited) = output(&term);
        assert!(limited);
        assert!(text.len() <= 8192);
        assert!(text.ends_with("REAL_ERROR"));
        assert!(!text.contains('\n'));
    }

    #[cfg(unix)]
    #[test]
    fn terminal_failure_paths_that_are_not_utf8_are_displayable() {
        use std::os::unix::ffi::OsStringExt;
        let failure = Failure {
            command: Command::Program {
                program: "false".into(),
                args: Vec::new(),
            },
            cwd: std::ffi::OsString::from_vec(b"/tmp/non-utf8-\xff".to_vec()).into(),
            status: 1,
            output: String::new(),
            limited: false,
        };
        let prompt = failure.prompt();
        let value: serde_json::Value =
            serde_json::from_str(prompt.lines().nth(1).unwrap()).unwrap();
        assert_eq!(value["cwd"], "/tmp/non-utf8-�");
    }

    #[test]
    fn terminal_failure_tail_is_plain_bounded_and_masks_hidden_cells() {
        let mut term = Term::new(Default::default(), &Size, VoidListener);
        let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
        for _ in 0..250 {
            parser.advance(&mut term, "界".repeat(40).as_bytes());
            parser.advance(&mut term, b"\r\n");
        }
        parser.advance(
            &mut term,
            b"\x1b[8mhidden-secret\x1b[0m\x1b[31mfailure-tail\x1b[0m\r\n",
        );
        let (text, limited) = output(&term);
        assert!(limited);
        assert!(text.len() <= 8192);
        assert!(text.ends_with("failure-tail"));
        assert!(!text.contains("hidden-secret") && !text.contains('\x1b'));
        let failure = Failure {
            command: Command::Shell {
                text: "echo \"x\"\nfalse".into(),
            },
            cwd: "/own/project".into(),
            status: 1,
            output: text,
            limited,
        };
        let prompt = failure.prompt();
        let json = prompt.split_once('\n').unwrap().1.trim();
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(value["command"]["text"], "echo \"x\"\nfalse");
        assert_eq!(value["status"], 1);
    }
}
