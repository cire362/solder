//! Checks real connections with the editor's drivers: connect, one query,
//! the schema, and whether the session is encrypted. Passwords are never
//! printed.
//!
//! Every connection Solder finds in a project:
//!     cargo run -p db --example probe -- path/to/project
//! One URL, read from the environment so it stays out of shell history:
//!     read -rs SOLDER_PROBE_URL && export SOLDER_PROBE_URL
//!     cargo run -p db --example probe

use std::{
    future::Future,
    time::{Duration, Instant},
};

use db::{ConnectionSpec, Engine, Session, Value};
use tokio::runtime::{Builder, Runtime};

fn main() {
    let mut require_tls = false;
    let mut directory = None;
    for argument in std::env::args().skip(1) {
        if argument == "--require-tls" {
            require_tls = true;
        } else if argument.starts_with('-') || directory.replace(argument).is_some() {
            eprintln!("Usage: probe [--require-tls] [project-folder]");
            std::process::exit(2);
        }
    }
    let specs = match directory {
        Some(dir) => {
            let root = std::fs::canonicalize(&dir).unwrap_or_else(|e| {
                eprintln!("{dir}: {e}");
                std::process::exit(2)
            });
            db::detect(&root, None)
        }
        None => {
            let Some(url) = std::env::var("SOLDER_PROBE_URL")
                .ok()
                .filter(|u| !u.is_empty())
            else {
                eprintln!("Pass a project folder, or set SOLDER_PROBE_URL.");
                std::process::exit(2)
            };
            let Some(engine) = Engine::from_url(&url) else {
                eprintln!("Unsupported database URL");
                std::process::exit(2)
            };
            vec![ConnectionSpec {
                name: "SOLDER_PROBE_URL".into(),
                engine,
                url,
                source: "environment".into(),
                read_only: true,
            }]
        }
    };
    if specs.is_empty() {
        println!("No connections found.");
        return;
    }
    let mut failed = 0;
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("probe runtime");
    for mut spec in specs {
        spec.read_only = true;
        if require_tls {
            match require_encryption(&spec) {
                Ok(url) => spec.url = url,
                Err(error) => {
                    eprintln!("{}: {}", redact(&spec, &spec.name), error);
                    failed += 1;
                    continue;
                }
            }
        }
        if !probe(spec, require_tls, &runtime) {
            failed += 1;
        }
        println!();
    }
    std::process::exit(if failed == 0 { 0 } else { 1 });
}

fn require_encryption(spec: &ConnectionSpec) -> db::Result<String> {
    if spec.engine == Engine::Sqlite {
        return Ok(spec.url.clone());
    }
    let mut url = url::Url::parse(&spec.url)
        .map_err(|_| "Invalid connection URL for TLS probe".to_string())?;
    let mut parameters: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    match spec.engine {
        Engine::Postgres => {
            if parameters
                .iter()
                .any(|(key, value)| key == "sslmode" && value.eq_ignore_ascii_case("disable"))
            {
                return Err("TLS probe refuses sslmode=disable".into());
            }
            let system = parameters
                .iter()
                .any(|(key, value)| key == "sslrootcert" && value == "system");
            let mode = if system { "verify-full" } else { "require" };
            let mut found = false;
            for (key, value) in &mut parameters {
                if key == "sslmode" {
                    found = true;
                    if matches!(value.to_ascii_lowercase().as_str(), "allow" | "prefer") {
                        *value = mode.into();
                    }
                }
            }
            if !found {
                parameters.push(("sslmode".into(), mode.into()));
            }
        }
        Engine::MySql => {
            let mut configured = false;
            for (key, value) in &parameters {
                if matches!(
                    key.as_str(),
                    "ssl-mode"
                        | "sslmode"
                        | "ssl_mode"
                        | "sslaccept"
                        | "ssl"
                        | "tls"
                        | "require_ssl"
                ) {
                    configured = true;
                    if matches!(
                        value.to_ascii_lowercase().as_str(),
                        "false" | "0" | "disabled" | "disable"
                    ) {
                        return Err("TLS probe refuses disabled MySQL encryption".into());
                    }
                }
            }
            if !configured {
                parameters.push(("require_ssl".into(), "true".into()));
            }
        }
        Engine::Redis if url.scheme() != "rediss" => {
            return Err("TLS probe requires a rediss:// URL".into());
        }
        Engine::Mongo => {
            if parameters.iter().any(|(key, value)| {
                matches!(key.as_str(), "tls" | "ssl") && value.eq_ignore_ascii_case("false")
            }) {
                return Err("TLS probe refuses disabled MongoDB encryption".into());
            }
            if !parameters
                .iter()
                .any(|(key, _)| matches!(key.as_str(), "tls" | "ssl"))
            {
                parameters.push(("tls".into(), "true".into()));
            }
        }
        Engine::Redis | Engine::Sqlite => {}
    }
    url.set_query(None);
    if !parameters.is_empty() {
        url.query_pairs_mut().extend_pairs(parameters);
    }
    if spec.engine == Engine::Postgres {
        let query = url.query().map(|query| query.replace('+', "%20"));
        url.set_query(query.as_deref());
    }
    Ok(url.into())
}

fn probe(spec: ConnectionSpec, require_tls: bool, runtime: &Runtime) -> bool {
    println!(
        "{}  {}  {}",
        redact(&spec, &spec.name),
        spec.engine.label(),
        redact(&spec, &spec.source)
    );
    println!("  url      {}", safe_url(&spec));
    let start = Instant::now();
    let session = match wait(runtime, Session::connect(spec.clone())) {
        Ok(s) => {
            println!("  connect  ok in {} ms", start.elapsed().as_millis());
            s
        }
        Err(e) => {
            println!(
                "  connect  FAILED after {} ms: {}",
                start.elapsed().as_millis(),
                redact(&spec, &e)
            );
            return false;
        }
    };
    let query = match spec.engine {
        Engine::Postgres => {
            "SELECT version(), (SELECT ssl::text || ' ' || coalesce(version, '') FROM pg_stat_ssl WHERE pid = pg_backend_pid())"
        }
        Engine::MySql => "SHOW STATUS LIKE 'Ssl_version'",
        Engine::Sqlite => "SELECT sqlite_version()",
        Engine::Redis => "PING",
        Engine::Mongo => "show collections",
    };
    let start = Instant::now();
    let ok = match wait(runtime, session.query(query.into())) {
        Ok(result) => {
            let (description, encrypted) = describe(&spec, &result.rows);
            println!(
                "  query    ok in {} ms: {}",
                start.elapsed().as_millis(),
                redact(&spec, &description)
            );
            if require_tls && spec.engine != Engine::Sqlite && !encrypted {
                println!("  tls      FAILED: encryption was required but is not active");
                false
            } else {
                true
            }
        }
        Err(e) => {
            println!("  query    FAILED: {}", redact(&spec, &e));
            false
        }
    };
    let start = Instant::now();
    match wait(runtime, session.schema()) {
        Ok(schema) => {
            println!(
                "  schema   ok in {} ms: {} objects{}",
                start.elapsed().as_millis(),
                schema.objects.len(),
                if schema.truncated {
                    " (more not listed)"
                } else {
                    ""
                }
            );
            ok
        }
        Err(e) => {
            println!("  schema   FAILED: {}", redact(&spec, &e));
            false
        }
    }
}

fn wait<ResultValue>(
    runtime: &Runtime,
    future: impl Future<Output = db::Result<ResultValue>>,
) -> db::Result<ResultValue> {
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), future)
            .await
            .map_err(|_| "Probe operation timed out after 30 seconds".to_string())?
    })
}

fn describe(spec: &ConnectionSpec, rows: &[Vec<Value>]) -> (String, bool) {
    let value = |column| {
        rows.first()
            .and_then(|row| row.get(column))
            .map(Value::display)
            .unwrap_or_default()
    };
    match spec.engine {
        Engine::Postgres => {
            let version = value(1);
            let encrypted = version.starts_with("true ");
            (
                format!("{} | tls: {version}", first_words(&value(0))),
                encrypted,
            )
        }
        Engine::MySql => {
            let version = value(1);
            let encrypted = !version.is_empty();
            (
                format!("tls: {}", if encrypted { version.as_str() } else { "off" }),
                encrypted,
            )
        }
        Engine::Sqlite => (format!("SQLite {}", value(0)), false),
        Engine::Redis | Engine::Mongo => {
            let encrypted = url::Url::parse(&spec.url).ok().is_some_and(|url| {
                let mut enabled = matches!(url.scheme(), "rediss" | "mongodb+srv");
                for (key, value) in url.query_pairs() {
                    if matches!(key.as_ref(), "tls" | "ssl") {
                        enabled = value.eq_ignore_ascii_case("true");
                    }
                }
                enabled
            });
            let detail = if spec.engine == Engine::Redis {
                value(0)
            } else {
                format!("{} collections", rows.len())
            };
            (
                format!(
                    "{detail} | tls: {} (connection settings)",
                    if encrypted { "on" } else { "off" }
                ),
                encrypted,
            )
        }
    }
}

fn safe_url(spec: &ConnectionSpec) -> String {
    let Ok(mut url) = url::Url::parse(&spec.url) else {
        return "<connection details omitted>".into();
    };
    let _ = url.set_password(None);
    let _ = url.set_username("");
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn redact(spec: &ConnectionSpec, message: &str) -> String {
    let mut secrets = vec![spec.url.clone()];
    if let Some(authority) = spec.url.split("//").nth(1)
        && let Some((credentials, _)) = authority.split_once('@')
        && let Some((_, password)) = credentials.split_once(':')
    {
        secrets.push(password.into());
    }
    if let Some((_, query)) = spec.url.split_once('?') {
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            let key = key.to_ascii_lowercase();
            if key.contains("password")
                || key.contains("secret")
                || key.contains("token")
                || key == "authmechanismproperties"
            {
                secrets.push(value.into_owned());
            }
        }
    }
    let encoded_secrets = secrets.clone();
    for secret in encoded_secrets {
        let query = format!("value={}", secret.replace('+', "%2B"));
        if let Some((_, decoded)) = url::form_urlencoded::parse(query.as_bytes()).next() {
            secrets.push(decoded.into_owned());
        }
    }
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    secrets
        .into_iter()
        .filter(|secret| !secret.is_empty())
        .fold(message.into(), |message: String, secret| {
            message.replace(&secret, "<redacted>")
        })
}

fn first_words(s: &str) -> String {
    s.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(url: &str) -> ConnectionSpec {
        ConnectionSpec {
            name: "probe".into(),
            engine: Engine::Postgres,
            url: url.into(),
            source: "test".into(),
            read_only: true,
        }
    }

    #[test]
    fn passwords_are_hidden_in_urls_and_driver_errors() {
        let spec = connection(
            "postgres://user:s%40fe+password@localhost/db?password=query-secret&sslpassword=key-secret",
        );
        assert_eq!(safe_url(&spec), "postgres://localhost/db");
        let message = format!("{} s@fe+password query-secret key-secret", spec.url);
        let message = redact(&spec, &message);
        for secret in [
            "s%40fe+password",
            "s@fe+password",
            "query-secret",
            "key-secret",
        ] {
            assert!(!message.contains(secret));
        }
        assert_eq!(
            safe_url(&connection("postgres://user:secret@[invalid")),
            "<connection details omitted>"
        );
    }

    #[test]
    fn tls_is_required_before_authentication() {
        let spec = connection("postgres://localhost/db");
        assert!(
            require_encryption(&spec)
                .unwrap()
                .contains("sslmode=require")
        );
        assert!(
            require_encryption(&connection("postgres://localhost/db?sslrootcert=system"))
                .unwrap()
                .contains("sslmode=verify-full")
        );
        assert!(
            require_encryption(&connection("postgres://localhost/db?sslmode=disable")).is_err()
        );
        for (engine, url) in [
            (Engine::MySql, "mysql://localhost/db?ssl-mode=DISABLED"),
            (Engine::Redis, "redis://localhost"),
            (Engine::Mongo, "mongodb+srv://example.com/db?tls=false"),
        ] {
            assert!(
                require_encryption(&ConnectionSpec {
                    engine,
                    url: url.into(),
                    ..spec.clone()
                })
                .is_err()
            );
        }
        assert!(
            require_encryption(&ConnectionSpec {
                engine: Engine::MySql,
                url: "mysql://localhost/db".into(),
                ..spec
            })
            .unwrap()
            .contains("require_ssl=true")
        );
    }

    #[test]
    fn tls_detection_handles_plain_text_and_srv() {
        let mut spec = connection("postgres://localhost/db");
        assert!(
            !describe(
                &spec,
                &[vec![
                    Value::Text("PostgreSQL 17".into()),
                    Value::Text("false ".into())
                ]]
            )
            .1
        );
        assert!(
            describe(
                &spec,
                &[vec![
                    Value::Text("PostgreSQL 17".into()),
                    Value::Text("true TLSv1.3".into())
                ]]
            )
            .1
        );
        spec.engine = Engine::Mongo;
        spec.url = "mongodb+srv://example.com/db".into();
        assert!(describe(&spec, &[]).1);
        spec.url.push_str("?tls=false");
        assert!(!describe(&spec, &[]).1);
        assert!(
            !describe(
                &ConnectionSpec {
                    engine: Engine::MySql,
                    ..spec
                },
                &[]
            )
            .1
        );
    }
}
