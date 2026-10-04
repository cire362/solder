//! OpenAPI 3.0 and 3.1 to a `.http` file: one request per operation, with
//! the server's URL, required query parameters and an example body built
//! from the schema. YAML and JSON are both read.

use serde_json::{Map, Value};
use yaml_rust2::{Yaml, YamlLoader};

const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
/// How deep example bodies go; deeper schemas are usually recursive.
const MAX_DEPTH: usize = 6;

#[derive(Clone, Debug, PartialEq)]
pub struct Operation {
    pub method: String,
    pub path: String,
    pub summary: String,
    /// `?name=value` pairs for required query parameters.
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub content_type: Option<String>,
    pub body: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub title: String,
    pub base_url: String,
    pub operations: Vec<Operation>,
}

/// Reads a spec from JSON or YAML text.
pub fn parse(text: &str) -> Result<Spec, String> {
    // JSON is YAML too; the YAML reader keeps the order paths are written
    // in, which serde_json's map does not.
    let yaml = YamlLoader::load_from_str(text)
        .ok()
        .and_then(|d| d.into_iter().next());
    let doc: Value = match (serde_json::from_str(text), &yaml) {
        (Ok(v), _) => v,
        (Err(_), Some(yaml)) => yaml_to_json(yaml),
        (Err(e), None) => return Err(format!("Not JSON or YAML: {e}")),
    };
    let order: Vec<String> = yaml
        .as_ref()
        .and_then(|y| y["paths"].as_hash())
        .map(|paths| {
            paths
                .keys()
                .filter_map(|k| k.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if doc.get("openapi").is_none() {
        return Err(if doc.get("swagger").is_some() {
            "Swagger 2.0 is not supported; convert it to OpenAPI 3".into()
        } else {
            "Not an OpenAPI document (no openapi field)".into()
        });
    }
    let title = doc["info"]["title"].as_str().unwrap_or("API").to_string();
    let base_url = base_url(&doc["servers"][0]);
    let mut operations = Vec::new();
    if let Some(paths) = doc["paths"].as_object() {
        let mut keys: Vec<&String> = order.iter().filter(|k| paths.contains_key(*k)).collect();
        keys.extend(paths.keys().filter(|k| !order.contains(k)));
        for path in keys {
            let item = &paths[path];
            let item = resolve(&doc, item);
            let shared = item["parameters"].as_array().cloned().unwrap_or_default();
            for method in METHODS {
                let Some(op) = item.get(method) else {
                    continue;
                };
                operations.push(operation(&doc, method, path, op, &shared));
            }
        }
    }
    Ok(Spec {
        title,
        base_url,
        operations,
    })
}

/// The first server's URL with its variables at their defaults.
fn base_url(server: &Value) -> String {
    let mut url = server["url"]
        .as_str()
        .unwrap_or("http://localhost:3000")
        .to_string();
    if let Some(vars) = server["variables"].as_object() {
        for (name, var) in vars {
            if let Some(default) = var["default"].as_str() {
                url = url.replace(&format!("{{{name}}}"), default);
            }
        }
    }
    url.trim_end_matches('/').to_string()
}

fn operation(doc: &Value, method: &str, path: &str, op: &Value, shared: &[Value]) -> Operation {
    let mut path = path.to_string();
    let mut query = Vec::new();
    let mut headers = Vec::new();
    let params = shared
        .iter()
        .chain(op["parameters"].as_array().into_iter().flatten());
    for param in params {
        let param = resolve(doc, param);
        let name = param["name"].as_str().unwrap_or("").to_string();
        let value = param
            .get("example")
            .cloned()
            .or_else(|| example(doc, &param["schema"], 0))
            .map(|v| match v {
                Value::String(s) => s,
                other => other.to_string(),
            })
            .unwrap_or_default();
        let value = if value.is_empty() {
            "1".to_string()
        } else {
            value
        };
        match param["in"].as_str() {
            Some("path") => path = path.replace(&format!("{{{name}}}"), &value),
            Some("query") if param["required"].as_bool() == Some(true) => query.push((name, value)),
            Some("header") if param["required"].as_bool() == Some(true) => {
                headers.push((name, value))
            }
            _ => {}
        }
    }
    let request_body = resolve(doc, &op["requestBody"]);
    let (content_type, body) = match request_body["content"].as_object() {
        Some(content) => {
            let (kind, media) = content
                .iter()
                .find(|(k, _)| k.contains("json"))
                .or_else(|| content.iter().next())
                .map(|(k, v)| (k.clone(), v.clone()))
                .unwrap_or_default();
            let body = media
                .get("example")
                .cloned()
                .or_else(|| {
                    media["examples"]
                        .as_object()
                        .and_then(|e| e.values().next())
                        .map(|e| resolve(doc, e)["value"].clone())
                })
                .or_else(|| example(doc, &media["schema"], 0));
            (Some(kind), body)
        }
        None => (None, None),
    };
    let summary = op["summary"]
        .as_str()
        .or_else(|| op["operationId"].as_str())
        .unwrap_or("")
        .to_string();
    Operation {
        method: method.to_ascii_uppercase(),
        path,
        summary,
        query,
        headers,
        content_type,
        body,
    }
}

/// Follows a local `$ref` (`#/components/...`).
fn resolve<'a>(doc: &'a Value, value: &'a Value) -> std::borrow::Cow<'a, Value> {
    let mut current = value;
    for _ in 0..16 {
        let Some(reference) = current["$ref"].as_str() else {
            return std::borrow::Cow::Borrowed(current);
        };
        let Some(pointer) = reference.strip_prefix('#') else {
            return std::borrow::Cow::Owned(Value::Null);
        };
        current = doc.pointer(pointer).unwrap_or(&Value::Null);
    }
    std::borrow::Cow::Owned(Value::Null)
}

/// An example value for a schema: its example, default or first enum value,
/// else one built from its type.
pub fn example(doc: &Value, schema: &Value, depth: usize) -> Option<Value> {
    if depth > MAX_DEPTH || schema.is_null() {
        return None;
    }
    let schema = resolve(doc, schema);
    for key in ["example", "default", "const"] {
        if let Some(v) = schema.get(key) {
            return Some(v.clone());
        }
    }
    if let Some(v) = schema["examples"].as_array().and_then(|e| e.first()) {
        return Some(v.clone());
    }
    if let Some(v) = schema["enum"].as_array().and_then(|e| e.first()) {
        return Some(v.clone());
    }
    if let Some(all) = schema["allOf"].as_array() {
        let mut merged = Map::new();
        for part in all {
            if let Some(Value::Object(o)) = example(doc, part, depth + 1) {
                merged.extend(o);
            }
        }
        return Some(Value::Object(merged));
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(first) = schema[key].as_array().and_then(|a| a.first()) {
            return example(doc, first, depth + 1);
        }
    }
    // 3.1 allows a list of types: the first that is not null.
    let ty = match &schema["type"] {
        Value::String(t) => t.clone(),
        Value::Array(types) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or("null")
            .to_string(),
        _ if schema.get("properties").is_some() => "object".into(),
        _ => return None,
    };
    Some(match ty.as_str() {
        "object" => {
            let mut object = Map::new();
            if let Some(props) = schema["properties"].as_object() {
                for (name, prop) in props {
                    if resolve(doc, prop)["readOnly"].as_bool() == Some(true) {
                        continue;
                    }
                    if let Some(v) = example(doc, prop, depth + 1) {
                        object.insert(name.clone(), v);
                    }
                }
            }
            Value::Object(object)
        }
        "array" => Value::Array(
            example(doc, &schema["items"], depth + 1)
                .into_iter()
                .collect(),
        ),
        "integer" => Value::from(schema["minimum"].as_i64().unwrap_or(0)),
        "number" => Value::from(schema["minimum"].as_f64().unwrap_or(0.)),
        "boolean" => Value::Bool(false),
        "string" => Value::String(
            match schema["format"].as_str() {
                Some("date-time") => "2024-01-01T00:00:00Z",
                Some("date") => "2024-01-01",
                Some("email") => "user@example.com",
                Some("uuid") => "00000000-0000-0000-0000-000000000000",
                Some("uri" | "url") => "https://example.com",
                _ => "string",
            }
            .into(),
        ),
        _ => Value::Null,
    })
}

/// One operation as a `.http` request, its URL starting at `base`.
pub fn operation_http(op: &Operation, base: &str) -> String {
    let mut url = format!("{base}{}", op.path);
    if !op.query.is_empty() {
        let query: Vec<String> = op.query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        url.push_str(&format!("?{}", query.join("&")));
    }
    let mut out = format!("### {} {}", op.method, op.path);
    if !op.summary.is_empty() {
        out.push_str(&format!(": {}", op.summary));
    }
    out.push_str(&format!("\n{} {url}\n", op.method));
    for (name, value) in &op.headers {
        out.push_str(&format!("{name}: {value}\n"));
    }
    if let Some(kind) = &op.content_type {
        out.push_str(&format!("Content-Type: {kind}\n"));
    }
    if let Some(body) = &op.body {
        let body = serde_json::to_string_pretty(body).unwrap_or_default();
        out.push_str(&format!("\n{body}\n"));
    }
    out
}

/// The spec as a `.http` file.
pub fn to_http(spec: &Spec) -> String {
    let mut out = format!(
        "# Requests for {} (imported from OpenAPI)\n@baseUrl = {}\n",
        spec.title, spec.base_url
    );
    for op in &spec.operations {
        out.push('\n');
        out.push_str(&operation_http(op, "{{baseUrl}}"));
    }
    out
}

/// OpenAPI documents in a project: `openapi.*`, `swagger.*` or
/// `*.openapi.*` files in YAML or JSON.
pub fn find_specs(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut dirs = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if depth < 4
                    && !name.starts_with('.')
                    && !matches!(
                        name.as_str(),
                        "node_modules" | "target" | "dist" | "build" | "vendor"
                    )
                {
                    dirs.push((path, depth + 1));
                }
            } else if (name.starts_with("openapi.")
                || name.starts_with("swagger.")
                || name.contains(".openapi."))
                && (name.ends_with(".yaml") || name.ends_with(".yml") || name.ends_with(".json"))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn yaml_to_json(yaml: &Yaml) -> Value {
    match yaml {
        Yaml::Real(r) => r.parse::<f64>().map_or(Value::Null, Value::from),
        Yaml::Integer(i) => Value::from(*i),
        Yaml::String(s) => Value::String(s.clone()),
        Yaml::Boolean(b) => Value::Bool(*b),
        Yaml::Array(items) => Value::Array(items.iter().map(yaml_to_json).collect()),
        Yaml::Hash(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let key = match k {
                        Yaml::String(s) => s.clone(),
                        Yaml::Integer(i) => i.to_string(),
                        Yaml::Boolean(b) => b.to_string(),
                        other => format!("{other:?}"),
                    };
                    (key, yaml_to_json(v))
                })
                .collect(),
        ),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r##"
openapi: 3.1.0
info:
  title: Shop
servers:
  - url: "{scheme}://localhost:{port}/v1"
    variables:
      scheme: { default: http }
      port: { default: "8080" }
paths:
  /users/{userId}:
    parameters:
      - name: userId
        in: path
        required: true
        schema: { type: integer, example: 42 }
    get:
      summary: Get a user
      parameters:
        - name: expand
          in: query
          required: true
          schema: { type: string, enum: [orders, none] }
        - name: page
          in: query
          schema: { type: integer }
    put:
      operationId: updateUser
      requestBody:
        content:
          application/json:
            schema: { $ref: "#/components/schemas/User" }
  /orders:
    post:
      requestBody:
        $ref: "#/components/requestBodies/Order"
components:
  schemas:
    User:
      allOf:
        - $ref: "#/components/schemas/Named"
        - type: object
          properties:
            id: { type: integer, readOnly: true }
            email: { type: string, format: email }
            tags: { type: array, items: { type: string } }
            nickname: { type: [string, "null"] }
    Named:
      type: object
      properties:
        name: { type: string, default: ada }
  requestBodies:
    Order:
      content:
        application/json:
          examples:
            small:
              value: { items: 1 }
"##;

    #[test]
    fn reads_yaml_3_1_with_refs_and_examples() {
        let spec = parse(SPEC).unwrap();
        assert_eq!(spec.title, "Shop");
        assert_eq!(spec.base_url, "http://localhost:8080/v1");
        let get = &spec.operations[0];
        assert_eq!(
            (get.method.as_str(), get.path.as_str()),
            ("GET", "/users/42")
        );
        assert_eq!(get.query, [("expand".to_string(), "orders".to_string())]);
        let put = &spec.operations[1];
        assert_eq!(put.summary, "updateUser");
        assert_eq!(
            put.body,
            Some(
                serde_json::json!({"name": "ada", "email": "user@example.com", "tags": ["string"], "nickname": "string"})
            )
        );
        assert_eq!(
            spec.operations[2].body,
            Some(serde_json::json!({"items": 1}))
        );
    }

    #[test]
    fn writes_a_http_file_that_parses_back() {
        let spec = parse(SPEC).unwrap();
        let text = to_http(&spec);
        assert!(text.starts_with(
            "# Requests for Shop (imported from OpenAPI)\n@baseUrl = http://localhost:8080/v1\n"
        ));
        assert!(
            text.contains(
                "### GET /users/42: Get a user\nGET {{baseUrl}}/users/42?expand=orders\n"
            )
        );
        let (blocks, _) = crate::http_file::blocks(&text);
        assert_eq!(blocks.len(), 3);
        let put = crate::http_file::parse(&text, &blocks[1], &Default::default()).unwrap();
        assert_eq!(put.url, "http://localhost:8080/v1/users/42");
        assert_eq!(
            put.headers,
            [("Content-Type".to_string(), "application/json".to_string())]
        );
        let body: Value = serde_json::from_str(put.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["name"], "ada");
        // JSON specs read the same way.
        let json = serde_json::json!({"openapi": "3.0.3", "info": {"title": "J"}, "paths": {"/ping": {"get": {}}}});
        let spec = parse(&json.to_string()).unwrap();
        assert_eq!(spec.operations[0].path, "/ping");
        assert!(parse("swagger: '2.0'").unwrap_err().contains("Swagger 2.0"));
    }
}
