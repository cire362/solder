//! Completion from the schema: tables, columns and keywords for SQL,
//! commands for Redis, collections, methods and fields for MongoDB.

use std::collections::HashSet;

use crate::{Engine, Object, ObjectKind, Schema, sql};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateKind {
    Keyword,
    Table,
    Column,
    Index,
    Method,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub label: String,
    pub kind: CandidateKind,
    pub detail: String,
}

const SQL_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS",
    "IN",
    "AS",
    "ON",
    "JOIN",
    "LEFT JOIN",
    "RIGHT JOIN",
    "INNER JOIN",
    "GROUP BY",
    "ORDER BY",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "DISTINCT",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE FROM",
    "RETURNING",
    "WITH",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "UNION",
    "ASC",
    "DESC",
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "COALESCE",
    "CREATE TABLE",
    "ALTER TABLE",
    "DROP TABLE",
    "CREATE INDEX",
    "DROP INDEX",
    "PRIMARY KEY",
    "DEFAULT",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "EXPLAIN",
];

const REDIS_COMMANDS: &[&str] = &[
    "GET", "SET", "DEL", "EXISTS", "EXPIRE", "TTL", "TYPE", "KEYS", "SCAN", "INCR", "DECR", "MGET",
    "MSET", "HGET", "HSET", "HGETALL", "HDEL", "HKEYS", "HVALS", "LPUSH", "RPUSH", "LPOP", "RPOP",
    "LRANGE", "LLEN", "SADD", "SREM", "SMEMBERS", "SCARD", "ZADD", "ZRANGE", "ZREM", "ZSCORE",
    "ZCARD", "XADD", "XRANGE", "XLEN", "PING", "INFO", "DBSIZE",
];

const MONGO_METHODS: &[&str] = &[
    "find",
    "findOne",
    "aggregate",
    "countDocuments",
    "distinct",
    "insertOne",
    "insertMany",
    "updateOne",
    "updateMany",
    "replaceOne",
    "deleteOne",
    "deleteMany",
];

/// Candidates for a cursor at `offset` in `text`. The editor filters them by
/// the word being typed.
pub fn candidates(engine: Engine, schema: &Schema, text: &str, offset: usize) -> Vec<Candidate> {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let word_start = before
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    let qualifier = before[..word_start].strip_suffix('.').map(|q| {
        let start = q
            .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '"' || c == '`'))
            .map_or(0, |i| i + 1);
        q[start..].trim_matches(['"', '`']).to_string()
    });
    match engine {
        Engine::Redis => redis_candidates(schema, before),
        Engine::Mongo => mongo_candidates(schema, before, qualifier.as_deref()),
        _ => sql_candidates(engine, schema, text, offset, qualifier.as_deref()),
    }
}

fn sql_candidates(
    engine: Engine,
    schema: &Schema,
    text: &str,
    offset: usize,
    qualifier: Option<&str>,
) -> Vec<Candidate> {
    let statement = sql::statement_at(engine, text, offset)
        .map_or("", |r| &text[r.start..r.end.max(offset).min(text.len())]);
    let refs = table_refs(statement);
    let find = |name: &str| -> Option<&Object> {
        schema
            .objects
            .iter()
            .find(|o| o.name.eq_ignore_ascii_case(name))
    };
    if let Some(qualifier) = qualifier {
        // `alias.` or `table.`: that table's columns.
        let table = refs
            .iter()
            .find(|(_, alias)| {
                alias
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(qualifier))
            })
            .map(|(t, _)| t.as_str())
            .unwrap_or(qualifier);
        if let Some(object) = find(table) {
            return columns(object, false);
        }
        // `schema.`: its tables.
        return schema
            .objects
            .iter()
            .filter(|o| {
                o.namespace
                    .as_deref()
                    .is_some_and(|ns| ns.eq_ignore_ascii_case(qualifier))
            })
            .map(table_candidate)
            .collect();
    }
    let previous = previous_keyword(&text[..offset]);
    let mut out = Vec::new();
    let wants_table = matches!(
        previous.as_str(),
        "FROM" | "JOIN" | "INTO" | "UPDATE" | "TABLE" | "EXISTS"
    );
    if previous == "INDEX" {
        for object in &schema.objects {
            out.extend(object.indexes.iter().map(|i| Candidate {
                label: i.name.clone(),
                kind: CandidateKind::Index,
                detail: format!("index on {}", object.name),
            }));
        }
        return out;
    }
    if !wants_table {
        let mut seen = HashSet::new();
        for (table, _) in &refs {
            if let Some(object) = find(table) {
                for c in columns(object, refs.len() > 1) {
                    if seen.insert(c.label.clone()) {
                        out.push(c);
                    }
                }
            }
        }
    }
    out.extend(schema.objects.iter().map(table_candidate));
    if !wants_table {
        out.extend(SQL_KEYWORDS.iter().map(|k| Candidate {
            label: (*k).into(),
            kind: CandidateKind::Keyword,
            detail: String::new(),
        }));
    }
    out
}

fn table_candidate(object: &Object) -> Candidate {
    Candidate {
        label: object.name.clone(),
        kind: CandidateKind::Table,
        detail: match (&object.kind, &object.namespace) {
            (ObjectKind::View, _) => "view".into(),
            (_, Some(ns)) => ns.clone(),
            _ => "table".into(),
        },
    }
}

fn columns(object: &Object, qualify_detail: bool) -> Vec<Candidate> {
    object
        .columns
        .iter()
        .map(|c| Candidate {
            label: c.name.clone(),
            kind: CandidateKind::Column,
            detail: if qualify_detail {
                format!("{}.{}", object.name, c.type_name)
            } else {
                c.type_name.clone()
            },
        })
        .collect()
}

/// Tables named after FROM (a comma-separated list), JOIN, UPDATE and INTO,
/// with their aliases.
pub(crate) fn table_refs(statement: &str) -> Vec<(String, Option<String>)> {
    let words = tokens(statement);
    let mut refs = Vec::new();
    for (i, word) in words.iter().enumerate() {
        let upper = word.to_ascii_uppercase();
        if !matches!(upper.as_str(), "FROM" | "JOIN" | "UPDATE" | "INTO") {
            continue;
        }
        let mut next = i + 1;
        while let Some(name) = words.get(next) {
            if is_keyword(name) || name == "," {
                break;
            }
            // `schema.table`: the last part names the table.
            let table = name.rsplit('.').next().unwrap_or(name).to_string();
            next += 1;
            if words
                .get(next)
                .is_some_and(|w| w.eq_ignore_ascii_case("AS"))
            {
                next += 1;
            }
            let alias = words
                .get(next)
                .filter(|w| !is_keyword(w) && w.chars().all(|c| c.is_alphanumeric() || c == '_'))
                .cloned();
            if alias.is_some() {
                next += 1;
            }
            refs.push((table, alias));
            if upper != "FROM" || words.get(next).map(String::as_str) != Some(",") {
                break;
            }
            next += 1;
        }
    }
    refs
}

fn is_keyword(word: &str) -> bool {
    let upper = word.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "WHERE"
            | "ON"
            | "JOIN"
            | "LEFT"
            | "RIGHT"
            | "INNER"
            | "OUTER"
            | "FULL"
            | "CROSS"
            | "GROUP"
            | "ORDER"
            | "LIMIT"
            | "SET"
            | "VALUES"
            | "SELECT"
            | "USING"
            | "HAVING"
            | "UNION"
            | "RETURNING"
            | "OFFSET"
            | "AS"
            | "NATURAL"
            | "LATERAL"
            | "WINDOW"
    )
}

/// Identifiers, keywords and commas, with quotes removed; strings and
/// comments skipped.
pub(crate) fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '\'' => {
                for (_, c) in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                }
            }
            '-' if text[i..].starts_with("--") => {
                for (_, c) in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            ',' => out.push(",".into()),
            c if c.is_alphanumeric() || matches!(c, '_' | '"' | '`') => {
                let mut end = i + c.len_utf8();
                while let Some(&(j, c)) = chars.peek() {
                    if c.is_alphanumeric() || matches!(c, '_' | '"' | '`' | '.') {
                        end = j + c.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push(text[i..end].replace(['"', '`'], ""));
            }
            _ => {}
        }
    }
    out
}

/// The last keyword before the word being typed, skipping the word itself.
fn previous_keyword(before: &str) -> String {
    let trimmed = before.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
    tokens(trimmed)
        .into_iter()
        .rfind(|t| t != ",")
        .map(|w| w.to_ascii_uppercase())
        .unwrap_or_default()
}

fn redis_candidates(schema: &Schema, before: &str) -> Vec<Candidate> {
    let line = before.rsplit('\n').next().unwrap_or(before);
    if !line.trim_start().contains(char::is_whitespace) {
        return REDIS_COMMANDS
            .iter()
            .map(|c| Candidate {
                label: (*c).into(),
                kind: CandidateKind::Keyword,
                detail: String::new(),
            })
            .collect();
    }
    schema
        .objects
        .iter()
        .map(|o| Candidate {
            label: o.name.clone(),
            kind: CandidateKind::Table,
            detail: match &o.kind {
                ObjectKind::Key(t) => t.clone(),
                _ => String::new(),
            },
        })
        .collect()
}

fn mongo_candidates(schema: &Schema, before: &str, qualifier: Option<&str>) -> Vec<Candidate> {
    match qualifier {
        Some("db") => schema.objects.iter().map(table_candidate).collect(),
        Some(_)
            if before
                .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_')
                .ends_with('.') =>
        {
            MONGO_METHODS
                .iter()
                .map(|m| Candidate {
                    label: (*m).into(),
                    kind: CandidateKind::Method,
                    detail: String::new(),
                })
                .collect()
        }
        _ => {
            // Inside a call: the fields of the collection it is on.
            let collection = before.rfind("db.").and_then(|i| {
                let rest = &before[i + 3..];
                rest.split('.').next().map(str::to_string)
            });
            schema
                .objects
                .iter()
                .filter(|o| collection.as_deref() == Some(o.name.as_str()))
                .flat_map(|o| columns(o, false))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ColumnInfo;

    fn schema() -> Schema {
        let table = |name: &str, cols: &[&str]| Object {
            namespace: Some("public".into()),
            name: name.into(),
            kind: ObjectKind::Table,
            columns: cols
                .iter()
                .map(|c| ColumnInfo {
                    name: (*c).into(),
                    type_name: "text".into(),
                    nullable: true,
                    primary_key: *c == "id",
                    ..Default::default()
                })
                .collect(),
            indexes: vec![crate::Index {
                name: format!("{name}_pkey"),
                ..Default::default()
            }],
            ..Default::default()
        };
        Schema {
            objects: vec![
                table("customers", &["id", "name", "email"]),
                table("orders", &["id", "customer_id", "status"]),
            ],
            truncated: false,
        }
    }

    fn labels(engine: Engine, text: &str) -> Vec<String> {
        let offset = text.find('|').unwrap();
        let text = text.replace('|', "");
        candidates(engine, &schema(), &text, offset)
            .into_iter()
            .map(|c| c.label)
            .collect()
    }

    #[test]
    fn sql_columns_of_the_tables_in_the_statement() {
        let got = labels(Engine::Postgres, "select sta| from orders where id = 1");
        assert_eq!(&got[..3], ["id", "customer_id", "status"]);
        assert!(got.contains(&"customers".to_string()) && got.contains(&"SELECT".to_string()));
        assert!(!got.contains(&"email".to_string()));
    }

    #[test]
    fn sql_aliases_tables_and_indexes() {
        let got = labels(
            Engine::Postgres,
            "select o.| from orders o join customers c on c.id = o.customer_id",
        );
        assert_eq!(got, ["id", "customer_id", "status"]);
        let got = labels(
            Engine::Postgres,
            "select c.na| from \"orders\" as o, customers c",
        );
        assert_eq!(got, ["id", "name", "email"]);
        assert_eq!(
            labels(Engine::Postgres, "select * from public.|"),
            ["customers", "orders"]
        );
        assert_eq!(
            labels(Engine::Postgres, "select * from cu|"),
            ["customers", "orders"]
        );
        assert_eq!(
            labels(Engine::Postgres, "drop index |"),
            ["customers_pkey", "orders_pkey"]
        );
        // Only the statement under the cursor counts.
        let got = labels(
            Engine::Postgres,
            "select * from customers;\nselect | from orders",
        );
        assert!(got.contains(&"status".to_string()) && !got.contains(&"email".to_string()));
    }

    #[test]
    fn redis_and_mongo() {
        assert!(labels(Engine::Redis, "HG|").contains(&"HGETALL".to_string()));
        assert_eq!(
            labels(Engine::Redis, "HGETALL cu|"),
            ["customers", "orders"]
        );
        assert_eq!(labels(Engine::Mongo, "db.|"), ["customers", "orders"]);
        assert!(labels(Engine::Mongo, "db.orders.fi|").contains(&"find".to_string()));
        assert_eq!(
            labels(Engine::Mongo, "db.orders.find({sta|"),
            ["id", "customer_id", "status"]
        );
    }
}
