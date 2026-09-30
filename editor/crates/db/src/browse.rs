//! Browsing a table without writing a query: a filter, a sort and pages of
//! rows, turned into the query each engine runs. SQL results stay plain
//! single-table `SELECT`s, so they can still be edited.

use crate::{Engine, Object, Value, edit, sql};

/// What is being browsed and how.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Browse {
    pub namespace: Option<String>,
    pub table: String,
    /// A SQL condition (`status = 'paid'`) or a MongoDB filter document.
    pub filter: String,
    /// Column and whether it sorts descending.
    pub sort: Option<(String, bool)>,
    /// The primary key: the order when nothing else is chosen, so pages
    /// do not overlap.
    pub key: Vec<String>,
}

/// Rows fetched per page.
pub const PAGE: usize = 200;

impl Browse {
    pub fn of(object: &Object) -> Self {
        Browse {
            namespace: object.namespace.clone(),
            table: object.name.clone(),
            key: object
                .columns
                .iter()
                .filter(|c| c.primary_key)
                .map(|c| c.name.clone())
                .collect(),
            ..Default::default()
        }
    }

    fn filter(&self) -> Option<&str> {
        Some(self.filter.trim()).filter(|f| !f.is_empty())
    }
}

/// Rows `offset..offset + limit` in the chosen order.
pub fn page_query(engine: Engine, browse: &Browse, limit: usize, offset: usize) -> String {
    match engine {
        Engine::Mongo => {
            let mut q = format!(
                "{}.find({})",
                mongo_collection(&browse.table),
                browse.filter().unwrap_or("{}")
            );
            if let Some((column, desc)) = &browse.sort {
                q.push_str(&format!(
                    ".sort({{{}: {}}})",
                    json_string(column),
                    if *desc { -1 } else { 1 }
                ));
            }
            if offset > 0 {
                q.push_str(&format!(".skip({offset})"));
            }
            q.push_str(&format!(".limit({limit})"));
            q
        }
        _ => {
            let mut q = format!(
                "SELECT * FROM {}",
                sql::qualified_name(engine, browse.namespace.as_deref(), &browse.table)
            );
            if let Some(filter) = browse.filter() {
                q.push_str(&format!(" WHERE {filter}"));
            }
            let order: Vec<String> = match &browse.sort {
                Some((column, desc)) => vec![format!(
                    "{}{}",
                    sql::quote_ident(engine, column),
                    if *desc { " DESC" } else { "" }
                )],
                None => browse
                    .key
                    .iter()
                    .map(|k| sql::quote_ident(engine, k))
                    .collect(),
            };
            if !order.is_empty() {
                q.push_str(&format!(" ORDER BY {}", order.join(", ")));
            }
            q.push_str(&format!(" LIMIT {limit}"));
            if offset > 0 {
                q.push_str(&format!(" OFFSET {offset}"));
            }
            q
        }
    }
}

/// How many rows match the filter.
pub fn count_query(engine: Engine, browse: &Browse) -> String {
    match engine {
        Engine::Mongo => format!(
            "{}.countDocuments({})",
            mongo_collection(&browse.table),
            browse.filter().unwrap_or("{}")
        ),
        _ => {
            let mut q = format!(
                "SELECT count(*) FROM {}",
                sql::qualified_name(engine, browse.namespace.as_deref(), &browse.table)
            );
            if let Some(filter) = browse.filter() {
                q.push_str(&format!(" WHERE {filter}"));
            }
            q
        }
    }
}

/// A condition matching `value` in `column`, for "filter by this value".
pub fn value_filter(engine: Engine, column: &str, value: &Value) -> String {
    match engine {
        Engine::Mongo => {
            let value = match value {
                Value::Null => "null".to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Int(i) => i.to_string(),
                Value::Float(f) => f.to_string(),
                Value::Number(n) => n.clone(),
                Value::Json(j) => j.clone(),
                Value::Text(t) => match t
                    .strip_prefix("ObjectId(\"")
                    .and_then(|r| r.strip_suffix("\")"))
                {
                    Some(id) => format!("ObjectId({})", json_string(id)),
                    None => json_string(t),
                },
                Value::Bytes(_) => json_string(&value.display()),
            };
            format!("{{{}: {value}}}", json_string(column))
        }
        _ => {
            let column = sql::quote_ident(engine, column);
            match value {
                Value::Null => format!("{column} IS NULL"),
                value => format!("{column} = {}", edit::literal(engine, value)),
            }
        }
    }
}

/// Both conditions: `a AND b` in SQL, `{$and: [a, b]}` in MongoDB.
pub fn and(engine: Engine, current: &str, extra: &str) -> String {
    let current = current.trim();
    if current.is_empty() {
        return extra.to_string();
    }
    match engine {
        Engine::Mongo => format!("{{$and: [{current}, {extra}]}}"),
        _ => format!("({current}) AND {extra}"),
    }
}

fn mongo_collection(name: &str) -> String {
    format!("db.getCollection({})", json_string(name))
}

fn json_string(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn orders() -> Browse {
        Browse {
            namespace: Some("public".into()),
            table: "orders".into(),
            key: vec!["id".into()],
            ..Default::default()
        }
    }

    #[test]
    fn sql_pages_are_ordered_by_key_unless_sorted() {
        let mut b = orders();
        assert_eq!(
            page_query(Engine::Postgres, &b, 200, 0),
            "SELECT * FROM \"orders\" ORDER BY \"id\" LIMIT 200"
        );
        b.filter = "status = 'paid'".into();
        b.sort = Some(("total".into(), true));
        assert_eq!(
            page_query(Engine::MySql, &b, 200, 400),
            "SELECT * FROM `public`.`orders` WHERE status = 'paid' ORDER BY `total` DESC LIMIT 200 OFFSET 400"
        );
        assert_eq!(
            count_query(Engine::Postgres, &b),
            "SELECT count(*) FROM \"orders\" WHERE status = 'paid'"
        );
    }

    #[test]
    fn mongo_pages_and_counts() {
        let mut b = Browse {
            table: "people".into(),
            ..Default::default()
        };
        b.filter = "{age: {$gt: 30}}".into();
        b.sort = Some(("name".into(), false));
        assert_eq!(
            page_query(Engine::Mongo, &b, 200, 200),
            "db.getCollection(\"people\").find({age: {$gt: 30}}).sort({\"name\": 1}).skip(200).limit(200)"
        );
        assert_eq!(
            count_query(Engine::Mongo, &b),
            "db.getCollection(\"people\").countDocuments({age: {$gt: 30}})"
        );
    }

    #[test]
    fn filters_by_value_and_combines() {
        assert_eq!(
            value_filter(Engine::Postgres, "status", &Value::Text("it's".into())),
            "\"status\" = 'it''s'"
        );
        assert_eq!(
            value_filter(Engine::Sqlite, "bio", &Value::Null),
            "\"bio\" IS NULL"
        );
        assert_eq!(
            value_filter(
                Engine::Mongo,
                "_id",
                &Value::Text("ObjectId(\"64b7f0000000000000000000\")".into())
            ),
            "{\"_id\": ObjectId(\"64b7f0000000000000000000\")}"
        );
        assert_eq!(and(Engine::Postgres, "", "a = 1"), "a = 1");
        assert_eq!(
            and(Engine::Postgres, "a = 1 OR b = 2", "c = 3"),
            "(a = 1 OR b = 2) AND c = 3"
        );
        assert_eq!(
            and(Engine::Mongo, "{a: 1}", "{b: 2}"),
            "{$and: [{a: 1}, {b: 2}]}"
        );
    }
}
