//! Chat with a model, streamed token by token, over the two APIs everything
//! speaks: OpenAI's chat completions (llama.cpp's server, Ollama, LM Studio,
//! OpenAI and compatible services) and Anthropic's messages.

use std::{
    future::{Future, poll_fn},
    task::Poll,
    time::Duration,
};

use tokio::sync::mpsc;

use crate::client;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Api {
    OpenAi,
    Anthropic,
}

/// Where to send requests.
#[derive(Clone, Debug, PartialEq)]
pub struct Endpoint {
    pub api: Api,
    /// Up to and including the version: `http://127.0.0.1:8080/v1`,
    /// `https://api.anthropic.com/v1`.
    pub base_url: String,
    pub key: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub who: Who,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Text(String),
    /// A reasoning model's thinking, shown apart from the answer.
    Thinking(String),
    Done {
        input: u64,
        output: u64,
    },
}

/// Turns server-sent events into their `data` payloads.
#[derive(Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    data: Vec<String>,
}

impl SseParser {
    /// Feeds bytes; returns each complete event's data.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.data
                    .push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
            // `event:`, `id:` and comments carry nothing the payload lacks.
        }
        events
    }
}

fn chat_url(endpoint: &Endpoint) -> String {
    let base = endpoint.base_url.trim_end_matches('/');
    match endpoint.api {
        Api::OpenAi => format!("{base}/chat/completions"),
        Api::Anthropic => format!("{base}/messages"),
    }
}

pub(crate) fn request(endpoint: &Endpoint, url: &str) -> reqwest::RequestBuilder {
    let mut builder = client()
        .post(url)
        .header("Content-Type", "application/json");
    match (endpoint.api, &endpoint.key) {
        (Api::OpenAi, Some(key)) => builder = builder.bearer_auth(key),
        (Api::Anthropic, Some(key)) => builder = builder.header("x-api-key", key),
        _ => {}
    }
    if endpoint.api == Api::Anthropic {
        builder = builder.header("anthropic-version", "2023-06-01");
    }
    builder
}

fn body(endpoint: &Endpoint, req: &ChatRequest) -> serde_json::Value {
    let role = |who: Who| match who {
        Who::User => "user",
        Who::Assistant => "assistant",
    };
    match endpoint.api {
        Api::OpenAi => {
            let mut messages = Vec::new();
            if let Some(system) = &req.system {
                messages.push(serde_json::json!({ "role": "system", "content": system }));
            }
            messages.extend(
                req.messages
                    .iter()
                    .map(|m| serde_json::json!({ "role": role(m.who), "content": m.text })),
            );
            serde_json::json!({
                "model": req.model,
                "messages": messages,
                "max_tokens": req.max_tokens,
                "stream": true,
                "stream_options": { "include_usage": true },
            })
        }
        Api::Anthropic => {
            let mut value = serde_json::json!({
                "model": req.model,
                "max_tokens": req.max_tokens,
                "stream": true,
                "messages": req.messages.iter()
                    .map(|m| serde_json::json!({ "role": role(m.who), "content": m.text }))
                    .collect::<Vec<_>>(),
            });
            if let Some(system) = &req.system {
                value["system"] = system.clone().into();
            }
            value
        }
    }
}

/// One event's payload as zero or more events, or the error it reports.
fn decode(api: Api, data: &str) -> Result<Vec<Event>, String> {
    if data == "[DONE]" {
        return Ok(Vec::new());
    }
    let value: serde_json::Value =
        serde_json::from_str(data).map_err(|e| format!("Bad answer from the model: {e}"))?;
    if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
        let message = error["message"]
            .as_str()
            .or(error.as_str())
            .unwrap_or("error");
        return Err(message.to_string());
    }
    let mut out = Vec::new();
    match api {
        Api::OpenAi => {
            let delta = &value["choices"][0]["delta"];
            for key in ["reasoning_content", "reasoning"] {
                if let Some(t) = delta[key].as_str().filter(|t| !t.is_empty()) {
                    out.push(Event::Thinking(t.into()));
                }
            }
            if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
                out.push(Event::Text(t.into()));
            }
            let usage = &value["usage"];
            if usage.is_object() {
                out.push(Event::Done {
                    input: usage["prompt_tokens"].as_u64().unwrap_or(0),
                    output: usage["completion_tokens"].as_u64().unwrap_or(0),
                });
            }
        }
        Api::Anthropic => match value["type"].as_str() {
            Some("content_block_delta") => {
                let delta = &value["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => out.push(Event::Text(
                        delta["text"].as_str().unwrap_or_default().into(),
                    )),
                    Some("thinking_delta") => out.push(Event::Thinking(
                        delta["thinking"].as_str().unwrap_or_default().into(),
                    )),
                    _ => {}
                }
            }
            Some("message_delta") => {
                let usage = &value["usage"];
                out.push(Event::Done {
                    input: usage["input_tokens"].as_u64().unwrap_or(0),
                    output: usage["output_tokens"].as_u64().unwrap_or(0),
                });
            }
            _ => {}
        },
    }
    Ok(out)
}

/// What a failed request means, in a sentence.
pub(crate) fn status_error(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .or(v["error"].as_str())
                .or(v["message"].as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(200).collect());
    let lead = match status {
        401 | 403 => "The key was rejected",
        404 => "The model or address was not found",
        429 => "Rate limited; try again shortly",
        500..=599 => "The provider had an error",
        _ => "The request failed",
    };
    if detail.trim().is_empty() {
        format!("{lead} (HTTP {status})")
    } else {
        format!("{lead} (HTTP {status}): {}", detail.trim())
    }
}

/// Streams a chat answer. Events arrive on the receiver as the model writes;
/// dropping the receiver stops the request.
pub fn stream(
    endpoint: Endpoint,
    req: ChatRequest,
) -> mpsc::UnboundedReceiver<Result<Event, String>> {
    stream_inner(endpoint, req, false)
}

pub fn stream_edit(
    endpoint: Endpoint,
    req: ChatRequest,
) -> mpsc::UnboundedReceiver<Result<Event, String>> {
    stream_inner(endpoint, req, true)
}

fn edit_complete(api: Api, data: &str) -> Result<bool, String> {
    if data == "[DONE]" {
        return Ok(api == Api::OpenAi);
    }
    let value: serde_json::Value = serde_json::from_str(data).map_err(|error| error.to_string())?;
    let reason = match api {
        Api::OpenAi => value["choices"][0]["finish_reason"].as_str(),
        Api::Anthropic => value["delta"]["stop_reason"].as_str(),
    };
    if reason.is_some_and(|reason| !matches!(reason, "stop" | "end_turn" | "stop_sequence")) {
        return Err("The model did not finish the edit. Try a smaller selection.".into());
    }
    Ok(api == Api::Anthropic && value["type"] == "message_stop")
}

fn stream_inner(
    endpoint: Endpoint,
    req: ChatRequest,
    complete_required: bool,
) -> mpsc::UnboundedReceiver<Result<Event, String>> {
    let (tx, rx) = mpsc::unbounded_channel();
    crate::runtime().spawn(async move {
        let result = async {
            let response = request(&endpoint, &chat_url(&endpoint))
                .body(body(&endpoint, &req).to_string())
                .timeout(Duration::from_secs(600))
                .send()
                .await
                .map_err(|e| {
                    if e.is_connect() {
                        format!("Could not connect to {}", endpoint.base_url)
                    } else {
                        e.to_string()
                    }
                })?;
            let status = response.status().as_u16();
            if status >= 400 {
                let text = response.text().await.unwrap_or_default();
                return Err(status_error(status, &text));
            }
            let mut response = response;
            let mut parser = SseParser::default();
            let mut done = false;
            let mut complete = false;
            'chunks: while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
                for data in parser.push(&chunk) {
                    if complete_required {
                        complete |= edit_complete(endpoint.api, &data)?;
                    }
                    for event in decode(endpoint.api, &data)? {
                        done |= matches!(event, Event::Done { .. });
                        if tx.send(Ok(event)).is_err() {
                            return Ok(());
                        }
                    }
                    if complete_required && complete {
                        break 'chunks;
                    }
                }
            }
            if complete_required && !complete {
                return Err("The model disconnected before completing the edit.".into());
            }
            if !done {
                let _ = tx.send(Ok(Event::Done {
                    input: 0,
                    output: 0,
                }));
            }
            Ok(())
        };
        let mut result = std::pin::pin!(result);
        let mut closed = std::pin::pin!(tx.closed());
        let result = poll_fn(|cx| {
            if closed.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Ok(()));
            }
            result.as_mut().poll(cx)
        })
        .await;
        if let Err(e) = result {
            let _ = tx.send(Err(e));
        }
    });
    rx
}

/// The models a provider offers, by id.
pub async fn list_models(endpoint: Endpoint) -> Result<Vec<String>, String> {
    let url = format!("{}/models", endpoint.base_url.trim_end_matches('/'));
    let mut builder = client().get(&url).timeout(Duration::from_secs(10));
    match (endpoint.api, &endpoint.key) {
        (Api::OpenAi, Some(key)) => builder = builder.bearer_auth(key),
        (Api::Anthropic, Some(key)) => {
            builder = builder
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
        }
        _ => {}
    }
    let response = builder.send().await.map_err(|e| {
        if e.is_connect() {
            "Not running".to_string()
        } else {
            e.to_string()
        }
    })?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if status >= 400 {
        return Err(status_error(status, &text));
    }
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut ids: Vec<String> = value["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect();
    if endpoint.api == Api::OpenAi && endpoint.base_url.contains("api.openai.com") {
        // The list includes speech, image and embedding models.
        ids.retain(|id| {
            ![
                "whisper",
                "tts",
                "dall-e",
                "embedding",
                "moderation",
                "image",
                "audio",
                "realtime",
                "transcribe",
                "search",
            ]
            .iter()
            .any(|skip| id.contains(skip))
        });
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};

    #[test]
    fn parses_server_sent_events_across_chunks() {
        let mut p = SseParser::default();
        assert!(p.push(b"event: x\ndata: {\"a\"").is_empty());
        assert_eq!(
            p.push(b":1}\n\n: comment\ndata: [DONE]\r\n\r\n"),
            ["{\"a\":1}", "[DONE]"]
        );
        assert_eq!(p.push(b"data: one\ndata: two\n\n"), ["one\ntwo"]);
    }

    #[test]
    fn edits_require_the_protocol_end_and_reject_token_limits_and_tool_calls() {
        assert!(edit_complete(Api::OpenAi, "[DONE]").unwrap());
        assert!(edit_complete(Api::Anthropic, r#"{"type":"message_stop"}"#).unwrap());
        assert!(!edit_complete(Api::Anthropic, r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":10}}"#).unwrap());
        assert!(!edit_complete(Api::OpenAi, r#"{"choices":[{"finish_reason":"stop"}]}"#).unwrap());
        for reason in ["length", "tool_calls", "content_filter"] {
            assert!(
                edit_complete(
                    Api::OpenAi,
                    &serde_json::json!({"choices":[{"finish_reason":reason}]}).to_string()
                )
                .is_err()
            );
        }
        assert!(
            edit_complete(
                Api::Anthropic,
                r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#
            )
            .is_err()
        );
        assert!(!edit_complete(Api::Anthropic, "[DONE]").unwrap());
        assert!(!edit_complete(Api::OpenAi, r#"{"type":"message_stop"}"#).unwrap());
    }

    #[test]
    fn preserves_unicode_across_every_chunk_boundary() {
        let payload = "data: Привет🙂\r\ndata: 世界\r\n\r\n".as_bytes();
        for boundary in 0..=payload.len() {
            let mut parser = SseParser::default();
            let mut events = parser.push(&payload[..boundary]);
            events.extend(parser.push(&payload[boundary..]));
            assert_eq!(events, ["Привет🙂\n世界"], "boundary {boundary}");
        }
        let mut parser = SseParser::default();
        let events: Vec<_> = payload
            .chunks(1)
            .flat_map(|chunk| parser.push(chunk))
            .collect();
        assert_eq!(events, ["Привет🙂\n世界"]);
    }

    fn assert_cancellation_closes_request(response: &'static str, event: Option<Event>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            connection
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(connection.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            connection.write_all(response.as_bytes()).unwrap();
            connection.flush().unwrap();
            ready_tx.send(()).unwrap();
            cancelled_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let mut byte = [0];
            let result = reader.read(&mut byte);
            matches!(result, Ok(0))
                || result.is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionReset)
        });
        let mut receiver = stream(
            Endpoint {
                api: Api::OpenAi,
                base_url,
                key: None,
            },
            ChatRequest {
                model: "test".into(),
                system: None,
                messages: Vec::new(),
                max_tokens: 16,
            },
        );
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if let Some(event) = event {
            let received = crate::runtime().block_on(async {
                tokio::time::timeout(Duration::from_secs(5), receiver.recv())
                    .await
                    .unwrap()
            });
            assert_eq!(received, Some(Ok(event)));
        }
        drop(receiver);
        cancelled_tx.send(()).unwrap();
        assert!(
            server.join().unwrap(),
            "The cancelled HTTP request stayed open"
        );
    }

    #[test]
    fn cancelling_before_response_headers_closes_the_request() {
        assert_cancellation_closes_request("", None);
    }

    #[test]
    fn cancelling_while_reading_an_error_closes_the_request() {
        assert_cancellation_closes_request(
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial",
            None,
        );
    }

    #[test]
    fn cancelling_while_the_stream_is_silent_closes_the_request() {
        assert_cancellation_closes_request(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
            Some(Event::Text("Hi".into())),
        );
    }

    #[test]
    fn decodes_both_apis() {
        let openai = r#"{"choices":[{"delta":{"content":"Hi","reasoning_content":"hmm"}}]}"#;
        assert_eq!(
            decode(Api::OpenAi, openai).unwrap(),
            [Event::Thinking("hmm".into()), Event::Text("Hi".into())]
        );
        let usage = r#"{"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":7}}"#;
        assert_eq!(
            decode(Api::OpenAi, usage).unwrap(),
            [Event::Done {
                input: 5,
                output: 7
            }]
        );
        assert!(decode(Api::OpenAi, "[DONE]").unwrap().is_empty());
        let anthropic =
            r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"Yo"}}"#;
        assert_eq!(
            decode(Api::Anthropic, anthropic).unwrap(),
            [Event::Text("Yo".into())]
        );
        let err = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert_eq!(decode(Api::Anthropic, err).unwrap_err(), "Overloaded");
        assert_eq!(
            status_error(401, r#"{"error":{"message":"invalid x-api-key"}}"#),
            "The key was rejected (HTTP 401): invalid x-api-key"
        );
    }

    /// A chat server that streams `words` back for any request and records
    /// each request's headers and body. Returns its base URL.
    pub fn chat_server(
        api: Api,
        words: &'static [&'static str],
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                    head.push_str(&line);
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).ok();
                log.lock()
                    .unwrap()
                    .push(format!("{head}\n{}", String::from_utf8_lossy(&body)));
                if head.starts_with("GET") {
                    let json = r#"{"data":[{"id":"model-b"},{"id":"model-a"}]}"#;
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                        json.len()
                    );
                    continue;
                }
                if String::from_utf8_lossy(&body).contains("\"bad-model\"") {
                    let json = r#"{"error":{"message":"model not found"}}"#;
                    let _ = write!(
                        stream,
                        "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                        json.len()
                    );
                    continue;
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
                );
                for w in words {
                    let data = match api {
                        Api::OpenAi => serde_json::json!({"choices":[{"delta":{"content": w}}]}).to_string(),
                        Api::Anthropic => serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text": w}}).to_string(),
                    };
                    let _ = write!(stream, "data: {data}\n\n");
                    let _ = stream.flush();
                    std::thread::sleep(Duration::from_millis(5));
                }
                let end = match api {
                    Api::OpenAi => "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n".to_string(),
                    Api::Anthropic => "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":2}}\n\ndata: {\"type\":\"message_stop\"}\n\n".to_string(),
                };
                let _ = write!(stream, "{end}");
            }
        });
        (base, seen)
    }

    fn collect(endpoint: Endpoint, model: &str) -> Vec<Result<Event, String>> {
        let mut rx = stream(
            endpoint,
            ChatRequest {
                model: model.into(),
                system: Some("Be brief.".into()),
                messages: vec![Message {
                    who: Who::User,
                    text: "Hello".into(),
                }],
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
    fn complete_edits_stream_from_both_provider_protocols() {
        for api in [Api::OpenAi, Api::Anthropic] {
            let (base_url, _) = chat_server(api, &["```rs\n", "new();\n", "```"]);
            let mut receiver = stream_edit(
                Endpoint {
                    api,
                    base_url,
                    key: None,
                },
                ChatRequest {
                    model: "model-a".into(),
                    system: None,
                    messages: vec![],
                    max_tokens: 64,
                },
            );
            let answer = crate::runtime().block_on(async move {
                let mut answer = String::new();
                while let Some(event) =
                    tokio::time::timeout(Duration::from_secs(5), receiver.recv())
                        .await
                        .unwrap()
                {
                    if let Event::Text(text) = event.unwrap() {
                        answer.push_str(&text);
                    }
                }
                answer
            });
            assert_eq!(
                crate::edit::replacement(&answer, "old\n").unwrap(),
                "new();\n"
            );
        }
    }

    #[test]
    fn streams_from_openai_compatible_servers() {
        let (base, seen) = chat_server(Api::OpenAi, &["Hel", "lo"]);
        let endpoint = Endpoint {
            api: Api::OpenAi,
            base_url: base.clone(),
            key: Some("sk-test".into()),
        };
        let events = collect(endpoint.clone(), "m");
        assert_eq!(
            events,
            [
                Ok(Event::Text("Hel".into())),
                Ok(Event::Text("lo".into())),
                Ok(Event::Done {
                    input: 3,
                    output: 2
                })
            ]
        );
        let request = seen.lock().unwrap()[0].clone();
        assert!(
            request.starts_with("POST /v1/chat/completions"),
            "{request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-test")
        );
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\n\n").unwrap().1.trim()).unwrap();
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "Be brief.");
        assert_eq!(body["stream"], true);

        let events = collect(endpoint.clone(), "bad-model");
        assert_eq!(
            events,
            [Err(
                "The model or address was not found (HTTP 404): model not found".into()
            )]
        );
        let models = crate::runtime().block_on(list_models(endpoint)).unwrap();
        assert_eq!(models, ["model-a", "model-b"]);

        let down = Endpoint {
            api: Api::OpenAi,
            base_url: "http://127.0.0.1:9/v1".into(),
            key: None,
        };
        assert!(
            matches!(&collect(down.clone(), "m")[..], [Err(e)] if e.contains("Could not connect"))
        );
        assert_eq!(
            crate::runtime().block_on(list_models(down)).unwrap_err(),
            "Not running"
        );
    }

    #[test]
    fn streams_from_anthropic() {
        let (base, seen) = chat_server(Api::Anthropic, &["Hi", "!"]);
        let events = collect(
            Endpoint {
                api: Api::Anthropic,
                base_url: base,
                key: Some("sk-ant".into()),
            },
            "claude",
        );
        assert_eq!(
            events,
            [
                Ok(Event::Text("Hi".into())),
                Ok(Event::Text("!".into())),
                Ok(Event::Done {
                    input: 0,
                    output: 2
                })
            ]
        );
        let request = seen.lock().unwrap()[0].clone().to_ascii_lowercase();
        assert!(request.starts_with("post /v1/messages"), "{request}");
        assert!(request.contains("x-api-key: sk-ant"));
        assert!(request.contains("anthropic-version: 2023-06-01"));
        assert!(request.contains("be brief."), "{request}");
    }
}
