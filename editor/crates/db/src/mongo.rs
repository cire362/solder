//! MongoDB with the shell's syntax for the calls people type most:
//! `db.users.find({age: {$gt: 30}}).sort({name: 1}).limit(10)`, `findOne`,
//! `aggregate`, `countDocuments`, `distinct`, inserts, updates, deletes,
//! `show collections` and `use <db>`. Arguments are JavaScript object
//! literals; `ObjectId(...)`, `ISODate(...)` and friends become extended JSON.

use futures::TryStreamExt;
use mongodb::{
    Client,
    bson::{Bson, Document},
};
use serde_json::{Map, Value as Json};
use tokio::sync::Mutex;

use crate::{
    Column, ColumnInfo, ConnectionSpec, Object, ObjectKind, PREVIEW_ROWS, QueryResult, Result,
    Schema, Value,
};

const SAMPLED_COLLECTIONS: usize = 100;

pub struct Mongo {
    client: Client,
    database: Mutex<String>,
    read_only: bool,
}

impl Mongo {
    pub async fn connect(spec: &ConnectionSpec) -> Result<Self> {
        let client = Client::with_uri_str(&spec.url)
            .await
            .map_err(|e| e.to_string())?;
        let database = client
            .default_database()
            .map_or_else(|| "test".to_string(), |db| db.name().to_string());
        // The driver connects lazily; ping so a bad URL fails here.
        client
            .database(&database)
            .run_command(mongodb::bson::doc! { "ping": 1 })
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            database: Mutex::new(database),
            read_only: spec.read_only,
        })
    }

    pub async fn query(&self, text: &str) -> Result<QueryResult> {
        let call = parse_call(text)?;
        let mut current = self.database.lock().await;
        let db = self.client.database(&current);
        if self.read_only && call.writes() {
            return Err(format!(
                "{} is not allowed on a read-only connection",
                call.method
            ));
        }
        let docs: Vec<Document> = match call.method.as_str() {
            "use" => {
                *current = call.collection.clone();
                return Ok(message(format!("switched to db {current}")));
            }
            "showCollections" => {
                let mut names = db.list_collection_names().await.map_err(err)?;
                names.sort();
                return Ok(list("collection", names));
            }
            "showDbs" => {
                let mut names = self.client.list_database_names().await.map_err(err)?;
                names.sort();
                return Ok(list("database", names));
            }
            _ => {
                let coll = db.collection::<Document>(&call.collection);
                let arg = |i: usize| call.args.get(i).cloned().unwrap_or_default();
                match call.method.as_str() {
                    "find" => {
                        let mut find = coll.find(document(arg(0))?);
                        if let Some(p) = call.args.get(1) {
                            find = find.projection(document(p.clone())?);
                        }
                        if let Some(sort) = &call.sort {
                            find = find.sort(document(sort.clone())?);
                        }
                        if let Some(skip) = call.skip {
                            find = find.skip(skip);
                        }
                        let limit = call.limit.unwrap_or(crate::ROW_LIMIT as i64 + 1);
                        find.limit(limit)
                            .await
                            .map_err(err)?
                            .try_collect()
                            .await
                            .map_err(err)?
                    }
                    "findOne" => coll
                        .find_one(document(arg(0))?)
                        .await
                        .map_err(err)?
                        .into_iter()
                        .collect(),
                    "aggregate" => {
                        let pipeline = match arg(0) {
                            Json::Array(stages) => stages
                                .into_iter()
                                .map(document)
                                .collect::<Result<Vec<_>>>()?,
                            Json::Null => Vec::new(),
                            other => vec![document(other)?],
                        };
                        coll.aggregate(pipeline)
                            .await
                            .map_err(err)?
                            .try_collect()
                            .await
                            .map_err(err)?
                    }
                    "countDocuments" | "count" => {
                        let n = coll.count_documents(document(arg(0))?).await.map_err(err)?;
                        return Ok(scalar("count", Value::Int(n as i64)));
                    }
                    "distinct" => {
                        let field = arg(0).as_str().unwrap_or_default().to_string();
                        let values = coll
                            .distinct(&field, document(arg(1))?)
                            .await
                            .map_err(err)?;
                        let mut result = QueryResult {
                            columns: vec![column(&field)],
                            ..Default::default()
                        };
                        for v in values {
                            if !result.push_row(vec![value(v)]) {
                                break;
                            }
                        }
                        return Ok(result);
                    }
                    "insertOne" => {
                        let r = coll.insert_one(document(arg(0))?).await.map_err(err)?;
                        return Ok(scalar("insertedId", value(r.inserted_id)));
                    }
                    "insertMany" => {
                        let docs = match arg(0) {
                            Json::Array(items) => items
                                .into_iter()
                                .map(document)
                                .collect::<Result<Vec<_>>>()?,
                            _ => return Err("insertMany takes an array".into()),
                        };
                        let r = coll.insert_many(docs).await.map_err(err)?;
                        return Ok(affected(r.inserted_ids.len() as u64));
                    }
                    "updateOne" | "updateMany" | "replaceOne" => {
                        let filter = document(arg(0))?;
                        let update = document(arg(1))?;
                        let r = match call.method.as_str() {
                            "updateOne" => coll.update_one(filter, update).await,
                            "updateMany" => coll.update_many(filter, update).await,
                            _ => coll.replace_one(filter, update).await,
                        }
                        .map_err(err)?;
                        return Ok(affected(r.modified_count));
                    }
                    "deleteOne" | "deleteMany" => {
                        let filter = document(arg(0))?;
                        let r = if call.method == "deleteOne" {
                            coll.delete_one(filter).await
                        } else {
                            coll.delete_many(filter).await
                        }
                        .map_err(err)?;
                        return Ok(affected(r.deleted_count));
                    }
                    other => return Err(format!("{other} is not supported")),
                }
            }
        };
        Ok(documents(docs))
    }

    pub async fn schema(&self) -> Result<Schema> {
        let name = self.database.lock().await.clone();
        let db = self.client.database(&name);
        let mut names = db.list_collection_names().await.map_err(err)?;
        names.sort();
        let mut objects = Vec::new();
        for (i, collection) in names.into_iter().enumerate() {
            let columns = if i < SAMPLED_COLLECTIONS {
                let sample = db
                    .collection::<Document>(&collection)
                    .find_one(Document::new())
                    .await
                    .map_err(err)?;
                sample
                    .map(|doc| {
                        doc.iter()
                            .map(|(k, v)| ColumnInfo {
                                name: k.clone(),
                                type_name: type_name(v).into(),
                                nullable: true,
                                primary_key: k == "_id",
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            objects.push(Object {
                namespace: None,
                name: collection,
                kind: ObjectKind::Collection,
                columns,
                indexes: Vec::new(),
            });
        }
        Ok(Schema {
            objects,
            truncated: false,
        })
    }
}

pub fn preview_query(object: &Object) -> String {
    let is_ident = object.name.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !object.name.starts_with(|c: char| c.is_ascii_digit());
    if is_ident {
        format!("db.{}.find({{}}).limit({PREVIEW_ROWS})", object.name)
    } else {
        format!(
            "db.getCollection({}).find({{}}).limit({PREVIEW_ROWS})",
            Json::String(object.name.clone())
        )
    }
}

fn err(e: mongodb::error::Error) -> String {
    e.to_string()
}

fn column(name: &str) -> Column {
    Column {
        name: name.into(),
        type_name: String::new(),
    }
}

fn scalar(name: &str, v: Value) -> QueryResult {
    QueryResult {
        columns: vec![column(name)],
        rows: vec![vec![v]],
        ..Default::default()
    }
}

fn message(text: String) -> QueryResult {
    scalar("result", Value::Text(text))
}

fn affected(n: u64) -> QueryResult {
    QueryResult {
        affected: Some(n),
        ..Default::default()
    }
}

fn list(name: &str, items: Vec<String>) -> QueryResult {
    QueryResult {
        columns: vec![column(name)],
        rows: items.into_iter().map(|n| vec![Value::Text(n)]).collect(),
        ..Default::default()
    }
}

/// Columns are the union of top-level fields, `_id` first, in the order
/// they first appear.
fn documents(docs: Vec<Document>) -> QueryResult {
    let mut names: Vec<String> = Vec::new();
    let mut types: Vec<&'static str> = Vec::new();
    for doc in &docs {
        for (k, v) in doc {
            if !names.contains(k) {
                names.push(k.clone());
                types.push(type_name(v));
            }
        }
    }
    if let Some(ix) = names.iter().position(|n| n == "_id") {
        let id = names.remove(ix);
        let ty = types.remove(ix);
        names.insert(0, id);
        types.insert(0, ty);
    }
    let mut result = QueryResult {
        columns: names
            .iter()
            .zip(&types)
            .map(|(n, t)| Column {
                name: n.clone(),
                type_name: (*t).into(),
            })
            .collect(),
        ..Default::default()
    };
    for mut doc in docs {
        let row = names
            .iter()
            .map(|n| doc.remove(n).map_or(Value::Null, value))
            .collect();
        if !result.push_row(row) {
            break;
        }
    }
    result
}

fn value(v: Bson) -> Value {
    match v {
        Bson::Null | Bson::Undefined => Value::Null,
        Bson::Boolean(b) => Value::Bool(b),
        Bson::Int32(i) => Value::Int(i.into()),
        Bson::Int64(i) => Value::Int(i),
        Bson::Double(f) => Value::Float(f),
        Bson::Decimal128(d) => Value::Number(d.to_string()),
        Bson::String(s) | Bson::Symbol(s) => Value::Text(s),
        Bson::ObjectId(id) => Value::Text(format!("ObjectId(\"{id}\")")),
        Bson::DateTime(d) => Value::Text(
            d.try_to_rfc3339_string()
                .unwrap_or_else(|_| d.timestamp_millis().to_string()),
        ),
        Bson::Binary(b) => Value::Bytes(b.bytes),
        other @ (Bson::Document(_) | Bson::Array(_)) => {
            Value::Json(other.into_relaxed_extjson().to_string())
        }
        other => Value::Text(other.to_string()),
    }
}

fn type_name(v: &Bson) -> &'static str {
    match v {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binData",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null => "null",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Decimal128(_) => "decimal",
        Bson::Timestamp(_) => "timestamp",
        Bson::RegularExpression(_) => "regex",
        _ => "",
    }
}

fn document(json: Json) -> Result<Document> {
    match json {
        Json::Null => Ok(Document::new()),
        json => match Bson::try_from(json).map_err(|e| e.to_string())? {
            Bson::Document(d) => Ok(d),
            other => Err(format!("Expected an object, got {other}")),
        },
    }
}

#[derive(Debug, Default, PartialEq)]
struct Call {
    collection: String,
    method: String,
    args: Vec<Json>,
    sort: Option<Json>,
    limit: Option<i64>,
    skip: Option<u64>,
}

impl Call {
    fn writes(&self) -> bool {
        match self.method.as_str() {
            "find" | "findOne" | "countDocuments" | "count" | "distinct" | "use"
            | "showCollections" | "showDbs" => false,
            "aggregate" => self.args.first().is_some_and(|p| {
                let text = p.to_string();
                text.contains("\"$out\"") || text.contains("\"$merge\"")
            }),
            _ => true,
        }
    }
}

fn parse_call(text: &str) -> Result<Call> {
    let text = text.trim().trim_end_matches(';').trim();
    let words: Vec<&str> = text.split_whitespace().collect();
    match words.as_slice() {
        ["show", "collections" | "tables"] => {
            return Ok(Call {
                method: "showCollections".into(),
                ..Default::default()
            });
        }
        ["show", "dbs" | "databases"] => {
            return Ok(Call {
                method: "showDbs".into(),
                ..Default::default()
            });
        }
        ["use", name] => {
            return Ok(Call {
                method: "use".into(),
                collection: name.to_string(),
                ..Default::default()
            });
        }
        _ => {}
    }
    let mut p = Parser::new(text);
    p.expect_word("db")?;
    p.expect('.')?;
    let first = p.ident()?;
    let collection = if first == "getCollection" {
        p.expect('(')?;
        let name = p.value()?;
        p.expect(')')?;
        name.as_str()
            .ok_or("getCollection takes a name")?
            .to_string()
    } else {
        first
    };
    let mut call = Call {
        collection,
        ..Default::default()
    };
    p.expect('.')?;
    call.method = p.ident()?;
    call.args = p.args()?;
    while p.eat('.') {
        let modifier = p.ident()?;
        let mut args = p.args()?;
        let arg = if args.is_empty() {
            Json::Null
        } else {
            args.remove(0)
        };
        match modifier.as_str() {
            "sort" => call.sort = Some(arg),
            "limit" => call.limit = arg.as_i64(),
            "skip" => call.skip = arg.as_u64(),
            "pretty" | "toArray" => {}
            other => return Err(format!("{other}() is not supported")),
        }
    }
    p.skip_ws();
    if !p.done() {
        return Err(format!("Unexpected text: {}", &p.text[p.pos..]));
    }
    Ok(call)
}

/// A JavaScript object-literal reader: unquoted keys, single quotes,
/// trailing commas, regex literals and the shell's type constructors.
struct Parser<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn skip_ws(&mut self) {
        loop {
            let rest = &self.text[self.pos..];
            let trimmed = rest.trim_start();
            self.pos += rest.len() - trimmed.len();
            if trimmed.starts_with("//") {
                self.pos += trimmed.find('\n').unwrap_or(trimmed.len());
            } else {
                break;
            }
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<()> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(self.unexpected(&format!("'{c}'")))
        }
    }

    fn unexpected(&self, wanted: &str) -> String {
        match self.peek() {
            Some(c) => format!("Expected {wanted} at '{c}' (offset {})", self.pos),
            None => format!("Expected {wanted} at the end"),
        }
    }

    fn ident(&mut self) -> Result<String> {
        self.skip_ws();
        let rest = &self.text[self.pos..];
        let len = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .unwrap_or(rest.len());
        if len == 0 {
            return Err(self.unexpected("a name"));
        }
        self.pos += len;
        Ok(rest[..len].to_string())
    }

    fn expect_word(&mut self, word: &str) -> Result<()> {
        let got = self.ident()?;
        if got == word {
            Ok(())
        } else {
            Err(format!("Expected {word}, got {got}"))
        }
    }

    fn args(&mut self) -> Result<Vec<Json>> {
        self.expect('(')?;
        let mut args = Vec::new();
        loop {
            if self.eat(')') {
                return Ok(args);
            }
            args.push(self.value()?);
            if !self.eat(',') {
                self.expect(')')?;
                return Ok(args);
            }
        }
    }

    fn value(&mut self) -> Result<Json> {
        self.skip_ws();
        match self.peek() {
            Some('{') => {
                self.pos += 1;
                let mut map = Map::new();
                loop {
                    if self.eat('}') {
                        return Ok(Json::Object(map));
                    }
                    self.skip_ws();
                    let key = match self.peek() {
                        Some('"' | '\'') => self.string()?,
                        _ => self.key()?,
                    };
                    self.expect(':')?;
                    map.insert(key, self.value()?);
                    if !self.eat(',') {
                        self.expect('}')?;
                        return Ok(Json::Object(map));
                    }
                }
            }
            Some('[') => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    if self.eat(']') {
                        return Ok(Json::Array(items));
                    }
                    items.push(self.value()?);
                    if !self.eat(',') {
                        self.expect(']')?;
                        return Ok(Json::Array(items));
                    }
                }
            }
            Some('"' | '\'') => Ok(Json::String(self.string()?)),
            Some('/') => self.regex(),
            Some(c) if c == '-' || c == '.' || c.is_ascii_digit() => self.number(),
            Some(_) => {
                let word = self.ident()?;
                match word.as_str() {
                    "true" => Ok(Json::Bool(true)),
                    "false" => Ok(Json::Bool(false)),
                    "null" | "undefined" => Ok(Json::Null),
                    "new" => self.value(),
                    _ => self.constructor(&word),
                }
            }
            None => Err(self.unexpected("a value")),
        }
    }

    fn key(&mut self) -> Result<String> {
        self.skip_ws();
        let rest = &self.text[self.pos..];
        let len = rest
            .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '$' | '.')))
            .unwrap_or(rest.len());
        if len == 0 {
            return Err(self.unexpected("a key"));
        }
        self.pos += len;
        Ok(rest[..len].to_string())
    }

    fn constructor(&mut self, name: &str) -> Result<Json> {
        let args = self.args()?;
        let first = args.first().cloned().unwrap_or(Json::Null);
        let text = match &first {
            Json::String(s) => s.clone(),
            Json::Number(n) => n.to_string(),
            _ => String::new(),
        };
        let tagged = |tag: &str, v: Json| Json::Object(Map::from_iter([(tag.to_string(), v)]));
        Ok(match name {
            "ObjectId" => tagged("$oid", Json::String(text)),
            "ISODate" | "Date" => {
                if text.is_empty() {
                    return Err("Date() needs a value".into());
                }
                tagged("$date", first)
            }
            "NumberLong" | "Long" => tagged("$numberLong", Json::String(text)),
            "NumberInt" | "Int32" => text
                .parse::<i32>()
                .map(Json::from)
                .map_err(|_| format!("Bad NumberInt: {text}"))?,
            "NumberDecimal" | "Decimal128" => tagged("$numberDecimal", Json::String(text)),
            "UUID" => tagged("$uuid", Json::String(text)),
            other => return Err(format!("{other}() is not supported")),
        })
    }

    fn string(&mut self) -> Result<String> {
        let quote = self.peek().ok_or("Expected a string")?;
        self.pos += 1;
        let mut out = String::new();
        let mut chars = self.text[self.pos..].char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                c if c == quote => {
                    self.pos += i + 1;
                    return Ok(out);
                }
                '\\' => match chars.next().map(|(_, c)| c) {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some(c) => out.push(c),
                    None => break,
                },
                c => out.push(c),
            }
        }
        Err("Unclosed string".into())
    }

    fn number(&mut self) -> Result<Json> {
        let rest = &self.text[self.pos..];
        let len = rest
            .find(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E')))
            .unwrap_or(rest.len());
        let text = &rest[..len];
        self.pos += len;
        if let Ok(i) = text.parse::<i64>() {
            return Ok(Json::from(i));
        }
        text.parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Json::Number)
            .ok_or_else(|| format!("Bad number: {text}"))
    }

    fn regex(&mut self) -> Result<Json> {
        self.pos += 1;
        let rest = &self.text[self.pos..];
        let mut end = None;
        let mut escaped = false;
        for (i, c) in rest.char_indices() {
            match c {
                '\\' if !escaped => escaped = true,
                '/' if !escaped => {
                    end = Some(i);
                    break;
                }
                _ => escaped = false,
            }
        }
        let end = end.ok_or("Unclosed regex")?;
        let pattern = rest[..end].to_string();
        self.pos += end + 1;
        let flags_len = self.text[self.pos..]
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(self.text.len() - self.pos);
        let options = self.text[self.pos..self.pos + flags_len].to_string();
        self.pos += flags_len;
        Ok(serde_json::json!({ "$regularExpression": { "pattern": pattern, "options": options } }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_shell_calls() {
        let call = parse_call(
            "db.users.find({ age: { $gt: 30 }, 'name': /^a/i, _id: ObjectId(\"64b7f0000000000000000000\"), },\n { name: 1 })\n  .sort({created: -1}).limit(5);",
        )
        .unwrap();
        assert_eq!(call.collection, "users");
        assert_eq!(call.method, "find");
        assert_eq!(
            call.args[0],
            json!({
                "age": {"$gt": 30},
                "name": {"$regularExpression": {"pattern": "^a", "options": "i"}},
                "_id": {"$oid": "64b7f0000000000000000000"}
            })
        );
        assert_eq!(call.args[1], json!({"name": 1}));
        assert_eq!(call.sort, Some(json!({"created": -1})));
        assert_eq!(call.limit, Some(5));
        assert!(!call.writes());

        let call = parse_call("db.getCollection(\"order items\").insertOne({at: new Date(\"2024-01-01T00:00:00Z\"), n: NumberLong(7)})").unwrap();
        assert_eq!(call.collection, "order items");
        assert_eq!(
            call.args[0],
            json!({"at": {"$date": "2024-01-01T00:00:00Z"}, "n": {"$numberLong": "7"}})
        );
        assert!(call.writes());
        assert!(
            parse_call("db.users.aggregate([{$match: {}}, {$out: 'x'}])")
                .unwrap()
                .writes()
        );
        assert_eq!(
            parse_call("show collections").unwrap().method,
            "showCollections"
        );
        assert!(parse_call("db.users.find({a: })").is_err());
        assert!(parse_call("db.users.find() junk").is_err());
    }

    #[test]
    fn documents_become_rows() {
        let docs = vec![
            mongodb::bson::doc! { "name": "ada", "_id": 1, "tags": ["a"] },
            mongodb::bson::doc! { "_id": 2, "age": 3.5 },
        ];
        let result = documents(docs);
        let names: Vec<_> = result.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["_id", "name", "tags", "age"]);
        assert_eq!(result.rows[0][2], Value::Json("[\"a\"]".into()));
        assert_eq!(
            result.rows[1],
            vec![Value::Int(2), Value::Null, Value::Null, Value::Float(3.5)]
        );
        assert!(document(json!({"_id": {"$oid": "64b7f0000000000000000000"}})).is_ok());
    }
}
