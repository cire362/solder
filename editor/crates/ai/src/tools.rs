//! Tool use: one step of an agent. The model gets the conversation and the
//! tools it may call, and answers with text, tool calls, or both. The
//! caller runs the calls and sends their results in the next step.
//!
//! Steps are not streamed: a step's tool calls are only usable whole, and
//! both APIs stream them as fragments of JSON.

use std::time::Duration;

use crate::provider::{self, Api, Endpoint};

#[derive(Clone, Debug, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the input object.
    pub parameters: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub id: String,
    pub output: String,
    pub error: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Turn {
    User(String),
    Assistant { text: String, calls: Vec<ToolCall> },
    Results(Vec<ToolResult>),
}

#[derive(Clone, Debug)]
pub struct StepRequest {
    pub model: String,
    pub system: String,
    pub turns: Vec<Turn>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The model is done for now: it answered without calling tools.
    End,
    ToolUse,
    /// The answer was cut at `max_tokens`.
    Length,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub stop: Stop,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

fn openai_body(req: &StepRequest) -> serde_json::Value {
    let mut messages = vec![serde_json::json!({ "role": "system", "content": req.system })];
    for turn in &req.turns {
        match turn {
            Turn::User(text) => {
                messages.push(serde_json::json!({ "role": "user", "content": text }))
            }
            Turn::Assistant { text, calls } => {
                let mut m = serde_json::json!({ "role": "assistant", "content": text });
                if !calls.is_empty() {
                    m["tool_calls"] = calls
                        .iter()
                        .map(|c| {
                            serde_json::json!({
                                "id": c.id,
                                "type": "function",
                                "function": { "name": c.name, "arguments": c.input.to_string() },
                            })
                        })
                        .collect();
                }
                messages.push(m);
            }
            Turn::Results(results) => {
                for r in results {
                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": r.id,
                        "content": if r.error { format!("Error: {}", r.output) } else { r.output.clone() },
                    }));
                }
            }
        }
    }
    serde_json::json!({
        "model": req.model,
        "messages": messages,
        "max_tokens": req.max_tokens,
        "tools": req.tools.iter().map(|t| serde_json::json!({
            "type": "function",
            "function": { "name": t.name, "description": t.description, "parameters": t.parameters },
        })).collect::<Vec<_>>(),
    })
}

fn anthropic_body(req: &StepRequest) -> serde_json::Value {
    let mut messages = Vec::new();
    for turn in &req.turns {
        match turn {
            Turn::User(text) => messages.push(serde_json::json!({ "role": "user", "content": text })),
            Turn::Assistant { text, calls } => {
                let mut content = Vec::new();
                if !text.is_empty() {
                    content.push(serde_json::json!({ "type": "text", "text": text }));
                }
                for c in calls {
                    content.push(serde_json::json!({
                        "type": "tool_use", "id": c.id, "name": c.name, "input": c.input,
                    }));
                }
                messages.push(serde_json::json!({ "role": "assistant", "content": content }));
            }
            Turn::Results(results) => messages.push(serde_json::json!({
                "role": "user",
                "content": results.iter().map(|r| serde_json::json!({
                    "type": "tool_result", "tool_use_id": r.id, "content": r.output, "is_error": r.error,
                })).collect::<Vec<_>>(),
            })),
        }
    }
    serde_json::json!({
        "model": req.model,
        "system": req.system,
        "max_tokens": req.max_tokens,
        "messages": messages,
        "tools": req.tools.iter().map(|t| serde_json::json!({
            "name": t.name, "description": t.description, "input_schema": t.parameters,
        })).collect::<Vec<_>>(),
    })
}

/// Arguments arrive as a JSON string from OpenAI-style APIs; a model that
/// writes broken JSON gets an object that says so, which the tool reports.
fn parse_arguments(raw: &serde_json::Value) -> serde_json::Value {
    match raw {
        serde_json::Value::String(s) if s.trim().is_empty() => serde_json::json!({}),
        serde_json::Value::String(s) => serde_json::from_str(s).unwrap_or_else(
            |e| serde_json::json!({ "__invalid": format!("arguments are not JSON: {e}") }),
        ),
        serde_json::Value::Null => serde_json::json!({}),
        other => other.clone(),
    }
}

pub(crate) fn decode_step(api: Api, value: &serde_json::Value) -> Result<Step, String> {
    if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
        return Err(error["message"]
            .as_str()
            .or(error.as_str())
            .unwrap_or("error")
            .to_string());
    }
    match api {
        Api::OpenAi => {
            let choice = &value["choices"][0];
            let message = &choice["message"];
            let calls: Vec<ToolCall> = message["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(i, c)| ToolCall {
                    id: c["id"]
                        .as_str()
                        .map_or_else(|| format!("call_{i}"), str::to_string),
                    name: c["function"]["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    input: parse_arguments(&c["function"]["arguments"]),
                })
                .collect();
            let stop = match choice["finish_reason"].as_str() {
                Some("length") => Stop::Length,
                _ if !calls.is_empty() => Stop::ToolUse,
                _ => Stop::End,
            };
            Ok(Step {
                text: message["content"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                calls,
                stop,
                input_tokens: value["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
                output_tokens: value["usage"]["completion_tokens"].as_u64().unwrap_or(0),
            })
        }
        Api::Anthropic => {
            let mut text = String::new();
            let mut calls = Vec::new();
            for block in value["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
                    Some("tool_use") => calls.push(ToolCall {
                        id: block["id"].as_str().unwrap_or_default().to_string(),
                        name: block["name"].as_str().unwrap_or_default().to_string(),
                        input: block["input"].clone(),
                    }),
                    _ => {}
                }
            }
            let stop = match value["stop_reason"].as_str() {
                Some("max_tokens") => Stop::Length,
                _ if !calls.is_empty() => Stop::ToolUse,
                _ => Stop::End,
            };
            Ok(Step {
                text: text.trim().to_string(),
                calls,
                stop,
                input_tokens: value["usage"]["input_tokens"].as_u64().unwrap_or(0),
                output_tokens: value["usage"]["output_tokens"].as_u64().unwrap_or(0),
            })
        }
    }
}

/// Runs one step: the model's next text and tool calls.
pub async fn step(endpoint: Endpoint, req: StepRequest) -> Result<Step, String> {
    let (url, body) = match endpoint.api {
        Api::OpenAi => (
            format!(
                "{}/chat/completions",
                endpoint.base_url.trim_end_matches('/')
            ),
            openai_body(&req),
        ),
        Api::Anthropic => (
            format!("{}/messages", endpoint.base_url.trim_end_matches('/')),
            anthropic_body(&req),
        ),
    };
    let response = provider::request(&endpoint, &url)
        .body(body.to_string())
        .timeout(Duration::from_secs(900))
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                format!("Could not connect to {}", endpoint.base_url)
            } else if e.is_timeout() {
                "The model took too long to answer".to_string()
            } else {
                e.to_string()
            }
        })?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if status >= 400 {
        return Err(provider::status_error(status, &text));
    }
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Bad answer from the model: {e}"))?;
    decode_step(endpoint.api, &value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};

    fn spec() -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            }),
        }
    }

    fn conversation() -> Vec<Turn> {
        vec![
            Turn::User("Fix it".into()),
            Turn::Assistant {
                text: "Looking.".into(),
                calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({ "path": "a.rs" }),
                }],
            },
            Turn::Results(vec![ToolResult {
                id: "c1".into(),
                output: "fn a() {}".into(),
                error: false,
            }]),
        ]
    }

    #[test]
    fn writes_both_protocols() {
        let req = StepRequest {
            model: "m".into(),
            system: "Be an agent.".into(),
            turns: conversation(),
            tools: vec![spec()],
            max_tokens: 100,
        };
        let o = openai_body(&req);
        assert_eq!(o["messages"][0]["role"], "system");
        assert_eq!(
            o["messages"][2]["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"a.rs"}"#
        );
        assert_eq!(o["messages"][3]["role"], "tool");
        assert_eq!(o["messages"][3]["tool_call_id"], "c1");
        assert_eq!(o["tools"][0]["function"]["name"], "read_file");
        let a = anthropic_body(&req);
        assert_eq!(a["system"], "Be an agent.");
        assert_eq!(a["messages"][1]["content"][1]["type"], "tool_use");
        assert_eq!(a["messages"][1]["content"][1]["input"]["path"], "a.rs");
        assert_eq!(a["messages"][2]["content"][0]["tool_use_id"], "c1");
        assert_eq!(a["tools"][0]["input_schema"]["required"][0], "path");
    }

    #[test]
    fn reads_both_protocols() {
        let o = serde_json::json!({
            "choices": [{ "finish_reason": "tool_calls", "message": {
                "content": null,
                "tool_calls": [
                    { "id": "x", "function": { "name": "read_file", "arguments": "{\"path\":\"b.rs\"}" } },
                    { "function": { "name": "list_files", "arguments": "{oops" } },
                ],
            }}],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 },
        });
        let step = decode_step(Api::OpenAi, &o).unwrap();
        assert_eq!(step.stop, Stop::ToolUse);
        assert_eq!(step.calls[0].input["path"], "b.rs");
        assert_eq!(step.calls[1].id, "call_1");
        assert!(step.calls[1].input["__invalid"].is_string());
        assert_eq!((step.input_tokens, step.output_tokens), (10, 5));

        let a = serde_json::json!({
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": " Done. " }],
            "usage": { "input_tokens": 3, "output_tokens": 2 },
        });
        let step = decode_step(Api::Anthropic, &a).unwrap();
        assert_eq!((step.text.as_str(), step.stop), ("Done.", Stop::End));
        let cut = serde_json::json!({ "stop_reason": "max_tokens", "content": [] });
        assert_eq!(
            decode_step(Api::Anthropic, &cut).unwrap().stop,
            Stop::Length
        );
        let err = serde_json::json!({ "error": { "message": "overloaded" } });
        assert_eq!(decode_step(Api::OpenAi, &err).unwrap_err(), "overloaded");
    }

    #[test]
    fn sends_a_step_and_reads_the_answer() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["tools"][0]["function"]["name"], "read_file");
            let json =
                r#"{"choices":[{"finish_reason":"stop","message":{"content":"All good."}}]}"#;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                json.len()
            );
        });
        let step = crate::runtime()
            .block_on(step(
                Endpoint {
                    api: Api::OpenAi,
                    base_url: base,
                    key: None,
                },
                StepRequest {
                    model: "m".into(),
                    system: "s".into(),
                    turns: vec![Turn::User("hi".into())],
                    tools: vec![spec()],
                    max_tokens: 50,
                },
            ))
            .unwrap();
        assert_eq!(step.text, "All good.");
        assert_eq!(step.stop, Stop::End);
    }
}
