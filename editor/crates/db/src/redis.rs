use redis::aio::MultiplexedConnection;

use crate::{
    Column, ConnectionSpec, Object, ObjectKind, QueryResult, Result, Schema, Value, params,
};

/// Keys listed in the schema; SCAN stops after this many.
const KEY_LIMIT: usize = 2_000;

pub struct Redis {
    conn: MultiplexedConnection,
    read_only: bool,
}

impl Redis {
    pub async fn connect(spec: &ConnectionSpec) -> Result<Self> {
        let params = params::redis(&spec.url)?;
        let client = match params.root {
            Some(root) => redis::Client::build_with_tls(
                params.url.as_str(),
                redis::TlsCertificates {
                    client_tls: None,
                    root_cert: Some(std::fs::read(root).map_err(|error| error.to_string())?),
                },
            ),
            None => redis::Client::open(params.url.as_str()),
        }
        .map_err(|error| error.to_string())?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self {
            conn,
            read_only: spec.read_only,
        })
    }

    pub async fn query(&self, text: &str) -> Result<QueryResult> {
        let args = split_args(text)?;
        let Some(name) = args.first() else {
            return Err("Empty command".into());
        };
        if self.read_only && !is_read_command(name) {
            return Err(format!(
                "{} is not allowed on a read-only connection",
                name.to_ascii_uppercase()
            ));
        }
        let mut cmd = redis::cmd(name);
        for arg in &args[1..] {
            cmd.arg(arg);
        }
        let mut conn = self.conn.clone();
        let value: redis::Value = cmd
            .query_async(&mut conn)
            .await
            .map_err(|e| e.to_string())?;
        Ok(to_result(pair_up(&args, value)))
    }

    pub async fn schema(&self) -> Result<Schema> {
        let mut conn = self.conn.clone();
        let mut keys: Vec<String> = Vec::new();
        let mut cursor: u64 = 0;
        loop {
            let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("COUNT")
                .arg(500)
                .query_async(&mut conn)
                .await
                .map_err(|e| e.to_string())?;
            keys.extend(batch);
            cursor = next;
            if cursor == 0 || keys.len() >= KEY_LIMIT {
                break;
            }
        }
        let truncated = cursor != 0;
        keys.truncate(KEY_LIMIT);
        keys.sort();
        let mut pipe = redis::pipe();
        for key in &keys {
            pipe.cmd("TYPE").arg(key);
        }
        let types: Vec<String> = if keys.is_empty() {
            Vec::new()
        } else {
            pipe.query_async(&mut conn)
                .await
                .map_err(|e| e.to_string())?
        };
        Ok(Schema {
            objects: keys
                .into_iter()
                .zip(types)
                .map(|(name, kind)| Object {
                    namespace: None,
                    name,
                    kind: ObjectKind::Key(kind),
                    ..Default::default()
                })
                .collect(),
            truncated,
        })
    }
}

/// RESP2 sends field/value replies as one flat array.
fn pair_up(args: &[String], value: redis::Value) -> redis::Value {
    let name = args[0].to_ascii_lowercase();
    let pairs = matches!(name.as_str(), "hgetall" | "config")
        || args[1..]
            .iter()
            .any(|a| a.eq_ignore_ascii_case("withscores"));
    match value {
        redis::Value::Array(items) if pairs && items.len() % 2 == 0 => {
            let mut items = items.into_iter();
            let mut out = Vec::new();
            while let (Some(k), Some(v)) = (items.next(), items.next()) {
                out.push((k, v));
            }
            redis::Value::Map(out)
        }
        other => other,
    }
}

/// Scalars become one cell; lists become `(#, value)` rows and maps
/// `(field, value)` rows. Anything nested is shown as text.
fn to_result(value: redis::Value) -> QueryResult {
    let column = |name: &str| Column {
        name: name.into(),
        type_name: String::new(),
    };
    let mut result = QueryResult::default();
    match value {
        redis::Value::Array(items) | redis::Value::Set(items) => {
            result.columns = vec![column("#"), column("value")];
            for (i, item) in items.into_iter().enumerate() {
                if !result.push_row(vec![Value::Int(i as i64 + 1), scalar(item)]) {
                    break;
                }
            }
        }
        redis::Value::Map(pairs) => {
            result.columns = vec![column("field"), column("value")];
            for (k, v) in pairs {
                if !result.push_row(vec![scalar(k), scalar(v)]) {
                    break;
                }
            }
        }
        other => {
            result.columns = vec![column("value")];
            result.push_row(vec![scalar(other)]);
        }
    }
    result
}

fn scalar(value: redis::Value) -> Value {
    match value {
        redis::Value::Nil => Value::Null,
        redis::Value::Int(i) => Value::Int(i),
        redis::Value::Double(f) => Value::Float(f),
        redis::Value::Boolean(b) => Value::Bool(b),
        redis::Value::Okay => Value::Text("OK".into()),
        redis::Value::SimpleString(s) => Value::Text(s),
        redis::Value::VerbatimString { text, .. } => Value::Text(text),
        redis::Value::BulkString(bytes) => match String::from_utf8(bytes) {
            Ok(s) => Value::Text(s),
            Err(e) => Value::Bytes(e.into_bytes()),
        },
        redis::Value::Array(items) | redis::Value::Set(items) => Value::Json(format!(
            "[{}]",
            items
                .into_iter()
                .map(|i| scalar(i).display())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        other => Value::Text(format!("{other:?}")),
    }
}

/// Splits a command line like redis-cli: spaces separate, quotes group,
/// `\n` and `\"` escape inside double quotes.
pub(crate) fn split_args(line: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut chars = line.trim().chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut arg = String::new();
        match c {
            '"' | '\'' => {
                chars.next();
                let mut closed = false;
                while let Some(ch) = chars.next() {
                    if ch == c {
                        closed = true;
                        break;
                    }
                    if ch == '\\' && c == '"' {
                        match chars.next() {
                            Some('n') => arg.push('\n'),
                            Some('t') => arg.push('\t'),
                            Some(other) => arg.push(other),
                            None => break,
                        }
                    } else {
                        arg.push(ch);
                    }
                }
                if !closed {
                    return Err("Unclosed quote".into());
                }
            }
            _ => {
                while let Some(&ch) = chars.peek() {
                    if ch.is_whitespace() {
                        break;
                    }
                    arg.push(ch);
                    chars.next();
                }
            }
        }
        args.push(arg);
    }
    Ok(args)
}

/// Quotes a key for a generated command when it needs it.
pub(crate) fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        return arg.to_string();
    }
    format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
}

fn is_read_command(name: &str) -> bool {
    const READ: &[&str] = &[
        "get",
        "mget",
        "strlen",
        "getrange",
        "exists",
        "type",
        "ttl",
        "pttl",
        "keys",
        "scan",
        "dbsize",
        "info",
        "ping",
        "echo",
        "time",
        "hget",
        "hmget",
        "hgetall",
        "hkeys",
        "hvals",
        "hlen",
        "hexists",
        "hscan",
        "hstrlen",
        "lrange",
        "llen",
        "lindex",
        "lpos",
        "smembers",
        "scard",
        "sismember",
        "smismember",
        "sscan",
        "srandmember",
        "sinter",
        "sunion",
        "sdiff",
        "zrange",
        "zrangebyscore",
        "zrevrange",
        "zrevrangebyscore",
        "zcard",
        "zscore",
        "zrank",
        "zrevrank",
        "zcount",
        "zscan",
        "zmscore",
        "xrange",
        "xrevrange",
        "xlen",
        "xinfo",
        "memory",
        "object",
        "randomkey",
        "select",
        "json.get",
        "pfcount",
        "getbit",
        "bitcount",
        "bitpos",
        "geopos",
        "geodist",
        "geosearch",
        "georadius_ro",
    ];
    READ.contains(&name.to_ascii_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_redis_cli() {
        assert_eq!(
            split_args(r#"SET "user:1 name" 'it''s' "a\"b\n""#).unwrap(),
            vec!["SET", "user:1 name", "it", "s", "a\"b\n"]
        );
        assert!(split_args("GET \"open").is_err());
        assert_eq!(quote_arg("user:1"), "user:1");
        assert_eq!(quote_arg("a b"), "\"a b\"");
    }

    #[test]
    fn shapes_replies_into_rows() {
        let r = to_result(redis::Value::Map(vec![(
            redis::Value::BulkString(b"name".to_vec()),
            redis::Value::BulkString(b"ada".to_vec()),
        )]));
        assert_eq!(r.columns[0].name, "field");
        assert_eq!(
            r.rows,
            vec![vec![Value::Text("name".into()), Value::Text("ada".into())]]
        );
        let r = to_result(redis::Value::Nil);
        assert_eq!(r.rows, vec![vec![Value::Null]]);
        assert!(is_read_command("HGETALL") && !is_read_command("del"));
        let flat = redis::Value::Array(vec![redis::Value::Okay, redis::Value::Int(1)]);
        let args = vec![
            "ZRANGE".to_string(),
            "k".into(),
            "0".into(),
            "-1".into(),
            "withscores".into(),
        ];
        assert!(matches!(pair_up(&args, flat), redis::Value::Map(p) if p.len() == 1));
    }
}
