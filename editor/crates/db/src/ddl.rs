//! Changing a table's structure: a draft of the table as it should be, and
//! the statements that take the database from how it is to the draft. The
//! reverse change comes from the same comparison, so migrations get a down
//! step for free.

use std::collections::BTreeSet;

use crate::{Engine, ForeignKey, Index, Object, sql};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnDraft {
    /// The column's current name; `None` for a new column.
    pub original: Option<String>,
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    /// A default expression as SQL (`'draft'`, `now()`).
    pub default: Option<String>,
    pub primary_key: bool,
    /// Filled in by the server. Kept as is: MySQL needs it restated when a
    /// column is modified, and SQLite keeps an INTEGER PRIMARY KEY as rowid.
    pub auto: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexDraft {
    pub original: Option<String>,
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForeignKeyDraft {
    pub original: Option<String>,
    pub name: String,
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: Option<String>,
}

/// A table as it should be.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableDraft {
    pub namespace: Option<String>,
    pub name: String,
    pub columns: Vec<ColumnDraft>,
    pub indexes: Vec<IndexDraft>,
    pub foreign_keys: Vec<ForeignKeyDraft>,
    /// The primary key constraint's name, for changing the key (Postgres).
    pub primary_key_name: Option<String>,
}

impl TableDraft {
    /// The table as it is: every part remembers its current name.
    pub fn of(object: &Object) -> Self {
        TableDraft {
            namespace: object.namespace.clone(),
            name: object.name.clone(),
            columns: object
                .columns
                .iter()
                .map(|c| ColumnDraft {
                    original: Some(c.name.clone()),
                    name: c.name.clone(),
                    type_name: c.type_name.clone(),
                    nullable: c.nullable,
                    default: c.default.clone(),
                    primary_key: c.primary_key,
                    auto: c.auto,
                })
                .collect(),
            // The primary key's index follows the columns marked as key.
            indexes: object
                .indexes
                .iter()
                .filter(|i| !i.primary)
                .map(|i: &Index| IndexDraft {
                    original: Some(i.name.clone()),
                    name: i.name.clone(),
                    columns: i.columns.clone(),
                    unique: i.unique,
                })
                .collect(),
            foreign_keys: object
                .foreign_keys
                .iter()
                .map(|fk: &ForeignKey| ForeignKeyDraft {
                    original: Some(fk.name.clone()),
                    name: fk.name.clone(),
                    columns: fk.columns.clone(),
                    ref_table: fk.ref_table.clone(),
                    ref_columns: fk.ref_columns.clone(),
                    on_delete: fk.on_delete.clone(),
                })
                .collect(),
            primary_key_name: object.primary_key_name.clone(),
        }
    }

    fn key(&self) -> Vec<&str> {
        self.columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| c.name.as_str())
            .collect()
    }

    /// Problems that would make the statements fail or mean something
    /// other than intended.
    pub fn check(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("The table needs a name".into());
        }
        if self.columns.is_empty() {
            return Err("A table needs at least one column".into());
        }
        let mut names = BTreeSet::new();
        for c in &self.columns {
            if c.name.trim().is_empty() {
                return Err("Every column needs a name".into());
            }
            if c.type_name.trim().is_empty() {
                return Err(format!("{} needs a type", c.name));
            }
            if !names.insert(c.name.to_ascii_lowercase()) {
                return Err(format!("{} appears twice", c.name));
            }
        }
        let known = |columns: &[String], what: &str| -> Result<(), String> {
            if columns.is_empty() {
                return Err(format!("{what} needs at least one column"));
            }
            match columns
                .iter()
                .find(|c| !names.contains(&c.to_ascii_lowercase()))
            {
                Some(c) => Err(format!("{what} uses {c}, which is not a column")),
                None => Ok(()),
            }
        };
        for i in &self.indexes {
            if i.name.trim().is_empty() {
                return Err("Every index needs a name".into());
            }
            known(&i.columns, &format!("Index {}", i.name))?;
        }
        for fk in &self.foreign_keys {
            if fk.name.trim().is_empty() {
                return Err("Every foreign key needs a name".into());
            }
            known(&fk.columns, &format!("Foreign key {}", fk.name))?;
            if fk.ref_table.trim().is_empty() {
                return Err(format!("Foreign key {} needs a table to point at", fk.name));
            }
            if fk.ref_columns.len() != fk.columns.len() {
                return Err(format!(
                    "Foreign key {} has {} columns but points at {}",
                    fk.name,
                    fk.columns.len(),
                    fk.ref_columns.len()
                ));
            }
        }
        Ok(())
    }
}

/// What to do to one table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Create(TableDraft),
    /// `before` is the table as it is (every `original` is its own name);
    /// in `after`, `original` names the part of `before` it came from.
    Alter {
        before: TableDraft,
        after: TableDraft,
    },
    Drop(TableDraft),
}

impl Change {
    /// The change that undoes this one: the down step of a migration.
    pub fn inverse(&self) -> Change {
        match self {
            Change::Create(t) => Change::Drop(t.clone()),
            Change::Drop(t) => Change::Create(t.clone()),
            Change::Alter { before, after } => {
                let mut from = after.clone();
                for c in &mut from.columns {
                    c.original = Some(c.name.clone());
                }
                for i in &mut from.indexes {
                    i.original = Some(i.name.clone());
                }
                for fk in &mut from.foreign_keys {
                    fk.original = Some(fk.name.clone());
                }
                let mut to = before.clone();
                for c in &mut to.columns {
                    c.original = after
                        .columns
                        .iter()
                        .find(|a| a.original.as_deref() == Some(c.name.as_str()))
                        .map(|a| a.name.clone());
                }
                for i in &mut to.indexes {
                    i.original = after
                        .indexes
                        .iter()
                        .find(|a| a.original.as_deref() == Some(i.name.as_str()))
                        .map(|a| a.name.clone());
                }
                for fk in &mut to.foreign_keys {
                    fk.original = after
                        .foreign_keys
                        .iter()
                        .find(|a| a.original.as_deref() == Some(fk.name.as_str()))
                        .map(|a| a.name.clone());
                }
                Change::Alter {
                    before: from,
                    after: to,
                }
            }
        }
    }
}

/// The statements that make the change, in order.
pub fn statements(engine: Engine, change: &Change) -> Result<Vec<String>, String> {
    if !engine.is_sql() {
        return Err("Structure can be changed for Postgres, MySQL and SQLite".into());
    }
    match change {
        Change::Create(t) => {
            t.check()?;
            Ok(create(engine, t, &t.name))
        }
        Change::Drop(t) => Ok(vec![format!("DROP TABLE {}", table(engine, t, &t.name))]),
        Change::Alter { before, after } => {
            after.check()?;
            Ok(match engine {
                Engine::Sqlite => alter_sqlite(before, after),
                _ => alter(engine, before, after),
            })
        }
    }
}

fn q(engine: Engine, name: &str) -> String {
    sql::quote_ident(engine, name)
}

fn list(engine: Engine, names: &[String]) -> String {
    names
        .iter()
        .map(|n| q(engine, n))
        .collect::<Vec<_>>()
        .join(", ")
}

fn table(engine: Engine, t: &TableDraft, name: &str) -> String {
    sql::qualified_name(engine, t.namespace.as_deref(), name)
}

fn column(engine: Engine, c: &ColumnDraft, creating: bool) -> String {
    let mut type_name = c.type_name.trim().to_string();
    let mut default = c.default.clone();
    let mut identity = false;
    // A Postgres table made again (undoing a drop) cannot reuse the sequence
    // its serial column had; it gets a new one.
    if engine == Engine::Postgres && creating && c.auto {
        if default
            .as_deref()
            .is_some_and(|d| d.starts_with("nextval("))
        {
            type_name = match type_name.as_str() {
                "bigint" => "bigserial".into(),
                "smallint" => "smallserial".into(),
                _ => "serial".into(),
            };
            default = None;
        } else if !type_name.contains("serial") {
            identity = true;
        }
    }
    let mut out = format!("{} {type_name}", q(engine, &c.name));
    if identity {
        out.push_str(" GENERATED BY DEFAULT AS IDENTITY");
    }
    if !c.nullable {
        out.push_str(" NOT NULL");
    }
    if let Some(d) = default.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        out.push_str(&format!(" DEFAULT {d}"));
    }
    if engine == Engine::MySql && c.auto {
        out.push_str(" AUTO_INCREMENT");
    }
    out
}

fn foreign_key(engine: Engine, t: &TableDraft, fk: &ForeignKeyDraft) -> String {
    let mut out = format!(
        "CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
        q(engine, &fk.name),
        list(engine, &fk.columns),
        table(engine, t, &fk.ref_table),
        list(engine, &fk.ref_columns)
    );
    if let Some(action) = fk.on_delete.as_deref().filter(|a| !a.is_empty()) {
        out.push_str(&format!(" ON DELETE {action}"));
    }
    out
}

fn create_index(engine: Engine, t: &TableDraft, name: &str, i: &IndexDraft) -> String {
    format!(
        "CREATE {}INDEX {} ON {} ({})",
        if i.unique { "UNIQUE " } else { "" },
        q(engine, &i.name),
        table(engine, t, name),
        list(engine, &i.columns)
    )
}

fn drop_index(engine: Engine, t: &TableDraft, name: &str, index: &str) -> String {
    match engine {
        Engine::MySql => format!(
            "DROP INDEX {} ON {}",
            q(engine, index),
            table(engine, t, name)
        ),
        // Postgres indexes live in the table's schema.
        _ => format!(
            "DROP INDEX {}",
            sql::qualified_name(engine, t.namespace.as_deref(), index)
        ),
    }
}

/// `CREATE TABLE` named `name`, then its indexes.
fn create(engine: Engine, t: &TableDraft, name: &str) -> Vec<String> {
    let mut parts: Vec<String> = t.columns.iter().map(|c| column(engine, c, true)).collect();
    let key = t.key();
    if !key.is_empty() {
        let key: Vec<String> = key.iter().map(|k| k.to_string()).collect();
        parts.push(format!("PRIMARY KEY ({})", list(engine, &key)));
    }
    parts.extend(t.foreign_keys.iter().map(|fk| foreign_key(engine, t, fk)));
    let mut out = vec![format!(
        "CREATE TABLE {} (\n  {}\n)",
        table(engine, t, name),
        parts.join(",\n  ")
    )];
    out.extend(t.indexes.iter().map(|i| create_index(engine, t, name, i)));
    out
}

/// A key column is never NULL whatever the flag says (SQLite reports its
/// rowid key as nullable), so only other columns can change it.
fn nullability_changed(b: &ColumnDraft, a: &ColumnDraft) -> bool {
    a.nullable != b.nullable && !(a.primary_key && b.primary_key)
}

fn same_type(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn same_default(a: &Option<String>, b: &Option<String>) -> bool {
    let norm = |d: &Option<String>| {
        d.as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string)
    };
    norm(a) == norm(b)
}

/// The parts of `after` that came from `before`, and those that did not.
fn pair<'a, T>(
    before: &'a [T],
    after: &'a [T],
    name: impl Fn(&T) -> &str,
    original: impl Fn(&T) -> Option<&str>,
) -> (Vec<(&'a T, &'a T)>, Vec<&'a T>, Vec<&'a T>) {
    let kept: Vec<(&T, &T)> = after
        .iter()
        .filter_map(|a| {
            let o = original(a)?;
            before.iter().find(|b| name(b) == o).map(|b| (b, a))
        })
        .collect();
    let added = after
        .iter()
        .filter(|a| !kept.iter().any(|(_, k)| std::ptr::eq(*k, *a)))
        .collect();
    let removed = before
        .iter()
        .filter(|b| !kept.iter().any(|(k, _)| std::ptr::eq(*k, *b)))
        .collect();
    (kept, added, removed)
}

/// `columns` of `after` under their names in `before`: renaming a column
/// carries its indexes and keys along, so that alone changes nothing.
fn as_before(after: &TableDraft, columns: &[String]) -> Vec<String> {
    columns
        .iter()
        .map(|c| {
            after
                .columns
                .iter()
                .find(|a| a.name == *c)
                .and_then(|a| a.original.clone())
                .unwrap_or_else(|| format!("new:{c}"))
        })
        .collect()
}

fn index_changed(after_table: &TableDraft, b: &IndexDraft, a: &IndexDraft) -> bool {
    a.name != b.name || a.unique != b.unique || as_before(after_table, &a.columns) != b.columns
}

fn foreign_key_changed(after_table: &TableDraft, b: &ForeignKeyDraft, a: &ForeignKeyDraft) -> bool {
    a.name != b.name
        || as_before(after_table, &a.columns) != b.columns
        || a.ref_table != b.ref_table
        || a.ref_columns != b.ref_columns
        || a.on_delete != b.on_delete
}

/// The key as names in `before`, to tell whether it changed.
fn key_in_before(before: &TableDraft, after: &TableDraft) -> (Vec<String>, Vec<String>) {
    let old: Vec<String> = before.key().iter().map(|k| k.to_string()).collect();
    let new: Vec<String> = after
        .columns
        .iter()
        .filter(|c| c.primary_key)
        .map(|c| {
            c.original
                .clone()
                .unwrap_or_else(|| format!("new:{}", c.name))
        })
        .collect();
    (old, new)
}

fn alter(engine: Engine, before: &TableDraft, after: &TableDraft) -> Vec<String> {
    let mut out = Vec::new();
    let mut name = before.name.clone();
    if after.name != before.name {
        out.push(match engine {
            Engine::MySql => format!(
                "RENAME TABLE {} TO {}",
                table(engine, before, &name),
                table(engine, after, &after.name)
            ),
            _ => format!(
                "ALTER TABLE {} RENAME TO {}",
                table(engine, before, &name),
                q(engine, &after.name)
            ),
        });
        name = after.name.clone();
    }
    let t = table(engine, after, &name);
    let (fk_kept, fk_added, fk_removed) = pair(
        &before.foreign_keys,
        &after.foreign_keys,
        |f| &f.name,
        |f| f.original.as_deref(),
    );
    let fk_changed: Vec<&ForeignKeyDraft> = fk_kept
        .iter()
        .filter(|(b, a)| foreign_key_changed(after, b, a))
        .map(|(_, a)| *a)
        .collect();
    let drop_fk = |fk: &str| match engine {
        Engine::MySql => format!("ALTER TABLE {t} DROP FOREIGN KEY {}", q(engine, fk)),
        _ => format!("ALTER TABLE {t} DROP CONSTRAINT {}", q(engine, fk)),
    };
    for fk in &fk_removed {
        out.push(drop_fk(&fk.name));
    }
    for fk in &fk_changed {
        out.push(drop_fk(fk.original.as_deref().unwrap_or(&fk.name)));
    }
    let (ix_kept, ix_added, ix_removed) = pair(
        &before.indexes,
        &after.indexes,
        |i| &i.name,
        |i| i.original.as_deref(),
    );
    let ix_changed: Vec<&IndexDraft> = ix_kept
        .iter()
        .filter(|(b, a)| index_changed(after, b, a))
        .map(|(_, a)| *a)
        .collect();
    for i in &ix_removed {
        out.push(drop_index(engine, after, &name, &i.name));
    }
    for i in &ix_changed {
        out.push(drop_index(
            engine,
            after,
            &name,
            i.original.as_deref().unwrap_or(&i.name),
        ));
    }
    let (old_key, new_key) = key_in_before(before, after);
    let key_changed = old_key != new_key;
    if key_changed && !old_key.is_empty() {
        out.push(match engine {
            Engine::MySql => format!("ALTER TABLE {t} DROP PRIMARY KEY"),
            _ => format!(
                "ALTER TABLE {t} DROP CONSTRAINT {}",
                q(
                    engine,
                    &before
                        .primary_key_name
                        .clone()
                        .unwrap_or_else(|| format!("{}_pkey", before.name))
                )
            ),
        });
    }
    let (kept, added, removed) = pair(
        &before.columns,
        &after.columns,
        |c| &c.name,
        |c| c.original.as_deref(),
    );
    for c in &removed {
        out.push(format!(
            "ALTER TABLE {t} DROP COLUMN {}",
            q(engine, &c.name)
        ));
    }
    for (b, a) in &kept {
        match engine {
            Engine::MySql => {
                let changed = a.name != b.name
                    || !same_type(&a.type_name, &b.type_name)
                    || nullability_changed(b, a)
                    || !same_default(&a.default, &b.default);
                if changed {
                    out.push(format!(
                        "ALTER TABLE {t} CHANGE COLUMN {} {}",
                        q(engine, &b.name),
                        column(engine, a, false)
                    ));
                }
            }
            _ => {
                let col = q(engine, &a.name);
                if a.name != b.name {
                    out.push(format!(
                        "ALTER TABLE {t} RENAME COLUMN {} TO {col}",
                        q(engine, &b.name)
                    ));
                }
                if !same_type(&a.type_name, &b.type_name) {
                    let ty = a.type_name.trim();
                    out.push(format!(
                        "ALTER TABLE {t} ALTER COLUMN {col} TYPE {ty} USING {col}::{ty}"
                    ));
                }
                if nullability_changed(b, a) {
                    out.push(format!(
                        "ALTER TABLE {t} ALTER COLUMN {col} {} NOT NULL",
                        if a.nullable { "DROP" } else { "SET" }
                    ));
                }
                if !same_default(&a.default, &b.default) {
                    out.push(
                        match a
                            .default
                            .as_deref()
                            .map(str::trim)
                            .filter(|d| !d.is_empty())
                        {
                            Some(d) => {
                                format!("ALTER TABLE {t} ALTER COLUMN {col} SET DEFAULT {d}")
                            }
                            None => format!("ALTER TABLE {t} ALTER COLUMN {col} DROP DEFAULT"),
                        },
                    );
                }
            }
        }
    }
    for c in &added {
        out.push(format!(
            "ALTER TABLE {t} ADD COLUMN {}",
            column(engine, c, false)
        ));
    }
    if key_changed {
        let key: Vec<String> = after.key().iter().map(|k| k.to_string()).collect();
        if !key.is_empty() {
            out.push(format!(
                "ALTER TABLE {t} ADD PRIMARY KEY ({})",
                list(engine, &key)
            ));
        }
    }
    for i in ix_added.iter().chain(&ix_changed) {
        out.push(create_index(engine, after, &name, i));
    }
    for fk in fk_added.iter().chain(&fk_changed) {
        out.push(format!(
            "ALTER TABLE {t} ADD {}",
            foreign_key(engine, after, fk)
        ));
    }
    out
}

/// SQLite changes names and adds or drops plain columns in place; anything
/// else rebuilds the table: create the new shape, copy the rows, swap.
fn alter_sqlite(before: &TableDraft, after: &TableDraft) -> Vec<String> {
    let engine = Engine::Sqlite;
    let (kept, added, removed) = pair(
        &before.columns,
        &after.columns,
        |c| &c.name,
        |c| c.original.as_deref(),
    );
    let (old_key, new_key) = key_in_before(before, after);
    let (fk_kept, fk_added, fk_removed) = pair(
        &before.foreign_keys,
        &after.foreign_keys,
        |f| &f.name,
        |f| f.original.as_deref(),
    );
    let fks_changed = !fk_added.is_empty()
        || !fk_removed.is_empty()
        || fk_kept
            .iter()
            .any(|(b, a)| foreign_key_changed(after, b, a));
    let used = |name: &str| {
        before
            .indexes
            .iter()
            .any(|i| i.columns.iter().any(|c| c == name))
            || before
                .foreign_keys
                .iter()
                .any(|f| f.columns.iter().any(|c| c == name))
    };
    let in_place = old_key == new_key
        && !fks_changed
        && kept.iter().all(|(b, a)| {
            same_type(&a.type_name, &b.type_name)
                && !nullability_changed(b, a)
                && same_default(&a.default, &b.default)
        })
        && added
            .iter()
            .all(|c| !c.primary_key && (c.nullable || c.default.is_some()))
        && removed.iter().all(|c| !c.primary_key && !used(&c.name));
    if !in_place {
        return rebuild(before, after, &kept);
    }
    let mut out = Vec::new();
    let t = table(engine, after, &after.name);
    if after.name != before.name {
        out.push(format!(
            "ALTER TABLE {} RENAME TO {}",
            table(engine, before, &before.name),
            q(engine, &after.name)
        ));
    }
    let (ix_kept, ix_added, ix_removed) = pair(
        &before.indexes,
        &after.indexes,
        |i| &i.name,
        |i| i.original.as_deref(),
    );
    let ix_changed: Vec<&IndexDraft> = ix_kept
        .iter()
        .filter(|(b, a)| index_changed(after, b, a))
        .map(|(_, a)| *a)
        .collect();
    for i in &ix_removed {
        out.push(drop_index(engine, after, &after.name, &i.name));
    }
    for i in &ix_changed {
        out.push(drop_index(
            engine,
            after,
            &after.name,
            i.original.as_deref().unwrap_or(&i.name),
        ));
    }
    for (b, a) in &kept {
        if a.name != b.name {
            out.push(format!(
                "ALTER TABLE {t} RENAME COLUMN {} TO {}",
                q(engine, &b.name),
                q(engine, &a.name)
            ));
        }
    }
    for c in &removed {
        out.push(format!(
            "ALTER TABLE {t} DROP COLUMN {}",
            q(engine, &c.name)
        ));
    }
    for c in &added {
        out.push(format!(
            "ALTER TABLE {t} ADD COLUMN {}",
            column(engine, c, false)
        ));
    }
    for i in ix_added.iter().chain(&ix_changed) {
        out.push(create_index(engine, after, &after.name, i));
    }
    out
}

fn rebuild(
    before: &TableDraft,
    after: &TableDraft,
    kept: &[(&ColumnDraft, &ColumnDraft)],
) -> Vec<String> {
    let engine = Engine::Sqlite;
    let temp = format!("{}__solder_new", after.name);
    let mut out = create(
        engine,
        &TableDraft {
            indexes: Vec::new(),
            ..after.clone()
        },
        &temp,
    );
    let targets: Vec<String> = kept.iter().map(|(_, a)| a.name.clone()).collect();
    let sources: Vec<String> = kept.iter().map(|(b, _)| b.name.clone()).collect();
    if !kept.is_empty() {
        out.push(format!(
            "INSERT INTO {} ({}) SELECT {} FROM {}",
            q(engine, &temp),
            list(engine, &targets),
            list(engine, &sources),
            table(engine, before, &before.name)
        ));
    }
    out.push(format!(
        "DROP TABLE {}",
        table(engine, before, &before.name)
    ));
    out.push(format!(
        "ALTER TABLE {} RENAME TO {}",
        q(engine, &temp),
        q(engine, &after.name)
    ));
    out.extend(
        after
            .indexes
            .iter()
            .map(|i| create_index(engine, after, &after.name, i)),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn users() -> TableDraft {
        let col = |name: &str, ty: &str, pk: bool| ColumnDraft {
            original: Some(name.into()),
            name: name.into(),
            type_name: ty.into(),
            nullable: !pk,
            primary_key: pk,
            ..Default::default()
        };
        TableDraft {
            namespace: Some("public".into()),
            name: "users".into(),
            columns: vec![
                col("id", "integer", true),
                col("name", "text", false),
                col("bio", "text", false),
            ],
            indexes: vec![IndexDraft {
                original: Some("users_name".into()),
                name: "users_name".into(),
                columns: vec!["name".into()],
                unique: false,
            }],
            foreign_keys: Vec::new(),
            primary_key_name: Some("users_pkey".into()),
        }
    }

    /// Rename name to full_name and make it required, change bio's type,
    /// drop nothing, add an email column with a unique index and a
    /// reference to teams.
    fn edited() -> TableDraft {
        let mut t = users();
        t.columns[1].name = "full_name".into();
        t.columns[1].nullable = false;
        t.columns[2].type_name = "varchar(200)".into();
        t.columns.push(ColumnDraft {
            name: "team_id".into(),
            type_name: "integer".into(),
            nullable: true,
            ..Default::default()
        });
        t.indexes[0].columns = vec!["full_name".into()];
        t.indexes.push(IndexDraft {
            name: "users_team".into(),
            columns: vec!["team_id".into()],
            unique: false,
            original: None,
        });
        t.foreign_keys.push(ForeignKeyDraft {
            name: "users_team_fk".into(),
            columns: vec!["team_id".into()],
            ref_table: "teams".into(),
            ref_columns: vec!["id".into()],
            on_delete: Some("SET NULL".into()),
            original: None,
        });
        t
    }

    #[test]
    fn postgres_alter_and_its_inverse() {
        let change = Change::Alter {
            before: users(),
            after: edited(),
        };
        assert_eq!(
            statements(Engine::Postgres, &change).unwrap(),
            [
                "ALTER TABLE \"users\" RENAME COLUMN \"name\" TO \"full_name\"",
                "ALTER TABLE \"users\" ALTER COLUMN \"full_name\" SET NOT NULL",
                "ALTER TABLE \"users\" ALTER COLUMN \"bio\" TYPE varchar(200) USING \"bio\"::varchar(200)",
                "ALTER TABLE \"users\" ADD COLUMN \"team_id\" integer",
                "CREATE INDEX \"users_team\" ON \"users\" (\"team_id\")",
                "ALTER TABLE \"users\" ADD CONSTRAINT \"users_team_fk\" FOREIGN KEY (\"team_id\") REFERENCES \"teams\" (\"id\") ON DELETE SET NULL",
            ]
        );
        assert_eq!(
            statements(Engine::Postgres, &change.inverse()).unwrap(),
            [
                "ALTER TABLE \"users\" DROP CONSTRAINT \"users_team_fk\"",
                "DROP INDEX \"users_team\"",
                "ALTER TABLE \"users\" DROP COLUMN \"team_id\"",
                "ALTER TABLE \"users\" RENAME COLUMN \"full_name\" TO \"name\"",
                "ALTER TABLE \"users\" ALTER COLUMN \"name\" DROP NOT NULL",
                "ALTER TABLE \"users\" ALTER COLUMN \"bio\" TYPE text USING \"bio\"::text",
            ]
        );
    }

    #[test]
    fn mysql_restates_columns_and_keeps_auto_increment() {
        let mut before = users();
        before.columns[0].auto = true;
        let mut after = before.clone();
        after.columns[0].type_name = "bigint".into();
        after.columns[1].name = "full_name".into();
        after.columns[1].default = Some("'anon'".into());
        after.indexes[0].columns = vec!["full_name".into()];
        assert_eq!(
            statements(Engine::MySql, &Change::Alter { before, after }).unwrap(),
            [
                "ALTER TABLE `public`.`users` CHANGE COLUMN `id` `id` bigint NOT NULL AUTO_INCREMENT",
                "ALTER TABLE `public`.`users` CHANGE COLUMN `name` `full_name` text DEFAULT 'anon'",
            ]
        );
    }

    #[test]
    fn primary_key_changes() {
        let mut after = users();
        after.columns[0].primary_key = false;
        after.columns[1].primary_key = true;
        let change = Change::Alter {
            before: users(),
            after,
        };
        let pg = statements(Engine::Postgres, &change).unwrap();
        assert_eq!(
            pg[0],
            "ALTER TABLE \"users\" DROP CONSTRAINT \"users_pkey\""
        );
        assert_eq!(pg[1], "ALTER TABLE \"users\" ADD PRIMARY KEY (\"name\")");
        let my = statements(Engine::MySql, &change).unwrap();
        assert_eq!(my[0], "ALTER TABLE `public`.`users` DROP PRIMARY KEY");
    }

    #[test]
    fn sqlite_alters_in_place_or_rebuilds() {
        let mut t = users();
        t.namespace = None;
        let mut renamed = t.clone();
        renamed.columns[1].name = "full_name".into();
        renamed.indexes[0].columns = vec!["full_name".into()];
        renamed.columns.push(ColumnDraft {
            name: "email".into(),
            type_name: "text".into(),
            nullable: true,
            ..Default::default()
        });
        assert_eq!(
            statements(
                Engine::Sqlite,
                &Change::Alter {
                    before: t.clone(),
                    after: renamed
                }
            )
            .unwrap(),
            [
                "ALTER TABLE \"users\" RENAME COLUMN \"name\" TO \"full_name\"",
                "ALTER TABLE \"users\" ADD COLUMN \"email\" text",
            ]
        );
        let mut retyped = t.clone();
        retyped.columns[2].type_name = "blob".into();
        retyped.columns[1].nullable = false;
        assert_eq!(
            statements(
                Engine::Sqlite,
                &Change::Alter {
                    before: t,
                    after: retyped
                }
            )
            .unwrap(),
            [
                "CREATE TABLE \"users__solder_new\" (\n  \"id\" integer NOT NULL,\n  \"name\" text NOT NULL,\n  \"bio\" blob,\n  PRIMARY KEY (\"id\")\n)",
                "INSERT INTO \"users__solder_new\" (\"id\", \"name\", \"bio\") SELECT \"id\", \"name\", \"bio\" FROM \"users\"",
                "DROP TABLE \"users\"",
                "ALTER TABLE \"users__solder_new\" RENAME TO \"users\"",
                "CREATE INDEX \"users_name\" ON \"users\" (\"name\")",
            ]
        );
    }

    #[test]
    fn create_drop_and_checks() {
        let mut t = users();
        t.namespace = None;
        t.columns[0].auto = true;
        t.columns[0].default = Some("nextval('users_id_seq'::regclass)".into());
        let created = statements(Engine::Postgres, &Change::Create(t.clone())).unwrap();
        assert_eq!(
            created[0],
            "CREATE TABLE \"users\" (\n  \"id\" serial NOT NULL,\n  \"name\" text,\n  \"bio\" text,\n  PRIMARY KEY (\"id\")\n)"
        );
        assert_eq!(
            created[1],
            "CREATE INDEX \"users_name\" ON \"users\" (\"name\")"
        );
        assert_eq!(
            statements(Engine::Postgres, &Change::Create(t.clone()).inverse()).unwrap(),
            ["DROP TABLE \"users\""]
        );
        let mut bad = t.clone();
        bad.indexes[0].columns = vec!["nope".into()];
        assert!(
            statements(Engine::Postgres, &Change::Create(bad))
                .unwrap_err()
                .contains("nope")
        );
        let mut bad = t;
        bad.columns[2].name = "NAME".into();
        assert!(
            statements(Engine::Sqlite, &Change::Create(bad))
                .unwrap_err()
                .contains("twice")
        );
    }
}
