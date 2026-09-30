//! Drivers against real servers. Each test runs when its URL is set and is
//! skipped otherwise; CI starts all four as service containers:
//!
//! SOLDER_TEST_POSTGRES=postgres://postgres:solder@localhost:55432/solder
//! SOLDER_TEST_MYSQL=mysql://root:solder@localhost:53306/solder
//! SOLDER_TEST_REDIS=redis://localhost:56379
//! SOLDER_TEST_MONGO=mongodb://localhost:57017/solder

use db::{ConnectionSpec, Engine, ObjectKind, Session, Value};

fn spec(var: &str, engine: Engine, read_only: bool) -> Option<ConnectionSpec> {
    let url = std::env::var(var).ok().filter(|u| !u.is_empty());
    if url.is_none() {
        eprintln!("skipped: {var} is not set");
    }
    Some(ConnectionSpec {
        name: var.into(),
        engine,
        url: url?,
        source: "test".into(),
        read_only,
    })
}

fn block<T>(f: impl std::future::Future<Output = T>) -> T {
    futures::executor::block_on(f)
}

fn run(session: &Session, query: &str) -> db::QueryResult {
    block(session.query(query.into())).unwrap_or_else(|e| panic!("{query}: {e}"))
}

#[test]
fn postgres() {
    let Some(spec) = spec("SOLDER_TEST_POSTGRES", Engine::Postgres, false) else {
        return;
    };
    let session = block(Session::connect(spec.clone())).unwrap();
    run(&session, "DROP TABLE IF EXISTS solder_users");
    run(
        &session,
        "CREATE TABLE solder_users (id serial PRIMARY KEY, name text NOT NULL, active boolean, \
         score numeric(5,2), meta jsonb, avatar bytea, seen timestamptz)",
    );
    run(
        &session,
        "CREATE INDEX solder_users_name ON solder_users (name)",
    );
    let insert = run(
        &session,
        "INSERT INTO solder_users (name, active, score, meta, avatar) VALUES \
         ('ada', true, 9.50, '{\"a\": 1}', '\\x00ff'), ('bob', false, NULL, NULL, NULL)",
    );
    assert_eq!(insert.affected, Some(2));

    let result = run(
        &session,
        "SELECT id, name, active, score, meta, avatar, seen FROM solder_users ORDER BY id",
    );
    let types: Vec<_> = result
        .columns
        .iter()
        .map(|c| c.type_name.as_str())
        .collect();
    assert_eq!(
        types,
        [
            "int4",
            "text",
            "bool",
            "numeric",
            "jsonb",
            "bytea",
            "timestamptz"
        ]
    );
    assert_eq!(
        result.rows[0][..6],
        [
            Value::Int(1),
            Value::Text("ada".into()),
            Value::Bool(true),
            Value::Number("9.50".into()),
            Value::Json("{\"a\": 1}".into()),
            Value::Bytes(vec![0, 255]),
        ]
    );
    assert_eq!(result.rows[1][3], Value::Null);

    let err = block(session.query("SELECT nope FROM solder_users".into())).unwrap_err();
    assert!(err.contains("nope"), "{err}");
    // The connection survives an error.
    assert_eq!(run(&session, "SELECT 1").rows[0][0], Value::Int(1));

    let schema = block(session.schema()).unwrap();
    let users = schema
        .objects
        .iter()
        .find(|o| o.name == "solder_users")
        .unwrap();
    assert_eq!(users.namespace.as_deref(), Some("public"));
    assert!(users.columns[0].primary_key && !users.columns[1].nullable);
    assert!(users.indexes.contains(&"solder_users_name".to_string()));
    assert_eq!(run(&session, &session.preview_query(users)).rows.len(), 2);

    let ro = block(Session::connect(ConnectionSpec {
        read_only: true,
        ..spec
    }))
    .unwrap();
    let err = block(ro.query("DELETE FROM solder_users".into())).unwrap_err();
    assert!(err.contains("read-only"), "{err}");
    assert_eq!(
        run(&ro, "SELECT count(*) FROM solder_users").rows[0][0],
        Value::Int(2)
    );
    run(&session, "DROP TABLE solder_users");
}

#[test]
fn mysql() {
    let Some(spec) = spec("SOLDER_TEST_MYSQL", Engine::MySql, false) else {
        return;
    };
    let session = block(Session::connect(spec.clone())).unwrap();
    run(&session, "DROP TABLE IF EXISTS solder_orders");
    run(
        &session,
        "CREATE TABLE solder_orders (id INT AUTO_INCREMENT PRIMARY KEY, customer VARCHAR(40) NOT NULL, \
         total DECIMAL(8,2), paid TINYINT(1), meta JSON, big BIGINT UNSIGNED, blob_col BLOB, \
         created DATETIME, INDEX by_customer (customer))",
    );
    let insert = run(
        &session,
        "INSERT INTO solder_orders (customer, total, paid, meta, big, blob_col, created) VALUES \
         ('ada', 12.50, 1, '{\"a\": 1}', 18446744073709551615, x'00ff', '2024-01-02 03:04:05'), \
         ('bob', NULL, 0, NULL, 1, NULL, NULL)",
    );
    assert_eq!(insert.affected, Some(2));
    let result = run(&session, "SELECT * FROM solder_orders ORDER BY id");
    assert_eq!(
        result.rows[0],
        vec![
            Value::Int(1),
            Value::Text("ada".into()),
            Value::Number("12.50".into()),
            Value::Int(1),
            Value::Json("{\"a\": 1}".into()),
            Value::Number("18446744073709551615".into()),
            Value::Bytes(vec![0, 255]),
            Value::Text("2024-01-02 03:04:05".into()),
        ]
    );
    assert_eq!(result.rows[1][2], Value::Null);
    assert_eq!(result.columns[2].type_name, "decimal");

    let schema = block(session.schema()).unwrap();
    let orders = schema
        .objects
        .iter()
        .find(|o| o.name == "solder_orders")
        .unwrap();
    assert!(orders.columns[0].primary_key && !orders.columns[1].nullable);
    assert!(orders.indexes.contains(&"by_customer".to_string()));
    assert_eq!(run(&session, &session.preview_query(orders)).rows.len(), 2);

    let err = block(session.query("SELEC 1".into())).unwrap_err();
    assert!(err.contains("syntax"), "{err}");
    assert_eq!(run(&session, "SELECT 1").rows[0][0], Value::Int(1));

    let ro = block(Session::connect(ConnectionSpec {
        read_only: true,
        ..spec
    }))
    .unwrap();
    let err = block(ro.query("DELETE FROM solder_orders".into())).unwrap_err();
    assert!(err.contains("READ ONLY"), "{err}");
    run(&session, "DROP TABLE solder_orders");
}

#[test]
fn redis() {
    let Some(spec) = spec("SOLDER_TEST_REDIS", Engine::Redis, false) else {
        return;
    };
    let session = block(Session::connect(spec.clone())).unwrap();
    run(
        &session,
        "DEL solder:user solder:list \"solder:with space\"",
    );
    run(&session, "HSET solder:user name ada lang rust");
    run(&session, "RPUSH solder:list a b c");
    run(&session, "SET \"solder:with space\" \"hello world\"");

    let hash = run(&session, "HGETALL solder:user");
    assert_eq!(hash.columns[0].name, "field");
    assert!(
        hash.rows
            .contains(&vec![Value::Text("name".into()), Value::Text("ada".into())])
    );
    let get = run(&session, "GET \"solder:with space\"");
    assert_eq!(get.rows, vec![vec![Value::Text("hello world".into())]]);
    assert_eq!(
        run(&session, "GET solder:missing").rows,
        vec![vec![Value::Null]]
    );

    let schema = block(session.schema()).unwrap();
    let list = schema
        .objects
        .iter()
        .find(|o| o.name == "solder:list")
        .unwrap();
    assert_eq!(list.kind, ObjectKind::Key("list".into()));
    assert_eq!(run(&session, &session.preview_query(list)).rows.len(), 3);
    let spaced = schema
        .objects
        .iter()
        .find(|o| o.name == "solder:with space")
        .unwrap();
    assert_eq!(run(&session, &session.preview_query(spaced)).rows.len(), 1);

    assert!(block(session.query("NOSUCHCOMMAND".into())).is_err());
    let ro = block(Session::connect(ConnectionSpec {
        read_only: true,
        ..spec
    }))
    .unwrap();
    assert!(
        block(ro.query("DEL solder:user".into()))
            .unwrap_err()
            .contains("read-only")
    );
    assert_eq!(
        run(&ro, "HGET solder:user lang").rows[0][0],
        Value::Text("rust".into())
    );
    run(
        &session,
        "DEL solder:user solder:list \"solder:with space\"",
    );
}

#[test]
fn mongo() {
    let Some(spec) = spec("SOLDER_TEST_MONGO", Engine::Mongo, false) else {
        return;
    };
    let session = block(Session::connect(spec.clone())).unwrap();
    run(&session, "db.solder_people.deleteMany({})");
    let inserted = run(
        &session,
        "db.solder_people.insertMany([\n  {name: 'ada', age: 36, tags: ['math'], joined: ISODate('2024-01-01T00:00:00Z')},\n  {name: 'bob', age: 25}\n])",
    );
    assert_eq!(inserted.affected, Some(2));

    let found = run(
        &session,
        "db.solder_people.find({age: {$gt: 30}}, {_id: 0}).sort({name: 1})",
    );
    let names: Vec<_> = found.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["name", "age", "tags", "joined"]);
    assert_eq!(found.rows.len(), 1);
    assert_eq!(found.rows[0][1], Value::Int(36));
    assert_eq!(found.rows[0][2], Value::Json("[\"math\"]".into()));
    assert_eq!(found.rows[0][3], Value::Text("2024-01-01T00:00:00Z".into()));

    let count = run(&session, "db.solder_people.countDocuments({name: /^b/})");
    assert_eq!(count.rows[0][0], Value::Int(1));
    let updated = run(
        &session,
        "db.solder_people.updateOne({name: 'bob'}, {$set: {age: 26}})",
    );
    assert_eq!(updated.affected, Some(1));
    let agg = run(
        &session,
        "db.solder_people.aggregate([{$group: {_id: null, total: {$sum: '$age'}}}])",
    );
    assert_eq!(agg.rows[0][1], Value::Int(62));

    let schema = block(session.schema()).unwrap();
    let people = schema
        .objects
        .iter()
        .find(|o| o.name == "solder_people")
        .unwrap();
    assert_eq!(people.kind, ObjectKind::Collection);
    assert!(
        people
            .columns
            .iter()
            .any(|c| c.name == "_id" && c.primary_key)
    );
    assert_eq!(run(&session, &session.preview_query(people)).rows.len(), 2);

    let err = block(session.query("db.solder_people.find({a: })".into())).unwrap_err();
    assert!(err.contains("Expected"), "{err}");
    let ro = block(Session::connect(ConnectionSpec {
        read_only: true,
        ..spec
    }))
    .unwrap();
    assert!(
        block(ro.query("db.solder_people.deleteMany({})".into()))
            .unwrap_err()
            .contains("read-only")
    );
    run(&session, "db.solder_people.deleteMany({})");
}

/// TLS against servers with a certificate from a throwaway CA
/// (`SOLDER_TEST_TLS_CA`, the CA's PEM file) for `localhost`:
///
/// SOLDER_TEST_POSTGRES_TLS=postgres://postgres:solder@localhost:55433/solder
/// SOLDER_TEST_REDIS_TLS=rediss://localhost:56380
/// SOLDER_TEST_MONGO_TLS=mongodb://localhost:57018/solder
///
/// MySQL creates its own certificate, so `SOLDER_TEST_MYSQL` covers it.
fn tls_ca() -> Option<String> {
    let ca = std::env::var("SOLDER_TEST_TLS_CA")
        .ok()
        .filter(|c| !c.is_empty());
    if ca.is_none() {
        eprintln!("skipped: SOLDER_TEST_TLS_CA is not set");
    }
    ca
}

fn connect(spec: &ConnectionSpec, url: String) -> db::Result<Session> {
    block(Session::connect(ConnectionSpec {
        url,
        ..spec.clone()
    }))
}

fn one(session: &Session, query: &str) -> Value {
    run(session, query).rows.remove(0).remove(0)
}

#[test]
fn postgres_tls_modes() {
    let (Some(spec), Some(ca)) = (
        spec("SOLDER_TEST_POSTGRES_TLS", Engine::Postgres, false),
        tls_ca(),
    ) else {
        return;
    };
    let url = spec.url.clone();
    let encrypted = "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()";

    // libpq's default and `require`: encrypted, certificate not checked.
    let session = connect(&spec, url.clone()).unwrap();
    assert_eq!(one(&session, encrypted), Value::Bool(true));
    let session = connect(&spec, format!("{url}?sslmode=require")).unwrap();
    assert_eq!(one(&session, encrypted), Value::Bool(true));
    let session = connect(&spec, format!("{url}?sslmode=disable")).unwrap();
    assert_eq!(one(&session, encrypted), Value::Bool(false));

    // Checked: a private CA is not in the Mozilla roots until named.
    let err = connect(&spec, format!("{url}?sslmode=verify-full"))
        .err()
        .unwrap();
    assert!(err.to_lowercase().contains("certificate"), "{err}");
    let session = connect(&spec, format!("{url}?sslmode=verify-full&sslrootcert={ca}")).unwrap();
    assert_eq!(one(&session, encrypted), Value::Bool(true));

    for params in [
        "sslaccept=strict",
        "sslrootcert=system",
        "sslmode=require&sslaccept=strict",
    ] {
        let error = connect(&spec, format!("{url}?{params}")).err().unwrap();
        assert!(error.to_lowercase().contains("certificate"), "{error}");
    }
    let session = connect(
        &spec,
        format!("{url}?sslaccept=strict&sslcert={ca}&channel_binding=require"),
    )
    .unwrap();
    assert_eq!(one(&session, encrypted), Value::Bool(true));
    let wrong_host = url.replace("localhost", "127.0.0.1");
    let error = connect(
        &spec,
        format!("{wrong_host}?sslmode=verify-full&sslrootcert={ca}"),
    )
    .err()
    .unwrap();
    assert!(error.to_lowercase().contains("certificate"), "{error}");

    // Prisma's parameters: `schema` sets the search path, the rest is dropped.
    run(&session, "CREATE SCHEMA IF NOT EXISTS solder_app");
    let prisma = connect(
        &spec,
        format!("{url}?schema=solder_app&connection_limit=5&pool_timeout=10"),
    )
    .unwrap();
    assert_eq!(
        one(&prisma, "SELECT current_schema()"),
        Value::Text("solder_app".into())
    );
    // Behind PgBouncer no prepared statements: values come back as text.
    let pooled = connect(&spec, format!("{url}?pgbouncer=true")).unwrap();
    assert_eq!(one(&pooled, "SELECT 1"), Value::Text("1".into()));
}

#[test]
fn mysql_tls_modes() {
    let Some(spec) = spec("SOLDER_TEST_MYSQL", Engine::MySql, false) else {
        return;
    };
    let url = spec.url.clone();
    let cipher = "SELECT VARIABLE_VALUE FROM performance_schema.session_status \
                  WHERE VARIABLE_NAME = 'Ssl_cipher'";
    // Local servers: no TLS unless asked.
    let plain = connect(&spec, url.clone()).unwrap();
    assert_eq!(one(&plain, cipher), Value::Text(String::new()));
    for asked in [
        "ssl-mode=REQUIRED",
        "sslaccept=accept_invalid_certs",
        "tls=skip-verify",
    ] {
        let session = connect(&spec, format!("{url}?{asked}")).unwrap();
        assert!(
            matches!(one(&session, cipher), Value::Text(c) if !c.is_empty()),
            "{asked}"
        );
    }
    // MySQL's own certificate is self-signed: a checked connection fails.
    let err = connect(&spec, format!("{url}?sslaccept=strict"))
        .err()
        .unwrap();
    assert!(err.to_lowercase().contains("certificate"), "{err}");
}

#[test]
fn mysql_tls_with_ca() {
    let (Some(spec), Some(ca)) = (
        spec("SOLDER_TEST_MYSQL_TLS", Engine::MySql, false),
        tls_ca(),
    ) else {
        return;
    };
    let url = spec.url.clone();
    let error = connect(&spec, format!("{url}?ssl-mode=VERIFY_IDENTITY"))
        .err()
        .unwrap();
    assert!(error.to_lowercase().contains("certificate"), "{error}");
    let session = connect(&spec, format!("{url}?ssl-mode=VERIFY_IDENTITY&ssl-ca={ca}")).unwrap();
    let result = run(&session, "SHOW STATUS LIKE 'Ssl_cipher'");
    assert!(!result.rows[0][1].display().is_empty());
    let wrong_host = url.replace("localhost", "127.0.0.1");
    let error = connect(&spec, format!("{wrong_host}?ssl-ca={ca}"))
        .err()
        .unwrap();
    assert!(error.to_lowercase().contains("certificate"), "{error}");
}

#[test]
fn redis_tls() {
    let (Some(spec), Some(ca)) = (
        spec("SOLDER_TEST_REDIS_TLS", Engine::Redis, false),
        tls_ca(),
    ) else {
        return;
    };
    let err = connect(&spec, spec.url.clone()).err().unwrap();
    assert!(err.to_lowercase().contains("certificate"), "{err}");
    let session = connect(&spec, format!("{}/#insecure", spec.url)).unwrap();
    assert_eq!(one(&session, "PING"), Value::Text("PONG".into()));
    let session = connect(&spec, format!("{}/?sslrootcert={ca}", spec.url)).unwrap();
    assert_eq!(one(&session, "PING"), Value::Text("PONG".into()));
    let wrong_host = spec.url.replace("localhost", "127.0.0.1");
    let error = connect(&spec, format!("{wrong_host}/?sslrootcert={ca}"))
        .err()
        .unwrap();
    assert!(error.to_lowercase().contains("certificate"), "{error}");
}

#[test]
fn mongo_tls() {
    let (Some(spec), Some(ca)) = (
        spec("SOLDER_TEST_MONGO_TLS", Engine::Mongo, false),
        tls_ca(),
    ) else {
        return;
    };
    let url = spec.url.clone();
    let err = connect(&spec, format!("{url}?tls=true")).err().unwrap();
    assert!(err.to_lowercase().contains("certificate"), "{err}");
    for ok in [
        format!("{url}?tls=true&tlsCAFile={ca}"),
        format!("{url}?tls=true&tlsAllowInvalidCertificates=true"),
    ] {
        let session = connect(&spec, ok.clone()).unwrap_or_else(|e| panic!("{ok}: {e}"));
        run(&session, "show collections");
    }
    let wrong_host = url.replace("localhost", "127.0.0.1");
    let error = connect(&spec, format!("{wrong_host}?tls=true&tlsCAFile={ca}"))
        .err()
        .unwrap();
    assert!(error.to_lowercase().contains("certificate"), "{error}");
}

/// Staged edits: all statements in one transaction, rolled back when one
/// does not match exactly one row.
fn check_apply(session: &Session, table: &str) {
    let q = |ident: &str| db::sql::quote_ident(session.engine(), ident);
    let t = q(table);
    run(session, &format!("DROP TABLE IF EXISTS {t}"));
    run(
        session,
        &format!("CREATE TABLE {t} (id INT PRIMARY KEY, name VARCHAR(20))"),
    );
    run(
        session,
        &format!("INSERT INTO {t} (id, name) VALUES (1, 'ada'), (2, 'bob'), (3, 'cy')"),
    );
    let names = |session: &Session| {
        run(session, &format!("SELECT name FROM {t} ORDER BY id"))
            .rows
            .into_iter()
            .map(|r| r[0].display())
            .collect::<Vec<_>>()
    };
    // Setting a value to what it already is still counts as one row.
    block(session.apply(vec![
        format!("UPDATE {t} SET name = 'Ada' WHERE id = 1"),
        format!("UPDATE {t} SET name = 'cy' WHERE id = 3"),
        format!("DELETE FROM {t} WHERE id = 2"),
    ]))
    .unwrap();
    assert_eq!(names(session), ["Ada", "cy"]);
    let err = block(session.apply(vec![
        format!("UPDATE {t} SET name = 'changed' WHERE id = 1"),
        format!("UPDATE {t} SET name = 'gone' WHERE id = 2"),
    ]))
    .unwrap_err();
    assert!(err.contains("Change 2 of 2 matched no row"), "{err}");
    assert_eq!(
        names(session),
        ["Ada", "cy"],
        "the first change was rolled back"
    );
    let err =
        block(session.apply(vec![format!("UPDATE {t} SET nope = 1 WHERE id = 1")])).unwrap_err();
    assert!(err.contains("Nothing was saved"), "{err}");
    // The connection is usable after a rollback.
    assert_eq!(names(session), ["Ada", "cy"]);
    run(session, &format!("DROP TABLE {t}"));
}

#[test]
fn postgres_apply() {
    if let Some(spec) = spec("SOLDER_TEST_POSTGRES", Engine::Postgres, false) {
        check_apply(&block(Session::connect(spec)).unwrap(), "solder_apply");
    }
}

#[test]
fn mysql_apply() {
    if let Some(spec) = spec("SOLDER_TEST_MYSQL", Engine::MySql, false) {
        check_apply(&block(Session::connect(spec)).unwrap(), "solder_apply");
    }
}

#[test]
fn sqlite_apply() {
    let path = std::env::temp_dir().join(format!("solder-apply-{}.db", std::process::id()));
    std::fs::write(&path, b"").unwrap();
    let spec = ConnectionSpec {
        name: "apply".into(),
        engine: Engine::Sqlite,
        url: path.display().to_string(),
        source: "test".into(),
        read_only: false,
    };
    check_apply(&block(Session::connect(spec)).unwrap(), "solder_apply");
}
