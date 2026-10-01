//! Measures how fast a running server reads a prompt and writes tokens,
//! from the timings llama.cpp reports with each completion.

use std::{path::PathBuf, time::Duration};

use crate::{LocalServer, client};

/// Tokens per second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Speed {
    pub prompt: f64,
    pub generate: f64,
}

impl Speed {
    pub fn at_least(&self, floor: Speed) -> bool {
        self.prompt >= floor.prompt && self.generate >= floor.generate
    }

    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({ "prompt": self.prompt, "generate": self.generate })
    }

    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            prompt: value["prompt"].as_f64()?,
            generate: value["generate"].as_f64()?,
        })
    }
}

/// About 1,600 tokens of code, the size of a typical request's context.
fn prompt() -> String {
    let mut text = String::from("// Review the following Rust module and summarize it.\n");
    for i in 0..24 {
        text.push_str(&format!(
            "pub fn handler_{i}(input: &[u8], limit: usize) -> Result<Vec<u8>, String> {{\n    \
             if input.len() > limit {{ return Err(format!(\"too long: {{}}\", input.len())); }}\n    \
             Ok(input.iter().map(|b| b.wrapping_add({i})).collect())\n}}\n"
        ));
    }
    text
}

async fn complete(url: &str, key: &str, prompt: &str, tokens: u32) -> Result<Speed, String> {
    let body = serde_json::json!({
        "prompt": prompt,
        "n_predict": tokens,
        "temperature": 0,
        "cache_prompt": false,
        "ignore_eos": true,
    });
    let response = client()
        .post(format!("{url}/completion"))
        .bearer_auth(key)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "The benchmark failed: HTTP {}",
            response.status().as_u16()
        ));
    }
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let timings = &value["timings"];
    Ok(Speed {
        prompt: timings["prompt_per_second"]
            .as_f64()
            .ok_or("No timings in the server's answer")?,
        generate: timings["predicted_per_second"]
            .as_f64()
            .ok_or("No timings in the server's answer")?,
    })
}

/// Times a running server: one short warm-up, then a measured run.
pub async fn measure(url: &str, key: &str) -> Result<Speed, String> {
    let prompt = prompt();
    complete(url, key, "Hello", 8).await?;
    complete(url, key, &prompt, 128).await
}

/// Starts `model` on `binary`, measures it and stops it.
pub async fn run(binary: PathBuf, model: PathBuf, logs: PathBuf) -> Result<Speed, String> {
    let server = LocalServer::start(binary, model, 4096, logs).await?;
    let speed = measure(&server.url(), &server.key).await;
    drop(server);
    speed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn mock_server() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_llama_server.py")
    }

    #[test]
    fn measures_a_server_and_stops_it() {
        let logs = std::env::temp_dir().join(format!("solder-ai-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&logs);
        let speed = crate::runtime()
            .block_on(run(mock_server(), "model.gguf".into(), logs.clone()))
            .unwrap();
        assert_eq!(
            speed,
            Speed {
                prompt: 1234.5,
                generate: 67.8
            }
        );
        // The mock logged the key it was given; the benchmark sent it back.
        let log = std::fs::read_to_string(logs.join("llama-server.log")).unwrap();
        assert!(log.contains("authorized"), "{log}");
        assert!(!logs.join("llama-server.pid").exists());
    }

    #[test]
    fn reports_why_the_server_stopped() {
        let logs = std::env::temp_dir().join(format!("solder-ai-crash-{}", std::process::id()));
        let err = crate::runtime()
            .block_on(LocalServer::start(
                mock_server(),
                "missing.gguf".into(),
                4096,
                logs,
            ))
            .err()
            .unwrap();
        assert!(err.contains("failed to load model"), "{err}");
    }

    #[test]
    fn speed_round_trips_and_compares() {
        let s = Speed {
            prompt: 100.,
            generate: 20.,
        };
        assert_eq!(Speed::from_json(&s.to_json()), Some(s));
        assert!(s.at_least(Speed {
            prompt: 100.,
            generate: 15.
        }));
        assert!(!s.at_least(Speed {
            prompt: 101.,
            generate: 1.
        }));
    }
}
