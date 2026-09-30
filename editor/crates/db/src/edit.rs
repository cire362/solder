//! Editing rows in a result: which table a result can be written back to,
//! and the statements that apply staged changes. Every statement targets one
//! row by its primary key, with the values the grid showed, so a row that
//! changed underneath is caught (it matches nothing) instead of overwritten.

use std::collections::{BTreeMap, BTreeSet};

use crate::{Column, Engine, Object, Schema, Value, complete, sql};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditTarget {
    pub namespace: Option<String>,
    pub table: String,
    /// Result columns that hold the primary key, by index.
    pub key: Vec<usize>,
}

/// The table a result came from, if its rows can be written back: a plain
/// `SELECT` from one table with a primary key, whose result has every key
/// column and only real columns (no expressions or renames).
pub fn edit_target(
    engine: Engine,
    schema: &Schema,
    query: &str,
    columns: &[Column],
) -> Result<EditTarget, String> {
    if !engine.is_sql() {
        return Err("Editing works for Postgres, MySQL and SQLite results".into());
    }
    let words: Vec<String> = complete::tokens(query)
        .into_iter()
        .map(|w| w.to_ascii_uppercase())
        .collect();
    if words.first().map(String::as_str) != Some("SELECT") {
        return Err("Only rows from a SELECT can be edited".into());
    }
    let joined = [
        "JOIN",
        "UNION",
        "GROUP",
        "DISTINCT",
        "HAVING",
        "INTERSECT",
        "EXCEPT",
    ];
    if words.iter().skip(1).any(|w| w == "SELECT")
        || words.iter().any(|w| joined.contains(&w.as_str()))
    {
        return Err("Rows from joins, groups or subqueries cannot be edited".into());
    }
    let refs = complete::table_refs(query);
    let [(table, _)] = refs.as_slice() else {
        return Err("Rows from several tables cannot be edited".into());
    };
    let qualified = complete::tokens(query).into_iter().find(|t| {
        t.rsplit('.')
            .next()
            .is_some_and(|last| last.eq_ignore_ascii_case(table))
    });
    let namespace = qualified
        .as_deref()
        .and_then(|q| q.rsplit_once('.'))
        .map(|(ns, _)| ns.to_string());
    let object: &Object = schema
        .objects
        .iter()
        .filter(|o| o.name.eq_ignore_ascii_case(table))
        .find(|o| {
            namespace.is_none()
                || o.namespace.as_deref().map(str::to_ascii_lowercase)
                    == namespace.as_deref().map(str::to_ascii_lowercase)
        })
        .ok_or_else(|| format!("{table} is not in the schema"))?;
    let mut seen = BTreeSet::new();
    for column in columns {
        if !seen.insert(column.name.as_str()) {
            return Err(format!("{} appears twice", column.name));
        }
        if !object.columns.iter().any(|c| c.name == column.name) {
            return Err(format!(
                "{} is not a column of {}",
                column.name, object.name
            ));
        }
    }
    let keys: Vec<&str> = object
        .columns
        .iter()
        .filter(|c| c.primary_key)
        .map(|c| c.name.as_str())
        .collect();
    if keys.is_empty() {
        return Err(format!("{} has no primary key", object.name));
    }
    let key = keys
        .iter()
        .map(|k| {
            columns
                .iter()
                .position(|c| c.name == *k)
                .ok_or_else(|| format!("Select the key column {k} to edit rows"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(EditTarget {
        namespace: object.namespace.clone(),
        table: object.name.clone(),
        key,
    })
}

/// Staged changes to a result: new cell values (`None` is NULL) and rows to
/// delete. A deleted row's cell edits are ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub cells: BTreeMap<(usize, usize), Option<String>>,
    pub deleted: BTreeSet<usize>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.deleted.is_empty()
    }

    /// Changed rows, each counted once.
    pub fn len(&self) -> usize {
        let mut rows: BTreeSet<usize> = self.cells.keys().map(|(row, _)| *row).collect();
        rows.extend(&self.deleted);
        rows.len()
    }
}

/// One statement per changed row, in row order.
pub fn statements(
    engine: Engine,
    target: &EditTarget,
    columns: &[Column],
    rows: &[Vec<Value>],
    changes: &Changes,
) -> Vec<String> {
    let table = sql::qualified_name(engine, target.namespace.as_deref(), &target.table);
    let mut by_row: BTreeMap<usize, Vec<(usize, &Option<String>)>> = BTreeMap::new();
    for ((row, column), value) in &changes.cells {
        if !changes.deleted.contains(row) {
            by_row.entry(*row).or_default().push((*column, value));
        }
    }
    let condition = |row: &[Value]| -> String {
        target
            .key
            .iter()
            .map(|&k| {
                let name = sql::quote_ident(engine, &columns[k].name);
                match &row[k] {
                    Value::Null => format!("{name} IS NULL"),
                    value => format!("{name} = {}", literal(engine, value)),
                }
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let mut out: BTreeMap<usize, String> = BTreeMap::new();
    for (row, cells) in by_row {
        let Some(values) = rows.get(row) else {
            continue;
        };
        let set = cells
            .iter()
            .map(|(column, value)| {
                format!(
                    "{} = {}",
                    sql::quote_ident(engine, &columns[*column].name),
                    value
                        .as_deref()
                        .map_or_else(|| "NULL".into(), |v| text_literal(engine, v))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.insert(
            row,
            format!("UPDATE {table} SET {set} WHERE {}", condition(values)),
        );
    }
    for &row in &changes.deleted {
        if let Some(values) = rows.get(row) {
            out.insert(
                row,
                format!("DELETE FROM {table} WHERE {}", condition(values)),
            );
        }
    }
    out.into_values().collect()
}

/// A typed value as it came back, for matching the row it came from.
pub fn literal(engine: Engine, value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::Bool(b) => match engine {
            Engine::Sqlite => u8::from(*b).to_string(),
            _ => b.to_string().to_ascii_uppercase(),
        },
        Value::Int(i) => i.to_string(),
        Value::Float(f) if f.is_finite() => f.to_string(),
        Value::Float(f) => text_literal(engine, &f.to_string()),
        Value::Number(n) if n.parse::<f64>().is_ok_and(f64::is_finite) => n.clone(),
        Value::Number(n) => text_literal(engine, n),
        Value::Text(s) | Value::Json(s) => text_literal(engine, s),
        Value::Bytes(b) => {
            let hex: String = b.iter().map(|b| format!("{b:02x}")).collect();
            match engine {
                Engine::Postgres => format!("'\\x{hex}'::bytea"),
                _ => format!("X'{hex}'"),
            }
        }
    }
}

/// A string literal. The server converts it to the column's type, so typed
/// numbers, booleans and dates work without knowing the type here.
pub fn text_literal(engine: Engine, text: &str) -> String {
    let escaped = text.replace('\'', "''");
    match engine {
        // MySQL also treats backslash as an escape inside strings.
        Engine::MySql => format!("'{}'", escaped.replace('\\', "\\\\")),
        _ => format!("'{escaped}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColumnInfo, ObjectKind};

    fn schema() -> Schema {
        let column = |name: &str, key: bool| ColumnInfo {
            name: name.into(),
            type_name: "text".into(),
            nullable: !key,
            primary_key: key,
        };
        Schema {
            objects: vec![
                Object {
                    namespace: Some("public".into()),
                    name: "users".into(),
                    kind: ObjectKind::Table,
                    columns: vec![
                        column("id", true),
                        column("name", false),
                        column("bio", false),
                    ],
                    indexes: Vec::new(),
                },
                Object {
                    namespace: Some("public".into()),
                    name: "logs".into(),
                    kind: ObjectKind::Table,
                    columns: vec![column("line", false)],
                    indexes: Vec::new(),
                },
            ],
            truncated: false,
        }
    }

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
    fn finds_the_table_a_result_can_be_written_to() {
        let s = schema();
        let target = edit_target(
            Engine::Postgres,
            &s,
            "select name, id from public.\"users\" where id > 3 order by id limit 10",
            &columns(&["name", "id"]),
        )
        .unwrap();
        assert_eq!(target.table, "users");
        assert_eq!(target.namespace.as_deref(), Some("public"));
        assert_eq!(target.key, [1]);
        let refuse = |q: &str, cols: &[&str]| {
            edit_target(Engine::Postgres, &s, q, &columns(cols)).unwrap_err()
        };
        assert!(refuse("select name from users", &["name"]).contains("key column id"));
        assert!(
            refuse("select id, upper(name) as n from users", &["id", "n"]).contains("not a column")
        );
        assert!(refuse("select u.id from users u join logs l on true", &["id"]).contains("joins"));
        assert!(refuse("select * from logs", &["line"]).contains("no primary key"));
        assert!(refuse("update users set name = 'x'", &[]).contains("SELECT"));
        assert!(
            edit_target(Engine::Redis, &s, "GET a", &columns(&["value"]))
                .unwrap_err()
                .contains("Postgres")
        );
    }

    #[test]
    fn statements_match_rows_by_key_and_quote_values() {
        let target = EditTarget {
            namespace: Some("public".into()),
            table: "users".into(),
            key: vec![0],
        };
        let cols = columns(&["id", "name", "bio"]);
        let rows = vec![
            vec![Value::Int(1), Value::Text("ada".into()), Value::Null],
            vec![Value::Int(2), Value::Text("bob".into()), Value::Null],
            vec![Value::Int(3), Value::Text("cy".into()), Value::Null],
        ];
        let mut changes = Changes::default();
        changes.cells.insert((0, 1), Some("Ada's".into()));
        changes.cells.insert((0, 2), None);
        changes.cells.insert((1, 1), Some("ignored".into()));
        changes.deleted.insert(1);
        changes.cells.insert((2, 2), Some("a\\b".into()));
        assert_eq!(changes.len(), 3);
        assert_eq!(
            statements(Engine::Postgres, &target, &cols, &rows, &changes),
            [
                "UPDATE \"users\" SET \"name\" = 'Ada''s', \"bio\" = NULL WHERE \"id\" = 1",
                "DELETE FROM \"users\" WHERE \"id\" = 2",
                "UPDATE \"users\" SET \"bio\" = 'a\\b' WHERE \"id\" = 3",
            ]
        );
        let mysql = statements(Engine::MySql, &target, &cols, &rows, &changes);
        assert_eq!(
            mysql[2],
            "UPDATE `public`.`users` SET `bio` = 'a\\\\b' WHERE `id` = 3"
        );
    }

    #[test]
    fn literals_per_engine() {
        assert_eq!(literal(Engine::Sqlite, &Value::Bool(true)), "1");
        assert_eq!(literal(Engine::Postgres, &Value::Bool(false)), "FALSE");
        assert_eq!(
            literal(Engine::Postgres, &Value::Number("12.50".into())),
            "12.50"
        );
        assert_eq!(
            literal(Engine::Postgres, &Value::Number("NaN".into())),
            "'NaN'"
        );
        assert_eq!(
            literal(Engine::Postgres, &Value::Bytes(vec![0, 255])),
            "'\\x00ff'::bytea"
        );
        assert_eq!(literal(Engine::MySql, &Value::Bytes(vec![1])), "X'01'");
    }
}
