//! JavaScript plugins: `plugin.js` runs in QuickJS, itself a WebAssembly
//! module inside the same sandbox as any other plugin.
//!
//! The engine is a WASI program. It gets no files, no environment and no
//! clock it could wait on: only its three standard streams. On those the
//! plugin and the editor speak in lines. The editor writes an event to the
//! engine's input; the script handles it and reads again. A request is a
//! line on its output that starts with `\x01`; the reply is the next line
//! on its input. Anything else it prints goes to the plugin's log.
//!
//! When the script reads and nothing is waiting, the read stops the engine
//! where it is, and the next event resumes it there: that pause is the end
//! of one event.

use std::collections::VecDeque;

use serde_json::Value;
use solder_plugin::{Reply, Request};
use wasmi::{Caller, Linker, Memory, Store};

use crate::runtime::{MESSAGE_LIMIT, State, memory};

/// QuickJS-ng 0.17.0 for WASI, as released; see `assets/README.md`.
pub(crate) const ENGINE: &[u8] = include_bytes!("../assets/qjs-wasi.wasm");

/// Defines `solder` before the plugin's script and runs its events after.
const PRELUDE: &str = include_str!("prelude.js");

const MODULE: &str = "wasi_snapshot_preview1";

// WASI error numbers.
const OK: i32 = 0;
const BADF: i32 = 8;
const NOSYS: i32 = 52;
const SPIPE: i32 = 70;

/// A read the engine made with nothing to give it yet.
#[derive(Clone, Copy)]
struct Read {
    iovs: u32,
    count: u32,
    nread: u32,
}

/// The engine's standard streams.
pub(crate) struct Io {
    args: Vec<Vec<u8>>,
    input: VecDeque<u8>,
    /// Output not yet ended by a newline, for stdout and stderr.
    lines: [Vec<u8>; 2],
    waiting: Option<Read>,
    exit: Option<i32>,
    /// The last lines the engine printed that were not requests: why it
    /// ended, if it did.
    printed: VecDeque<String>,
    /// The text of the last `editor_text` reply, to turn the script's
    /// UTF-16 positions into the editor's bytes.
    text: Option<String>,
    /// What a handler threw during this event.
    failure: Option<String>,
}

impl Io {
    pub(crate) fn new(source: Option<&str>) -> Self {
        let args = match source {
            // `--std` gives the script `std` and `os`, which the prelude
            // uses and then takes away.
            Some(source) => vec![
                b"qjs".to_vec(),
                b"--std".to_vec(),
                b"-e".to_vec(),
                format!("{}\n{source}\n;__solder_run();", PRELUDE.trim_end()).into_bytes(),
            ],
            None => Vec::new(),
        };
        Self {
            args,
            input: VecDeque::new(),
            lines: [Vec::new(), Vec::new()],
            waiting: None,
            exit: None,
            printed: VecDeque::new(),
            text: None,
            failure: None,
        }
    }

    /// Queues `line` for the script to read.
    pub(crate) fn push_line(&mut self, line: &[u8]) {
        self.input.extend(line);
        self.input.push_back(b'\n');
    }

    /// Whether the script is parked in a read, waiting for an event.
    pub(crate) fn waiting(&self) -> bool {
        self.waiting.is_some()
    }

    pub(crate) fn take_failure(&mut self) -> Option<String> {
        self.failure.take()
    }

    /// Why a script that is no longer running stopped.
    pub(crate) fn ended(&self) -> String {
        // The engine prints what was thrown, then where: `Error: x`, and
        // `    at f (plugin.js:3:1)` under it.
        let mut lines = self.printed.iter().skip_while(|l| l.starts_with(' '));
        match (lines.next(), self.exit) {
            (Some(what), _) => {
                let place = lines.next().map(|l| l.trim()).unwrap_or_default();
                format!("The script failed: {what} {place}")
                    .trim_end()
                    .to_string()
            }
            (None, Some(code)) => format!("The script ended with code {code}"),
            (None, None) => "The script ended".into(),
        }
    }
}

/// A line the engine printed, with places in `<cmdline>` (the prelude and
/// the script as one text) named by the script's own lines.
fn relabel(line: &str) -> String {
    const MARK: &str = "<cmdline>:";
    let prelude = PRELUDE.trim_end().lines().count();
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find(MARK) {
        let after = &rest[at + MARK.len()..];
        let digits = after.chars().take_while(char::is_ascii_digit).count();
        out.push_str(&rest[..at]);
        match after[..digits].parse::<usize>() {
            Ok(n) if n > prelude => out.push_str(&format!("plugin.js:{}", n - prelude)),
            _ => out.push_str(&rest[at..at + MARK.len() + digits]),
        }
        rest = &after[digits..];
    }
    out + rest
}

/// Byte offset of UTF-16 position `units` in `text`, rounded down to a
/// character.
fn utf16_to_byte(text: &str, units: u64) -> usize {
    let mut seen = 0u64;
    for (byte, c) in text.char_indices() {
        if seen + c.len_utf16() as u64 > units {
            return byte;
        }
        seen += c.len_utf16() as u64;
    }
    text.len()
}

/// UTF-16 position of byte offset `byte` in `text`.
fn byte_to_utf16(text: &str, byte: u64) -> u64 {
    let byte = (byte as usize).min(text.len());
    text.char_indices()
        .take_while(|(at, _)| *at < byte)
        .map(|(_, c)| c.len_utf16() as u64)
        .sum()
}

/// Answers a request line from a script. JavaScript counts positions in
/// UTF-16 units and the editor in bytes, so both directions are converted
/// against the text the script last read.
fn answer(state: &mut State, line: &[u8]) -> Reply {
    let mut value: Value =
        serde_json::from_slice(line).map_err(|e| format!("Not a request: {e}"))?;
    if value["call"] == "edit" {
        let text = state.script.text.take().ok_or(
            "Call solder.editor() before solder.edit(): positions are in the text it returned",
        )?;
        for key in ["start", "end"] {
            let units = value[key].as_u64().ok_or("edit needs a start and an end")?;
            value[key] = utf16_to_byte(&text, units).into();
        }
    }
    let request: Request =
        serde_json::from_value(value).map_err(|e| format!("Not a request: {e}"))?;
    let reads_text = request == Request::EditorText;
    let mut reply = state.answer(request)?;
    if reads_text {
        let text = reply["text"].as_str().unwrap_or_default().to_string();
        for key in ["selection_start", "selection_end"] {
            let byte = reply[key].as_u64().unwrap_or(0);
            reply[key] = byte_to_utf16(&text, byte).into();
        }
        state.script.text = Some(text);
    }
    Ok(reply)
}

/// Takes complete lines out of what the script wrote: requests are
/// answered, the rest is logged.
fn written(state: &mut State, stream: usize, bytes: &[u8]) -> Result<(), wasmi::Error> {
    state.script.lines[stream].extend_from_slice(bytes);
    if state.script.lines[stream].len() > MESSAGE_LIMIT {
        return Err(wasmi::Error::new(
            "the script wrote a line that is too long",
        ));
    }
    while let Some(end) = state.script.lines[stream].iter().position(|b| *b == b'\n') {
        let line: Vec<u8> = state.script.lines[stream].drain(..=end).collect();
        let line = &line[..end];
        match line.split_first() {
            Some((1, request)) if stream == 0 => {
                let mut reply = serde_json::to_vec(&answer(state, request)).unwrap_or_default();
                if reply.len() > MESSAGE_LIMIT {
                    reply = serde_json::to_vec(&Reply::Err("The answer is too large".into()))
                        .unwrap_or_default();
                }
                state.script.push_line(&reply);
            }
            Some((2, thrown)) if stream == 0 => {
                let thrown = relabel(&String::from_utf8_lossy(thrown));
                state.log(thrown.clone());
                state.script.failure = Some(thrown);
            }
            _ => {
                let text = relabel(&String::from_utf8_lossy(line));
                if !text.trim().is_empty() {
                    state.script.printed.push_back(text.clone());
                    if state.script.printed.len() > 4 {
                        state.script.printed.pop_front();
                    }
                    state.log(text);
                }
            }
        }
    }
    Ok(())
}

fn read_u32(caller: &Caller<'_, State>, memory: Memory, at: u32) -> Result<u32, wasmi::Error> {
    let mut bytes = [0; 4];
    memory
        .read(caller, at as usize, &mut bytes)
        .map_err(|_| wasmi::Error::new("outside the engine's memory"))?;
    Ok(u32::from_le_bytes(bytes))
}

fn write(
    caller: &mut Caller<'_, State>,
    memory: Memory,
    at: u32,
    bytes: &[u8],
) -> Result<(), wasmi::Error> {
    memory
        .write(caller, at as usize, bytes)
        .map_err(|_| wasmi::Error::new("outside the engine's memory"))
}

/// Copies waiting input into the buffers of the read the script is parked
/// in, so the read can return.
pub(crate) fn finish_read(store: &mut Store<State>, memory: Memory) -> Result<(), wasmi::Error> {
    let Some(read) = store.data_mut().script.waiting.take() else {
        return Ok(());
    };
    let outside = |_| wasmi::Error::new("outside the engine's memory");
    let mut total = 0u32;
    for i in 0..read.count {
        let mut iov = [0u8; 8];
        memory
            .read(&*store, (read.iovs + 8 * i) as usize, &mut iov)
            .map_err(outside)?;
        let buf = u32::from_le_bytes([iov[0], iov[1], iov[2], iov[3]]);
        let len = u32::from_le_bytes([iov[4], iov[5], iov[6], iov[7]]) as usize;
        let input = &mut store.data_mut().script.input;
        let take = len.min(input.len());
        let bytes: Vec<u8> = input.drain(..take).collect();
        memory
            .write(&mut *store, buf as usize, &bytes)
            .map_err(outside)?;
        total += take as u32;
        if take < len {
            break;
        }
    }
    memory
        .write(&mut *store, read.nread as usize, &total.to_le_bytes())
        .map_err(outside)
}

/// The WASI functions the engine imports. Its streams work; everything that
/// would reach a file or wait answers that there is nothing there.
pub(crate) fn link(linker: &mut Linker<State>) -> Result<(), wasmi::errors::LinkerError> {
    linker.func_wrap(
        MODULE,
        "args_sizes_get",
        |mut caller: Caller<'_, State>, count: u32, size: u32| -> Result<i32, wasmi::Error> {
            let memory = memory(&caller)?;
            let args = &caller.data().script.args;
            let (n, bytes) = (
                args.len() as u32,
                args.iter().map(|a| a.len() as u32 + 1).sum::<u32>(),
            );
            write(&mut caller, memory, count, &n.to_le_bytes())?;
            write(&mut caller, memory, size, &bytes.to_le_bytes())?;
            Ok(OK)
        },
    )?;
    linker.func_wrap(
        MODULE,
        "args_get",
        |mut caller: Caller<'_, State>, argv: u32, buf: u32| -> Result<i32, wasmi::Error> {
            let memory = memory(&caller)?;
            let args = caller.data().script.args.clone();
            let mut at = buf;
            for (i, arg) in args.iter().enumerate() {
                write(&mut caller, memory, argv + 4 * i as u32, &at.to_le_bytes())?;
                write(&mut caller, memory, at, arg)?;
                write(&mut caller, memory, at + arg.len() as u32, &[0])?;
                at += arg.len() as u32 + 1;
            }
            Ok(OK)
        },
    )?;
    linker.func_wrap(
        MODULE,
        "environ_sizes_get",
        |mut caller: Caller<'_, State>, count: u32, size: u32| -> Result<i32, wasmi::Error> {
            let memory = memory(&caller)?;
            write(&mut caller, memory, count, &0u32.to_le_bytes())?;
            write(&mut caller, memory, size, &0u32.to_le_bytes())?;
            Ok(OK)
        },
    )?;
    linker.func_wrap(MODULE, "environ_get", |_: u32, _: u32| -> i32 { OK })?;
    linker.func_wrap(
        MODULE,
        "clock_time_get",
        |mut caller: Caller<'_, State>,
         _id: u32,
         _precision: u64,
         at: u32|
         -> Result<i32, wasmi::Error> {
            let memory = memory(&caller)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
            write(&mut caller, memory, at, &now.to_le_bytes())?;
            Ok(OK)
        },
    )?;
    linker.func_wrap(MODULE, "fd_close", |_: u32| -> i32 { OK })?;
    linker.func_wrap(
        MODULE,
        "fd_fdstat_get",
        |mut caller: Caller<'_, State>, fd: u32, at: u32| -> Result<i32, wasmi::Error> {
            if fd > 2 {
                return Ok(BADF);
            }
            let memory = memory(&caller)?;
            // A character device with every right.
            let mut stat = [0u8; 24];
            stat[0] = 2;
            stat[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
            write(&mut caller, memory, at, &stat)?;
            Ok(OK)
        },
    )?;
    linker.func_wrap(MODULE, "fd_fdstat_set_flags", |_: u32, _: u32| -> i32 {
        OK
    })?;
    // No folder is open to the engine, so no path leads anywhere.
    linker.func_wrap(MODULE, "fd_prestat_get", |_: u32, _: u32| -> i32 { BADF })?;
    linker.func_wrap(
        MODULE,
        "fd_prestat_dir_name",
        |_: u32, _: u32, _: u32| -> i32 { BADF },
    )?;
    linker.func_wrap(
        MODULE,
        "fd_read",
        |mut caller: Caller<'_, State>,
         fd: u32,
         iovs: u32,
         count: u32,
         nread: u32|
         -> Result<i32, wasmi::Error> {
            if fd != 0 {
                return Ok(BADF);
            }
            caller.data_mut().script.waiting = Some(Read { iovs, count, nread });
            if caller.data().script.input.is_empty() {
                // Nothing to read: the engine stops here, and the next
                // event resumes this read.
                return Err(wasmi::Error::new("waiting for an event"));
            }
            let memory = memory(&caller)?;
            // The same copy as when a parked read is resumed.
            let read = Read { iovs, count, nread };
            let mut total = 0u32;
            for i in 0..read.count {
                let buf = read_u32(&caller, memory, read.iovs + 8 * i)?;
                let len = read_u32(&caller, memory, read.iovs + 8 * i + 4)? as usize;
                let input = &mut caller.data_mut().script.input;
                let take = len.min(input.len());
                let bytes: Vec<u8> = input.drain(..take).collect();
                write(&mut caller, memory, buf, &bytes)?;
                total += take as u32;
                if take < len {
                    break;
                }
            }
            caller.data_mut().script.waiting = None;
            write(&mut caller, memory, read.nread, &total.to_le_bytes())?;
            Ok(OK)
        },
    )?;
    linker.func_wrap(
        MODULE,
        "fd_readdir",
        |_: u32, _: u32, _: u32, _: u64, _: u32| -> i32 { BADF },
    )?;
    linker.func_wrap(MODULE, "fd_seek", |_: u32, _: u64, _: u32, _: u32| -> i32 {
        SPIPE
    })?;
    linker.func_wrap(
        MODULE,
        "fd_write",
        |mut caller: Caller<'_, State>,
         fd: u32,
         iovs: u32,
         count: u32,
         nwritten: u32|
         -> Result<i32, wasmi::Error> {
            if !(1..=2).contains(&fd) {
                return Ok(BADF);
            }
            let memory = memory(&caller)?;
            let mut total = 0u32;
            for i in 0..count {
                let buf = read_u32(&caller, memory, iovs + 8 * i)?;
                let len = read_u32(&caller, memory, iovs + 8 * i + 4)?;
                if len as usize > MESSAGE_LIMIT {
                    return Err(wasmi::Error::new("the script wrote too much at once"));
                }
                let mut bytes = vec![0; len as usize];
                memory
                    .read(&caller, buf as usize, &mut bytes)
                    .map_err(|_| wasmi::Error::new("outside the engine's memory"))?;
                written(caller.data_mut(), fd as usize - 1, &bytes)?;
                total += len;
            }
            write(&mut caller, memory, nwritten, &total.to_le_bytes())?;
            Ok(OK)
        },
    )?;
    linker.func_wrap(
        MODULE,
        "path_create_directory",
        |_: u32, _: u32, _: u32| -> i32 { BADF },
    )?;
    linker.func_wrap(
        MODULE,
        "path_filestat_get",
        |_: u32, _: u32, _: u32, _: u32, _: u32| -> i32 { BADF },
    )?;
    linker.func_wrap(
        MODULE,
        "path_filestat_set_times",
        |_: u32, _: u32, _: u32, _: u32, _: u64, _: u64, _: u32| -> i32 { BADF },
    )?;
    linker.func_wrap(
        MODULE,
        "path_open",
        |_: u32, _: u32, _: u32, _: u32, _: u32, _: u64, _: u64, _: u32, _: u32| -> i32 { BADF },
    )?;
    linker.func_wrap(
        MODULE,
        "path_remove_directory",
        |_: u32, _: u32, _: u32| -> i32 { BADF },
    )?;
    linker.func_wrap(
        MODULE,
        "path_rename",
        |_: u32, _: u32, _: u32, _: u32, _: u32, _: u32| -> i32 { BADF },
    )?;
    linker.func_wrap(
        MODULE,
        "path_unlink_file",
        |_: u32, _: u32, _: u32| -> i32 { BADF },
    )?;
    // Waiting (timers, sleep) is not available to a plugin.
    linker.func_wrap(
        MODULE,
        "poll_oneoff",
        |_: u32, _: u32, _: u32, _: u32| -> i32 { NOSYS },
    )?;
    linker.func_wrap(
        MODULE,
        "proc_exit",
        |mut caller: Caller<'_, State>, code: u32| -> Result<(), wasmi::Error> {
            caller.data_mut().script.exit = Some(code as i32);
            Err(wasmi::Error::i32_exit(code as i32))
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_convert_between_utf16_and_bytes() {
        // `é` is one unit and two bytes, the emoji two units and four bytes.
        let text = "aé😀b";
        for (units, byte) in [(0, 0), (1, 1), (2, 3), (4, 7), (5, 8)] {
            assert_eq!(utf16_to_byte(text, units), byte, "{units} units");
            assert_eq!(byte_to_utf16(text, byte as u64), units, "{byte} bytes");
        }
        // Inside the emoji's pair, and past the end.
        assert_eq!(utf16_to_byte(text, 3), 3);
        assert_eq!(utf16_to_byte(text, 99), 8);
        assert_eq!(byte_to_utf16(text, 99), 5);
    }
}
