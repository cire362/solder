//! Saving a structure change as a migration in the project, in the format
//! of the tool the project already uses, so the change ships with the code
//! instead of living only in one database. Touches the disk: run it on a
//! background thread.

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Prisma,
    Drizzle,
    Supabase,
    /// golang-migrate and others with `N_name.up.sql` / `.down.sql`.
    UpDown,
    /// `-- migrate:up` / `-- migrate:down` in one file.
    Dbmate,
    /// Timestamped `.sql` files and nothing else.
    Plain,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Prisma => "Prisma",
            Tool::Drizzle => "Drizzle",
            Tool::Supabase => "Supabase",
            Tool::UpDown => "up/down files",
            Tool::Dbmate => "dbmate",
            Tool::Plain => "SQL file",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub tool: Tool,
    pub dir: PathBuf,
}

/// Where this project keeps migrations. Without any, a `migrations` folder
/// at the root.
pub fn detect(root: &Path) -> Target {
    let found = |tool: Tool, dir: PathBuf| Some(Target { tool, dir });
    let sql_files = |dir: &Path| -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(".sql"))
                    .collect()
            })
            .unwrap_or_default()
    };
    let detected = (|| {
        if root.join("prisma/schema.prisma").exists() || root.join("prisma/migrations").is_dir() {
            return found(Tool::Prisma, root.join("prisma/migrations"));
        }
        if let Some(dir) = drizzle_out(root) {
            return found(Tool::Drizzle, dir);
        }
        if root.join("supabase/migrations").is_dir() {
            return found(Tool::Supabase, root.join("supabase/migrations"));
        }
        for dir in [
            "migrations",
            "db/migrations",
            "database/migrations",
            "sql/migrations",
        ] {
            let dir = root.join(dir);
            let files = sql_files(&dir);
            if files.iter().any(|f| f.ends_with(".up.sql")) {
                return found(Tool::UpDown, dir);
            }
            let dbmate = files.iter().any(|f| {
                std::fs::read_to_string(dir.join(f))
                    .is_ok_and(|text| text.contains("-- migrate:up"))
            });
            if dbmate {
                return found(Tool::Dbmate, dir);
            }
            if !files.is_empty() {
                return found(Tool::Plain, dir);
            }
        }
        None
    })();
    detected.unwrap_or(Target {
        tool: Tool::Plain,
        dir: root.join("migrations"),
    })
}

/// Drizzle's output folder: `out` in drizzle.config, else `drizzle` when it
/// has a journal.
fn drizzle_out(root: &Path) -> Option<PathBuf> {
    for config in [
        "drizzle.config.ts",
        "drizzle.config.js",
        "drizzle.config.mjs",
        "drizzle.config.json",
    ] {
        let Ok(text) = std::fs::read_to_string(root.join(config)) else {
            continue;
        };
        let out = text.split("out").nth(1).and_then(|rest| {
            let start = rest.find(['"', '\''])?;
            let quote = rest[start..].chars().next()?;
            let value = &rest[start + 1..];
            Some(value[..value.find(quote)?].to_string())
        });
        return Some(root.join(out.unwrap_or_else(|| "drizzle".into())));
    }
    root.join("drizzle/meta/_journal.json")
        .exists()
        .then(|| root.join("drizzle"))
}

/// Writes the migration and returns the files it created or changed.
pub fn write(
    target: &Target,
    name: &str,
    up: &[String],
    down: &[String],
) -> std::io::Result<Vec<PathBuf>> {
    write_at(target, name, up, down, SystemTime::now())
}

fn write_at(
    target: &Target,
    name: &str,
    up: &[String],
    down: &[String],
    now: SystemTime,
) -> std::io::Result<Vec<PathBuf>> {
    let slug = slug(name);
    let stamp = timestamp(now);
    let script = |statements: &[String]| -> String {
        statements
            .iter()
            .map(|s| format!("{s};\n"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    std::fs::create_dir_all(&target.dir)?;
    let file = |name: String| target.dir.join(name);
    let written = match target.tool {
        Tool::Prisma => {
            let dir = target.dir.join(format!("{stamp}_{slug}"));
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("migration.sql");
            std::fs::write(&path, script(up))?;
            vec![path]
        }
        Tool::Supabase => {
            let path = file(format!("{stamp}_{slug}.sql"));
            std::fs::write(&path, script(up))?;
            vec![path]
        }
        Tool::Plain => {
            let path = file(format!("{stamp}_{slug}.sql"));
            let undo: String = script(down).lines().map(|l| format!("-- {l}\n")).collect();
            std::fs::write(&path, format!("{}\n-- To undo:\n{undo}", script(up)))?;
            vec![path]
        }
        Tool::Dbmate => {
            let path = file(format!("{stamp}_{slug}.sql"));
            std::fs::write(
                &path,
                format!(
                    "-- migrate:up\n{}\n-- migrate:down\n{}",
                    script(up),
                    script(down)
                ),
            )?;
            vec![path]
        }
        Tool::UpDown => {
            let version = next_version(&target.dir, &stamp);
            let up_path = file(format!("{version}_{slug}.up.sql"));
            let down_path = file(format!("{version}_{slug}.down.sql"));
            std::fs::write(&up_path, script(up))?;
            std::fs::write(&down_path, script(down))?;
            vec![up_path, down_path]
        }
        Tool::Drizzle => {
            let journal_path = target.dir.join("meta/_journal.json");
            let mut journal: serde_json::Value = std::fs::read_to_string(&journal_path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_else(
                    || serde_json::json!({"version": "7", "dialect": "postgresql", "entries": []}),
                );
            let entries = journal["entries"].as_array().cloned().unwrap_or_default();
            let idx = entries.len();
            let tag = format!("{idx:04}_{slug}");
            let version = entries
                .last()
                .and_then(|e| e["version"].as_str())
                .unwrap_or("7")
                .to_string();
            let path = file(format!("{tag}.sql"));
            std::fs::write(&path, up.join(";\n--> statement-breakpoint\n") + ";\n")?;
            let when = now
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let mut entries = entries;
            entries.push(serde_json::json!({
                "idx": idx,
                "version": version,
                "when": when,
                "tag": tag,
                "breakpoints": true,
            }));
            journal["entries"] = serde_json::Value::Array(entries);
            std::fs::create_dir_all(target.dir.join("meta"))?;
            std::fs::write(
                &journal_path,
                serde_json::to_string_pretty(&journal)? + "\n",
            )?;
            vec![path, journal_path]
        }
    };
    Ok(written)
}

/// The next version in a folder of `N_name.up.sql` files: a timestamp if
/// they use timestamps, else the next number at the same width.
fn next_version(dir: &Path, stamp: &str) -> String {
    let numbers: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let digits: String = name.chars().take_while(char::is_ascii_digit).collect();
                    (!digits.is_empty()).then_some(digits)
                })
                .collect()
        })
        .unwrap_or_default();
    if numbers.is_empty() || numbers.iter().any(|n| n.len() >= 14) {
        return stamp.to_string();
    }
    let width = numbers.iter().map(String::len).max().unwrap_or(1);
    let next = numbers
        .iter()
        .filter_map(|n| n.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    format!("{next:0width$}")
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() { "change".into() } else { out }
}

/// `YYYYMMDDHHMMSS` in UTC.
fn timestamp(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // 2026-10-01 12:34:56 UTC.
    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_790_858_096)
    }

    fn up() -> Vec<String> {
        vec![
            "ALTER TABLE users ADD COLUMN email text".into(),
            "CREATE INDEX users_email ON users (email)".into(),
        ]
    }

    fn down() -> Vec<String> {
        vec![
            "DROP INDEX users_email".into(),
            "ALTER TABLE users DROP COLUMN email".into(),
        ]
    }

    #[test]
    fn detects_tools() {
        let root = crate::testing::dir("migrations-detect");
        assert_eq!(
            detect(&root),
            Target {
                tool: Tool::Plain,
                dir: root.join("migrations")
            }
        );
        std::fs::create_dir_all(root.join("db/migrations")).unwrap();
        std::fs::write(root.join("db/migrations/001_init.up.sql"), "").unwrap();
        assert_eq!(detect(&root).tool, Tool::UpDown);
        std::fs::write(
            root.join("drizzle.config.ts"),
            "export default { out: './db/drizzle', dialect: 'postgresql' }",
        )
        .unwrap();
        assert_eq!(
            detect(&root),
            Target {
                tool: Tool::Drizzle,
                dir: root.join("./db/drizzle")
            }
        );
        std::fs::create_dir_all(root.join("prisma")).unwrap();
        std::fs::write(root.join("prisma/schema.prisma"), "").unwrap();
        assert_eq!(
            detect(&root),
            Target {
                tool: Tool::Prisma,
                dir: root.join("prisma/migrations")
            }
        );
    }

    #[test]
    fn writes_each_format() {
        let root = crate::testing::dir("migrations-write");
        let target = |tool: Tool, dir: &str| Target {
            tool,
            dir: root.join(dir),
        };
        assert_eq!(timestamp(now()), "20261001123456");

        let files = write_at(
            &target(Tool::Prisma, "prisma/migrations"),
            "Add email!",
            &up(),
            &down(),
            now(),
        )
        .unwrap();
        assert_eq!(
            files,
            [root.join("prisma/migrations/20261001123456_add_email/migration.sql")]
        );
        assert_eq!(
            std::fs::read_to_string(&files[0]).unwrap(),
            "ALTER TABLE users ADD COLUMN email text;\n\nCREATE INDEX users_email ON users (email);\n"
        );

        let dir = root.join("migrations");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("0007_old.up.sql"), "").unwrap();
        let files = write_at(
            &target(Tool::UpDown, "migrations"),
            "add email",
            &up(),
            &down(),
            now(),
        )
        .unwrap();
        assert_eq!(
            files,
            [
                dir.join("0008_add_email.up.sql"),
                dir.join("0008_add_email.down.sql")
            ]
        );
        assert!(
            std::fs::read_to_string(&files[1])
                .unwrap()
                .starts_with("DROP INDEX users_email;")
        );

        let files = write_at(
            &target(Tool::Dbmate, "db/migrations"),
            "add email",
            &up(),
            &down(),
            now(),
        )
        .unwrap();
        let text = std::fs::read_to_string(&files[0]).unwrap();
        assert!(text.starts_with("-- migrate:up\nALTER TABLE users ADD COLUMN email text;"));
        assert!(text.contains("-- migrate:down\nDROP INDEX users_email;"));

        let files = write_at(
            &target(Tool::Plain, "sql"),
            "add email",
            &up(),
            &down(),
            now(),
        )
        .unwrap();
        assert!(
            std::fs::read_to_string(&files[0])
                .unwrap()
                .contains("-- To undo:\n-- DROP INDEX users_email;")
        );

        let drizzle = root.join("drizzle");
        std::fs::create_dir_all(drizzle.join("meta")).unwrap();
        std::fs::write(
            drizzle.join("meta/_journal.json"),
            r#"{"version": "7", "dialect": "postgresql", "entries": [{"idx": 0, "version": "7", "when": 1, "tag": "0000_init", "breakpoints": true}]}"#,
        )
        .unwrap();
        let files = write_at(
            &target(Tool::Drizzle, "drizzle"),
            "add email",
            &up(),
            &down(),
            now(),
        )
        .unwrap();
        assert_eq!(files[0], drizzle.join("0001_add_email.sql"));
        assert_eq!(
            std::fs::read_to_string(&files[0]).unwrap(),
            "ALTER TABLE users ADD COLUMN email text;\n--> statement-breakpoint\nCREATE INDEX users_email ON users (email);\n"
        );
        let journal: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&files[1]).unwrap()).unwrap();
        assert_eq!(journal["entries"][1]["tag"], "0001_add_email");
        assert_eq!(journal["entries"][1]["idx"], 1);
    }
}
