//! Editing MongoDB documents and Redis keys from the results grid: which
//! collection or key a result shows, and the calls or commands that save
//! staged changes.

use std::collections::BTreeSet;

use crate::{
    Column, Engine, Value,
    browse::value_filter,
    edit::{Changes, EditTarget, KeyKind, TargetKind},
    mongo, redis,
};

/// Documents from a `find` on one collection, editable by `_id`.
pub fn documents_target(query: &str, columns: &[Column]) -> Result<EditTarget, String> {
    let collection = mongo::find_collection(query)
        .ok_or("Only documents from a find() on one collection can be edited")?;
    let id = columns
        .iter()
        .position(|c| c.name == "_id")
        .ok_or("Keep _id in the results to edit documents")?;
    Ok(EditTarget {
        table: collection,
        key: vec![id],
        // MongoDB makes an _id for new documents.
        auto: vec![id],
        kind: TargetKind::Documents,
        ..Default::default()
    })
}

/// A Redis key shown by the command that reads its whole value.
pub fn key_target(query: &str, columns: &[Column]) -> Result<EditTarget, String> {
    let args = redis::split_args(query)?;
    let not_a_key = || "Only a key opened from the Database tab can be edited".to_string();
    let command = args
        .first()
        .map(|c| c.to_ascii_uppercase())
        .ok_or_else(not_a_key)?;
    let key = args.get(1).cloned().ok_or_else(not_a_key)?;
    let kind = match command.as_str() {
        "GET" => KeyKind::String,
        "HGETALL" => KeyKind::Hash,
        "LRANGE" => KeyKind::List,
        "SMEMBERS" => KeyKind::Set,
        "ZRANGE" if args.iter().any(|a| a.eq_ignore_ascii_case("withscores")) => KeyKind::SortedSet,
        _ => return Err(not_a_key()),
    };
    // The `#` column numbers list items and set members; it is not data.
    let numbered = columns.first().is_some_and(|c| c.name == "#");
    Ok(EditTarget {
        table: key,
        key: vec![0],
        auto: if numbered { vec![0] } else { Vec::new() },
        kind: TargetKind::Key(kind),
        ..Default::default()
    })
}

/// A value as the shell reads it. Text stays text unless it is clearly
/// something else (quoted, an object, an array or `ObjectId(...)`), so a
/// name like `true` or `42` in a string field stays a string.
fn shell_value(typed: &str, original: Option<&Value>) -> String {
    let as_string = || serde_json::Value::String(typed.to_string()).to_string();
    let text_field = matches!(original, Some(Value::Text(t)) if !t.starts_with("ObjectId("));
    let explicit = typed.starts_with(['"', '\'', '{', '['])
        || typed.starts_with("ObjectId(")
        || typed.starts_with("ISODate(");
    if text_field && !explicit {
        return as_string();
    }
    if mongo::is_shell_value(typed) {
        typed.to_string()
    } else {
        as_string()
    }
}

fn field_name(name: &str) -> String {
    serde_json::Value::String(name.to_string()).to_string()
}

pub fn document_statements(
    target: &EditTarget,
    columns: &[Column],
    rows: &[Vec<Value>],
    changes: &Changes,
) -> Vec<String> {
    let collection = format!(
        "db.getCollection({})",
        serde_json::Value::String(target.table.clone())
    );
    let id = target.key[0];
    let filter = |row: &[Value]| value_filter(Engine::Mongo, "_id", &row[id]);
    let mut out = Vec::new();
    let changed: BTreeSet<usize> = changes.cells.keys().map(|(r, _)| *r).collect();
    for &row in changed
        .iter()
        .chain(changes.deleted.iter())
        .collect::<BTreeSet<_>>()
    {
        let Some(values) = rows.get(row) else {
            continue;
        };
        if changes.deleted.contains(&row) {
            out.push(format!("{collection}.deleteOne({})", filter(values)));
            continue;
        }
        let set: Vec<String> = changes
            .cells
            .range((row, 0)..(row + 1, 0))
            .filter(|((_, c), _)| *c != id)
            .map(|((_, c), v)| {
                let value = match v {
                    None => "null".to_string(),
                    Some(text) => shell_value(text, values.get(*c)),
                };
                format!("{}: {value}", field_name(&columns[*c].name))
            })
            .collect();
        if !set.is_empty() {
            out.push(format!(
                "{collection}.updateOne({}, {{$set: {{{}}}}})",
                filter(values),
                set.join(", ")
            ));
        }
    }
    for new in &changes.inserted {
        let fields: Vec<String> = new
            .iter()
            .map(|(c, v)| {
                let value = match v {
                    None => "null".to_string(),
                    Some(text) => shell_value(text, None),
                };
                format!("{}: {value}", field_name(&columns[*c].name))
            })
            .collect();
        out.push(format!("{collection}.insertOne({{{}}})", fields.join(", ")));
    }
    out
}

/// Commands that make `changes` to one key. Rows are the key's members as
/// read: hash fields, list items (in order), set members, sorted set
/// members with scores.
pub fn key_statements(
    kind: KeyKind,
    target: &EditTarget,
    rows: &[Vec<Value>],
    changes: &Changes,
) -> Vec<String> {
    let key = redis::quote_arg(&target.table);
    let q = |s: &str| redis::quote_arg(s);
    // The value of column `c` in `row` after its staged change.
    let cell = |row: usize, c: usize| -> Option<String> {
        match changes.cells.get(&(row, c)) {
            Some(v) => v.clone(),
            None => rows.get(row).and_then(|r| r.get(c)).map(Value::display),
        }
    };
    let original = |row: usize, c: usize| {
        rows.get(row)
            .and_then(|r| r.get(c))
            .map(Value::display)
            .unwrap_or_default()
    };
    let changed: BTreeSet<usize> = changes.cells.keys().map(|(r, _)| *r).collect();
    let mut out = Vec::new();
    match kind {
        KeyKind::String => {
            if changes.deleted.contains(&0) {
                out.push(format!("DEL {key}"));
            } else if changed.contains(&0) || !changes.inserted.is_empty() {
                let value = changes
                    .inserted
                    .last()
                    .and_then(|r| r.get(&0).cloned().flatten())
                    .or_else(|| cell(0, 0))
                    .unwrap_or_default();
                out.push(format!("SET {key} {} KEEPTTL", q(&value)));
            }
        }
        KeyKind::Hash => {
            for &row in &changed {
                if changes.deleted.contains(&row) {
                    continue;
                }
                let (field, value) = (original(row, 0), cell(row, 1).unwrap_or_default());
                let renamed = cell(row, 0).unwrap_or_default();
                if renamed != field {
                    out.push(format!("HDEL {key} {}", q(&field)));
                }
                out.push(format!("HSET {key} {} {}", q(&renamed), q(&value)));
            }
            for &row in &changes.deleted {
                out.push(format!("HDEL {key} {}", q(&original(row, 0))));
            }
            for new in &changes.inserted {
                let get = |c: usize| new.get(&c).cloned().flatten().unwrap_or_default();
                out.push(format!("HSET {key} {} {}", q(&get(0)), q(&get(1))));
            }
        }
        KeyKind::List => {
            for &row in &changed {
                if !changes.deleted.contains(&row) {
                    out.push(format!(
                        "LSET {key} {row} {}",
                        q(&cell(row, 1).unwrap_or_default())
                    ));
                }
            }
            if !changes.deleted.is_empty() {
                // Items go by position: mark them, then remove the marks.
                let mark = "__solder_deleted__";
                for &row in &changes.deleted {
                    out.push(format!("LSET {key} {row} {mark}"));
                }
                out.push(format!("LREM {key} 0 {mark}"));
            }
            for new in &changes.inserted {
                out.push(format!(
                    "RPUSH {key} {}",
                    q(&new.get(&1).cloned().flatten().unwrap_or_default())
                ));
            }
        }
        KeyKind::Set => {
            for &row in &changed {
                if !changes.deleted.contains(&row) {
                    out.push(format!("SREM {key} {}", q(&original(row, 1))));
                    out.push(format!(
                        "SADD {key} {}",
                        q(&cell(row, 1).unwrap_or_default())
                    ));
                }
            }
            for &row in &changes.deleted {
                out.push(format!("SREM {key} {}", q(&original(row, 1))));
            }
            for new in &changes.inserted {
                out.push(format!(
                    "SADD {key} {}",
                    q(&new.get(&1).cloned().flatten().unwrap_or_default())
                ));
            }
        }
        KeyKind::SortedSet => {
            for &row in &changed {
                if changes.deleted.contains(&row) {
                    continue;
                }
                let (member, renamed) = (original(row, 0), cell(row, 0).unwrap_or_default());
                if renamed != member {
                    out.push(format!("ZREM {key} {}", q(&member)));
                }
                out.push(format!(
                    "ZADD {key} {} {}",
                    q(&cell(row, 1).unwrap_or_else(|| "0".into())),
                    q(&renamed)
                ));
            }
            for &row in &changes.deleted {
                out.push(format!("ZREM {key} {}", q(&original(row, 0))));
            }
            for new in &changes.inserted {
                let get = |c: usize, or: &str| {
                    new.get(&c).cloned().flatten().unwrap_or_else(|| or.into())
                };
                out.push(format!("ZADD {key} {} {}", q(&get(1, "0")), q(&get(0, ""))));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn columns(names: &[&str]) -> Vec<Column> {
        names
            .iter()
            .map(|n| Column {
                name: (*n).into(),
                type_name: String::new(),
            })
            .collect()
    }

    #[test]
    fn documents_update_delete_and_insert_by_id() {
        let cols = columns(&["_id", "name", "age"]);
        let target = documents_target(
            "db.getCollection(\"people\").find({}).sort({\"name\": 1}).limit(200)",
            &cols,
        )
        .unwrap();
        assert_eq!(
            (target.table.as_str(), target.key.clone()),
            ("people", vec![0])
        );
        assert!(documents_target("db.people.aggregate([])", &cols).is_err());
        assert!(documents_target("db.people.find({})", &columns(&["name"])).is_err());
        let rows = vec![
            vec![
                Value::Text("ObjectId(\"64b7f0000000000000000001\")".into()),
                Value::Text("ada".into()),
                Value::Int(36),
            ],
            vec![Value::Int(2), Value::Text("bob".into()), Value::Int(25)],
        ];
        let mut changes = Changes::default();
        changes.cells.insert((0, 1), Some("true".into()));
        changes.cells.insert((0, 2), Some("37".into()));
        changes.deleted.insert(1);
        changes.inserted.push(BTreeMap::from([
            (1, Some("cy".into())),
            (2, Some("{y: 1}".into())),
        ]));
        assert_eq!(
            document_statements(&target, &cols, &rows, &changes),
            [
                "db.getCollection(\"people\").updateOne({\"_id\": ObjectId(\"64b7f0000000000000000001\")}, {$set: {\"name\": \"true\", \"age\": 37}})",
                "db.getCollection(\"people\").deleteOne({\"_id\": 2})",
                "db.getCollection(\"people\").insertOne({\"name\": \"cy\", \"age\": {y: 1}})",
            ]
        );
    }

    #[test]
    fn redis_commands_per_type() {
        let hash = key_target("HGETALL \"user:1\"", &columns(&["field", "value"])).unwrap();
        assert_eq!(hash.kind, TargetKind::Key(KeyKind::Hash));
        let rows = vec![
            vec![Value::Text("name".into()), Value::Text("ada".into())],
            vec![Value::Text("lang".into()), Value::Text("rust".into())],
        ];
        let mut changes = Changes::default();
        changes.cells.insert((0, 0), Some("full name".into()));
        changes.cells.insert((0, 1), Some("Ada L".into()));
        changes.deleted.insert(1);
        changes.inserted.push(BTreeMap::from([
            (0, Some("age".into())),
            (1, Some("36".into())),
        ]));
        assert_eq!(
            key_statements(KeyKind::Hash, &hash, &rows, &changes),
            [
                "HDEL user:1 name",
                "HSET user:1 \"full name\" \"Ada L\"",
                "HDEL user:1 lang",
                "HSET user:1 age 36",
            ]
        );

        let list = key_target("LRANGE q 0 199", &columns(&["#", "value"])).unwrap();
        assert_eq!(list.auto, [0]);
        let rows = vec![
            vec![Value::Int(1), Value::Text("a".into())],
            vec![Value::Int(2), Value::Text("b".into())],
        ];
        let mut changes = Changes::default();
        changes.cells.insert((1, 1), Some("B".into()));
        changes.deleted.insert(0);
        changes
            .inserted
            .push(BTreeMap::from([(1, Some("c".into()))]));
        assert_eq!(
            key_statements(KeyKind::List, &list, &rows, &changes),
            [
                "LSET q 1 B",
                "LSET q 0 __solder_deleted__",
                "LREM q 0 __solder_deleted__",
                "RPUSH q c"
            ]
        );

        let zset = key_target(
            "ZRANGE board 0 199 WITHSCORES",
            &columns(&["field", "value"]),
        )
        .unwrap();
        let rows = vec![vec![Value::Text("ada".into()), Value::Text("10".into())]];
        let mut changes = Changes::default();
        changes.cells.insert((0, 1), Some("12".into()));
        assert_eq!(
            key_statements(KeyKind::SortedSet, &zset, &rows, &changes),
            ["ZADD board 12 ada"]
        );

        let string = key_target("GET greeting", &columns(&["value"])).unwrap();
        let rows = vec![vec![Value::Text("hi".into())]];
        let mut changes = Changes::default();
        changes.cells.insert((0, 0), Some("hello there".into()));
        assert_eq!(
            key_statements(KeyKind::String, &string, &rows, &changes),
            ["SET greeting \"hello there\" KEEPTTL"]
        );
        assert!(key_target("KEYS *", &columns(&["#", "value"])).is_err());
    }
}
