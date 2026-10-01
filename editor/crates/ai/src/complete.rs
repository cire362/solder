//! Code completions at the cursor, streamed as the model writes them.
//!
//! llama.cpp's server fills in the middle (`/infill`) for models trained to:
//! it sees the code before and after the cursor and writes what goes
//! between. Other providers have no such API, so they get a chat request
//! that asks for the missing text only, and the answer is cleaned of the
//! fences and repetition chat models add.

use std::{
    future::{Future, poll_fn},
    task::Poll,
    time::Duration,
};

use tokio::sync::mpsc;

use crate::provider::{self, Api, ChatRequest, Endpoint, Event, Message, SseParser, Who};

/// A completion stops after this many lines.
pub const MAX_LINES: usize = 16;

/// The error for a model that cannot fill in the middle; the caller asks it
/// through chat instead.
pub const NO_INFILL: &str = "This model cannot fill in the middle";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// llama.cpp's `/infill`.
    Infill,
    /// A chat request asking for the text at the cursor.
    Chat,
}

#[derive(Clone, Debug)]
pub struct FillRequest {
    pub model: String,
    /// The file's path from the project root, for the model's sake.
    pub path: String,
    pub prefix: String,
    pub suffix: String,
    pub max_tokens: u32,
}

const SYSTEM: &str = "You complete code in an editor. Reply with only the text that goes at \
<CURSOR>: no explanation, no code fences, and nothing that is already before or after the \
cursor. Reply with nothing if nothing fits.";

fn chat_request(req: &FillRequest) -> ChatRequest {
    ChatRequest {
        model: req.model.clone(),
        system: Some(SYSTEM.into()),
        messages: vec![Message {
            who: Who::User,
            text: format!(
                "File: {}\n```\n{}<CURSOR>{}\n```",
                req.path, req.prefix, req.suffix
            ),
        }],
        max_tokens: req.max_tokens,
    }
}

/// Turns a stream of text into what to insert: drops a leading fence and a
/// repeated start of the current line, and stops at a closing fence or after
/// `MAX_LINES` lines.
pub struct Cleaner {
    /// The current line up to the cursor, which chat models tend to repeat.
    line_before: String,
    /// The cursor is on a line of indentation only: models often start a
    /// new line there anyway, which would leave this one blank.
    blank_line: bool,
    pending: String,
    started: bool,
    lines: usize,
    /// A newline not shown yet: if a closing fence follows, it is dropped.
    held_newline: bool,
    /// At the start of a line, where a fence would close the answer.
    line_start: bool,
    pub done: bool,
}

impl Cleaner {
    pub fn new(line_before: &str) -> Self {
        Self {
            line_before: line_before.trim_start().to_string(),
            blank_line: line_before.trim().is_empty(),
            pending: String::new(),
            started: false,
            lines: 0,
            held_newline: false,
            line_start: false,
            done: false,
        }
    }

    /// Feeds model output; returns the text to show now.
    pub fn push(&mut self, text: &str) -> String {
        if self.done {
            return String::new();
        }
        self.pending.push_str(text);
        if !self.started {
            // Hold the start until it is clear it is neither an opening
            // fence nor the current line written again.
            let p = self.pending.trim_start();
            if "```".starts_with(p) || p.starts_with("```") && !p.contains('\n') {
                return String::new();
            }
            if p.starts_with("```") {
                let newline = self.pending.find('\n').unwrap_or(0);
                self.pending.drain(..=newline);
            }
            if self.blank_line && self.pending.starts_with('\n') {
                let rest = self.pending[1..].trim_start_matches([' ', '\t']);
                if rest.is_empty() {
                    return String::new();
                }
                self.pending = rest.to_string();
            }
            if !self.line_before.is_empty() {
                let p = self.pending.trim_start();
                if let Some(rest) = p.strip_prefix(&self.line_before) {
                    self.pending = rest.to_string();
                } else if self.line_before.starts_with(p) {
                    return String::new();
                }
            }
            self.started = true;
        }
        self.take_pending()
    }

    /// The text held back when the stream ends.
    pub fn finish(&mut self) -> String {
        if self.done || self.started {
            return String::new();
        }
        self.started = true;
        let p = self.pending.trim_start();
        if p.starts_with("```") || p == self.line_before {
            self.pending.clear();
        }
        self.take_pending()
    }

    fn take_pending(&mut self) -> String {
        let mut out = String::new();
        let text = std::mem::take(&mut self.pending);
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim_start();
            if self.line_start && trimmed.starts_with("```") {
                self.done = true;
                break;
            }
            if self.line_start && !line.ends_with('\n') && "```".starts_with(trimmed) {
                // Backticks that may become a closing fence: wait.
                if !trimmed.is_empty() {
                    self.pending = line.to_string();
                }
                break;
            }
            if self.held_newline {
                out.push('\n');
                self.held_newline = false;
            }
            match line.strip_suffix('\n') {
                Some(body) => {
                    out.push_str(body);
                    self.held_newline = true;
                    self.line_start = true;
                    self.lines += 1;
                    if self.lines >= MAX_LINES {
                        self.done = true;
                        break;
                    }
                }
                None => {
                    out.push_str(line);
                    self.line_start = false;
                }
            }
        }
        out
    }
}

/// Streams the text to insert at the cursor. Dropping the receiver stops the
/// request.
pub fn stream(
    endpoint: Endpoint,
    mode: Mode,
    req: FillRequest,
) -> mpsc::UnboundedReceiver<Result<String, String>> {
    let (tx, rx) = mpsc::unbounded_channel();
    let line_before = req.prefix.rsplit('\n').next().unwrap_or("").to_string();
    crate::runtime().spawn(async move {
        let work = async {
            let mut cleaner = Cleaner::new(&line_before);
            let send = |text: &str, cleaner: &mut Cleaner| -> bool {
                let out = cleaner.push(text);
                (out.is_empty() || tx.send(Ok(out)).is_ok()) && !cleaner.done
            };
            match mode {
                Mode::Infill => {
                    let base = endpoint.base_url.trim_end_matches('/');
                    let base = base.strip_suffix("/v1").unwrap_or(base);
                    let body = serde_json::json!({
                        "input_prefix": req.prefix,
                        "input_suffix": req.suffix,
                        "n_predict": req.max_tokens,
                        "temperature": 0.1,
                        "stream": true,
                        "cache_prompt": true,
                        "t_max_predict_ms": 3000,
                    });
                    let response = provider::request(&endpoint, &format!("{base}/infill"))
                        .body(body.to_string())
                        .timeout(Duration::from_secs(60))
                        .send()
                        .await
                        .map_err(|e| e.to_string())?;
                    let status = response.status().as_u16();
                    if matches!(status, 400 | 404 | 501) {
                        return Err(NO_INFILL.to_string());
                    }
                    if status >= 400 {
                        let text = response.text().await.unwrap_or_default();
                        return Err(provider::status_error(status, &text));
                    }
                    let mut response = response;
                    let mut parser = SseParser::default();
                    'stream: while let Some(chunk) =
                        response.chunk().await.map_err(|e| e.to_string())?
                    {
                        for data in parser.push(&chunk) {
                            let value: serde_json::Value =
                                serde_json::from_str(&data).map_err(|e| e.to_string())?;
                            if let Some(e) = value["error"]["message"].as_str() {
                                return Err(e.to_string());
                            }
                            let text = value["content"].as_str().unwrap_or("");
                            if !send(text, &mut cleaner) {
                                return Ok(());
                            }
                            if value["stop"] == true {
                                break 'stream;
                            }
                        }
                    }
                }
                Mode::Chat => {
                    let mut events = provider::stream(endpoint.clone(), chat_request(&req));
                    while let Some(event) = events.recv().await {
                        match event? {
                            Event::Text(text) => {
                                if !send(&text, &mut cleaner) {
                                    return Ok(());
                                }
                            }
                            Event::Thinking(_) => {}
                            Event::Done { .. } => break,
                        }
                    }
                }
            }
            let rest = cleaner.finish();
            if !rest.is_empty() {
                let _ = tx.send(Ok(rest));
            }
            Ok(())
        };
        let mut work = std::pin::pin!(work);
        let mut closed = std::pin::pin!(tx.closed());
        let result = poll_fn(|cx| {
            if closed.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Ok(()));
            }
            work.as_mut().poll(cx)
        })
        .await;
        if let Err(e) = result {
            let _ = tx.send(Err(e));
        }
    });
    rx
}

/// Which way to ask a provider: only llama.cpp fills in the middle.
pub fn mode_for(local: bool, api: Api) -> Mode {
    if local && api == Api::OpenAi {
        Mode::Infill
    } else {
        Mode::Chat
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};

    fn clean(line_before: &str, chunks: &[&str]) -> (String, bool) {
        let mut c = Cleaner::new(line_before);
        let out: String = chunks.iter().map(|t| c.push(t)).collect();
        (out, c.done)
    }

    #[test]
    fn cleans_chat_answers() {
        assert_eq!(clean("", &["a + b", "\n}"]), ("a + b\n}".into(), false));
        // On a line of indentation only, a leading new line is dropped.
        assert_eq!(
            clean("  ", &["\n", "  return a - b;\n", "}"]),
            ("return a - b;\n}".into(), false)
        );
        // Held back while it might repeat the line, then let go at the end.
        let mut c = Cleaner::new("count");
        assert_eq!(c.push("co"), "");
        assert_eq!(c.finish(), "co");
        assert_eq!(
            clean("", &["```", "rust\nlet x", " = 1;\n```\nmore"]),
            ("let x = 1;".into(), true)
        );
        // The model repeats the start of the line before writing the rest.
        assert_eq!(
            clean("    let total = ", &["let total = ", "items.len();\n"]),
            ("items.len();".into(), false)
        );
        let many: Vec<String> = (0..30).map(|i| format!("line{i}\n")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let (out, done) = clean("", &refs);
        assert!(done);
        assert_eq!(out.lines().count(), MAX_LINES);
        assert!(!out.ends_with('\n'));
    }

    /// A llama.cpp-like server: `/infill` streams `pieces`, or answers 501
    /// when `fim` is false; `/v1/chat/completions` streams a fenced answer.
    fn server(
        fim: bool,
        pieces: &'static [&'static str],
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap_or(0);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).ok();
                log.lock().unwrap().push(format!(
                    "{}\n{}",
                    first.trim(),
                    String::from_utf8_lossy(&body)
                ));
                if first.contains("/infill") && !fim {
                    let json = r#"{"error":{"message":"infill not supported"}}"#;
                    let _ = write!(
                        stream,
                        "HTTP/1.1 501 Not Implemented\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                        json.len()
                    );
                    continue;
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
                );
                if first.contains("/infill") {
                    for (i, p) in pieces.iter().enumerate() {
                        let stop = i + 1 == pieces.len();
                        let data = serde_json::json!({"content": p, "stop": stop});
                        let _ = write!(stream, "data: {data}\n\n");
                    }
                } else {
                    for p in ["```js\n", "return a + b;\n", "```"] {
                        let data = serde_json::json!({"choices":[{"delta":{"content": p}}]});
                        let _ = write!(stream, "data: {data}\n\n");
                    }
                    let _ = write!(stream, "data: [DONE]\n\n");
                }
            }
        });
        (base, seen)
    }

    fn collect(base: String, mode: Mode) -> Vec<Result<String, String>> {
        let mut rx = stream(
            Endpoint {
                api: Api::OpenAi,
                base_url: base,
                key: Some("k".into()),
            },
            mode,
            FillRequest {
                model: "m".into(),
                path: "src/add.js".into(),
                prefix: "function add(a, b) {\n  ".into(),
                suffix: "\n}\n".into(),
                max_tokens: 64,
            },
        );
        crate::runtime().block_on(async move {
            let mut out = Vec::new();
            while let Some(e) = rx.recv().await {
                out.push(e);
            }
            out
        })
    }

    #[test]
    fn fills_in_the_middle_with_llama_cpp() {
        let (base, seen) = server(true, &["return", " a + b;"]);
        let out = collect(base, Mode::Infill);
        assert_eq!(out, [Ok("return".to_string()), Ok(" a + b;".to_string())]);
        let request = seen.lock().unwrap()[0].clone();
        assert!(request.starts_with("POST /infill "), "{request}");
        assert!(request.contains(r#""input_suffix":"\n}\n""#), "{request}");
    }

    #[test]
    fn says_when_a_model_cannot_fill_and_falls_back_to_chat() {
        let (base, seen) = server(false, &[]);
        assert_eq!(
            collect(base.clone(), Mode::Infill),
            [Err(NO_INFILL.to_string())]
        );
        let out: String = collect(base, Mode::Chat)
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(out, "return a + b;");
        let request = seen.lock().unwrap()[1].clone();
        assert!(request.contains("<CURSOR>"), "{request}");
        assert_eq!(mode_for(true, Api::OpenAi), Mode::Infill);
        assert_eq!(mode_for(false, Api::OpenAi), Mode::Chat);
    }
}
