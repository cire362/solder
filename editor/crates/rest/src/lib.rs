//! The HTTP client: `.http` request files, sending them, the routes a
//! project serves, and OpenAPI import. No UI.
//!
//! Requests run on a small Tokio runtime started by the first one, so the
//! editor pays nothing until a request is sent.

pub mod http_file;
pub mod openapi;
pub mod routes;

use std::{
    future::Future,
    sync::OnceLock,
    time::{Duration, Instant},
};

pub use http_file::{Request, parse, request_at};

/// Bodies larger than this are cut, so a large download cannot fill memory.
pub const BODY_LIMIT: usize = 10 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// More than `BODY_LIMIT` bytes came back.
    pub truncated: bool,
    pub elapsed: Duration,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body as text, JSON pretty-printed.
    pub fn text(&self) -> String {
        let text = String::from_utf8_lossy(&self.body);
        let json = self
            .header("content-type")
            .is_some_and(|t| t.contains("json"))
            || text.trim_start().starts_with(['{', '[']);
        if json && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            return serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.into_owned());
        }
        text.into_owned()
    }
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("solder-http")
            .enable_all()
            .build()
            .expect("HTTP runtime")
    })
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(concat!("Solder/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("HTTP client")
    })
}

/// Sends `request`; the future can be awaited from any executor.
pub fn send(request: Request) -> impl Future<Output = Result<Response, String>> + Send + 'static {
    let handle = runtime().spawn(async move {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| format!("{} is not an HTTP method", request.method))?;
        let mut builder = client().request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        let start = Instant::now();
        let mut response = builder.send().await.map_err(describe)?;
        let status = response.status();
        let version = format!("{:?}", response.version());
        let headers = response
            .headers()
            .iter()
            .map(|(n, v)| {
                (
                    n.to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        let mut body = Vec::new();
        let mut truncated = false;
        while let Some(chunk) = response.chunk().await.map_err(describe)? {
            if body.len() + chunk.len() > BODY_LIMIT {
                body.extend_from_slice(&chunk[..BODY_LIMIT - body.len()]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Response {
            status: status.as_u16(),
            reason: status.canonical_reason().unwrap_or("").to_string(),
            version,
            headers,
            body,
            truncated,
            elapsed: start.elapsed(),
        })
    });
    async move { handle.await.map_err(|e| e.to_string())? }
}

/// reqwest's errors name the URL and the cause on separate levels.
fn describe(e: reqwest::Error) -> String {
    let mut message = if e.is_connect() {
        "Could not connect".to_string()
    } else if e.is_timeout() {
        "Timed out".to_string()
    } else {
        e.to_string()
    };
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        message = format!("{message}: {s}");
        source = s.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    /// Answers one request with `reply` and returns what it received.
    fn serve_once(reply: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = vec![0; 8192];
            let n = stream.read(&mut buf).unwrap();
            stream.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        (url, handle)
    }

    #[test]
    fn sends_and_reads_back() {
        let (url, server) = serve_once(
            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: 15\r\nX-Id: 7\r\n\r\n{\"id\":7,\"ok\":1}",
        );
        let request = Request {
            name: String::new(),
            method: "POST".into(),
            url: format!("{url}/users"),
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: Some("{\"name\":\"ada\"}".into()),
        };
        let response = futures_lite_block_on(send(request)).unwrap();
        let received = server.join().unwrap();
        assert!(received.starts_with("POST /users HTTP/1.1"), "{received}");
        assert!(received.ends_with("{\"name\":\"ada\"}"), "{received}");
        assert_eq!(
            (response.status, response.reason.as_str()),
            (201, "Created")
        );
        assert_eq!(response.header("x-id"), Some("7"));
        assert_eq!(response.text(), "{\n  \"id\": 7,\n  \"ok\": 1\n}");
    }

    #[test]
    fn connection_errors_are_readable() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let request = Request {
            method: "GET".into(),
            url: format!("http://127.0.0.1:{port}/"),
            ..Default::default()
        };
        let err = futures_lite_block_on(send(request)).unwrap_err();
        assert!(err.starts_with("Could not connect"), "{err}");
    }

    fn futures_lite_block_on<T>(f: impl Future<Output = T>) -> T {
        // A test helper: park the thread until the runtime's task is done.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(f)
    }
}
