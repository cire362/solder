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
    assert!(
        users
            .indexes
            .iter()
            .any(|i| i.name == "solder_users_name" && i.columns == ["name"])
    );
    assert!(
        users
            .indexes
            .iter()
            .any(|i| i.primary && i.unique && i.columns == ["id"])
    );
    assert_eq!(users.primary_key_name.as_deref(), Some("solder_users_pkey"));
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
    assert!(
        orders
            .indexes
            .iter()
            .any(|i| i.name == "by_customer" && !i.unique && i.columns == ["customer"])
    );
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
        format!("INSERT INTO {t} (id, name) VALUES (4, 'di')"),
    ]))
    .unwrap();
    assert_eq!(names(session), ["Ada", "cy", "di"]);
    let err = block(session.apply(vec![
        format!("UPDATE {t} SET name = 'changed' WHERE id = 1"),
        format!("UPDATE {t} SET name = 'gone' WHERE id = 2"),
    ]))
    .unwrap_err();
    assert!(err.contains("Change 2 of 2 matched no row"), "{err}");
    assert_eq!(
        names(session),
        ["Ada", "cy", "di"],
        "the first change was rolled back"
    );
    let err =
        block(session.apply(vec![format!("UPDATE {t} SET nope = 1 WHERE id = 1")])).unwrap_err();
    assert!(err.contains("Nothing was saved"), "{err}");
    // The connection is usable after a rollback.
    assert_eq!(names(session), ["Ada", "cy", "di"]);
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
    let path = db::testing::dir("sqlite-apply").join("test.db");
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

/// Defaults, server-filled columns and foreign keys come back in the schema.
fn check_schema_details(session: &Session, ddl: &[&str]) {
    for statement in [
        "DROP TABLE IF EXISTS solder_o",
        "DROP TABLE IF EXISTS solder_c",
    ]
    .iter()
    .chain(ddl)
    {
        run(session, statement);
    }
    let schema = block(session.schema()).unwrap();
    let table = |name: &str| schema.objects.iter().find(|o| o.name == name).unwrap();
    let customers = table("solder_c");
    assert!(customers.columns[0].auto, "{:?}", customers.columns[0]);
    assert!(
        customers.columns[1]
            .default
            .as_deref()
            .is_some_and(|d| d.contains("anon")),
        "{:?}",
        customers.columns[1]
    );
    let orders = table("solder_o");
    let [fk] = orders.foreign_keys.as_slice() else {
        panic!("{:?}", orders.foreign_keys)
    };
    assert_eq!(fk.columns, ["c_id"]);
    assert_eq!(fk.ref_table, "solder_c");
    assert_eq!(fk.ref_columns, ["id"]);
    assert!(!orders.columns[1].auto);
    run(session, "DROP TABLE solder_o");
    run(session, "DROP TABLE solder_c");
}

#[test]
fn postgres_schema_details() {
    if let Some(spec) = spec("SOLDER_TEST_POSTGRES", Engine::Postgres, false) {
        check_schema_details(
            &block(Session::connect(spec)).unwrap(),
            &[
                "CREATE TABLE solder_c (id SERIAL PRIMARY KEY, name TEXT NOT NULL DEFAULT 'anon')",
                "CREATE TABLE solder_o (id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                 c_id INT REFERENCES solder_c(id), status TEXT DEFAULT 'new')",
            ],
        );
    }
}

#[test]
fn mysql_schema_details() {
    if let Some(spec) = spec("SOLDER_TEST_MYSQL", Engine::MySql, false) {
        check_schema_details(
            &block(Session::connect(spec)).unwrap(),
            &[
                "CREATE TABLE solder_c (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20) NOT NULL DEFAULT 'anon')",
                "CREATE TABLE solder_o (id INT AUTO_INCREMENT PRIMARY KEY, c_id INT, status VARCHAR(10) DEFAULT 'new', \
                 CONSTRAINT solder_o_c FOREIGN KEY (c_id) REFERENCES solder_c(id))",
            ],
        );
    }
}

#[test]
fn sqlite_schema_details() {
    let path = db::testing::dir("sqlite-details").join("test.db");
    std::fs::write(&path, b"").unwrap();
    let session = block(Session::connect(ConnectionSpec {
        name: "details".into(),
        engine: Engine::Sqlite,
        url: path.display().to_string(),
        source: "test".into(),
        read_only: false,
    }))
    .unwrap();
    check_schema_details(
        &session,
        &[
            "CREATE TABLE solder_c (id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT 'anon')",
            // No column named: the referenced table's key.
            "CREATE TABLE solder_o (id INTEGER PRIMARY KEY, c_id INTEGER REFERENCES solder_c, status TEXT DEFAULT 'new')",
        ],
    );
}

#[test]
fn mongo_browse() {
    use db::browse::{Browse, and, count_query, page_query, value_filter};
    let Some(spec) = spec("SOLDER_TEST_MONGO", Engine::Mongo, false) else {
        return;
    };
    let session = block(Session::connect(spec)).unwrap();
    run(&session, "db.solder_browse.deleteMany({})");
    run(
        &session,
        "db.solder_browse.insertMany([{n: 1, s: 'a'}, {n: 2, s: 'b'}, {n: 3, s: 'a'}, {n: 4, s: 'a'}])",
    );
    let mut browse = Browse {
        table: "solder_browse".into(),
        ..Default::default()
    };
    browse.filter = value_filter(Engine::Mongo, "s", &Value::Text("a".into()));
    browse.filter = and(Engine::Mongo, &browse.filter, "{n: {$gt: 1}}");
    browse.sort = Some(("n".into(), true));
    let page = run(&session, &page_query(Engine::Mongo, &browse, 1, 1));
    let n = page.columns.iter().position(|c| c.name == "n").unwrap();
    assert_eq!(page.rows.len(), 1);
    // Matching a, n > 1, descending: 4, 3; the second page of one row is 3.
    assert_eq!(page.rows[0][n], Value::Int(3));
    let count = run(&session, &count_query(Engine::Mongo, &browse));
    assert_eq!(count.rows[0][0], Value::Int(2));
    run(&session, "db.solder_browse.deleteMany({})");
}

/// Structure changes run on the server: create, a broad alter, its inverse,
/// drop. Rows survive every step.
fn check_ddl(session: &Session) {
    use db::ddl::{Change, ColumnDraft, ForeignKeyDraft, IndexDraft, TableDraft, statements};
    let engine = session.engine();
    for t in ["solder_members", "solder_teams"] {
        run(session, &format!("DROP TABLE IF EXISTS {t}"));
    }
    let apply = |change: &Change| {
        let ddl = statements(engine, change).unwrap();
        block(session.apply_ddl(ddl.clone())).unwrap_or_else(|e| panic!("{ddl:#?}: {e}"));
    };
    let col = |name: &str, ty: &str, key: bool| ColumnDraft {
        name: name.into(),
        type_name: ty.into(),
        nullable: !key,
        primary_key: key,
        ..Default::default()
    };
    apply(&Change::Create(TableDraft {
        name: "solder_teams".into(),
        columns: vec![
            col("id", "integer", true),
            col("title", "varchar(50)", false),
        ],
        ..Default::default()
    }));
    apply(&Change::Create(TableDraft {
        name: "solder_members".into(),
        columns: vec![
            col("id", "integer", true),
            col("name", "varchar(40)", false),
            col("note", "varchar(20)", false),
        ],
        indexes: vec![IndexDraft {
            name: "solder_members_name".into(),
            columns: vec!["name".into()],
            ..Default::default()
        }],
        ..Default::default()
    }));
    run(
        session,
        "INSERT INTO solder_teams (id, title) VALUES (1, 'core')",
    );
    run(
        session,
        "INSERT INTO solder_members (id, name, note) VALUES (1, 'ada', 'x')",
    );

    let members = |session: &Session| {
        block(session.schema())
            .unwrap()
            .objects
            .into_iter()
            .find(|o| o.name == "solder_members")
            .unwrap()
    };
    let before = TableDraft::of(&members(session));
    let mut after = before.clone();
    after.columns[1].name = "full_name".into();
    after.columns[1].nullable = false;
    after.columns[2].type_name = "varchar(200)".into();
    after.indexes[0].columns = vec!["full_name".into()];
    after.columns.push(col("team_id", "integer", false));
    after.indexes.push(IndexDraft {
        name: "solder_members_team".into(),
        columns: vec!["team_id".into()],
        ..Default::default()
    });
    after.foreign_keys.push(ForeignKeyDraft {
        name: "solder_members_team_fk".into(),
        columns: vec!["team_id".into()],
        ref_table: "solder_teams".into(),
        ref_columns: vec!["id".into()],
        on_delete: Some("SET NULL".into()),
        ..Default::default()
    });
    let change = Change::Alter { before, after };
    apply(&change);
    let changed = members(session);
    let names: Vec<_> = changed.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "full_name", "note", "team_id"]);
    assert!(!changed.columns[1].nullable);
    assert!(
        changed.columns[2].type_name.contains("200"),
        "{:?}",
        changed.columns[2]
    );
    let [fk] = changed.foreign_keys.as_slice() else {
        panic!("{:?}", changed.foreign_keys)
    };
    assert_eq!(
        (fk.ref_table.as_str(), fk.on_delete.as_deref()),
        ("solder_teams", Some("SET NULL"))
    );
    assert!(changed.indexes.iter().any(|i| i.columns == ["team_id"]));
    assert_eq!(
        one(session, "SELECT full_name FROM solder_members"),
        Value::Text("ada".into())
    );

    apply(&change.inverse());
    let restored = members(session);
    let names: Vec<_> = restored.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "name", "note"]);
    assert!(restored.foreign_keys.is_empty());
    assert!(restored.columns[1].nullable);
    assert_eq!(
        one(session, "SELECT name FROM solder_members"),
        Value::Text("ada".into())
    );

    let schema = block(session.schema()).unwrap();
    for t in ["solder_members", "solder_teams"] {
        let object = schema.objects.iter().find(|o| o.name == t).unwrap();
        apply(&Change::Drop(TableDraft::of(object)));
    }
}

#[test]
fn postgres_ddl() {
    if let Some(spec) = spec("SOLDER_TEST_POSTGRES", Engine::Postgres, false) {
        check_ddl(&block(Session::connect(spec)).unwrap());
    }
}

#[test]
fn mysql_ddl() {
    if let Some(spec) = spec("SOLDER_TEST_MYSQL", Engine::MySql, false) {
        check_ddl(&block(Session::connect(spec)).unwrap());
    }
}

#[test]
fn sqlite_ddl() {
    let path = db::testing::dir("sqlite-ddl").join("test.db");
    std::fs::write(&path, b"").unwrap();
    check_ddl(
        &block(Session::connect(ConnectionSpec {
            name: "ddl".into(),
            engine: Engine::Sqlite,
            url: path.display().to_string(),
            source: "test".into(),
            read_only: false,
        }))
        .unwrap(),
    );
}

#[test]
fn redis_key_edits() {
    use db::edit::{Changes, edit_target, statements};
    let Some(spec) = spec("SOLDER_TEST_REDIS", Engine::Redis, false) else {
        return;
    };
    let session = block(Session::connect(spec)).unwrap();
    run(&session, "DEL solder:h solder:l solder:s");
    run(&session, "HSET solder:h name ada lang rust");
    run(&session, "RPUSH solder:l a b c");
    run(&session, "SET solder:s hi EX 1000");
    let save = |query: &str, changes: Changes| {
        let result = run(&session, query);
        let target =
            edit_target(Engine::Redis, &Default::default(), query, &result.columns).unwrap();
        let commands = statements(
            Engine::Redis,
            &target,
            &result.columns,
            &result.rows,
            &changes,
        );
        block(session.apply(commands)).unwrap();
        result
    };
    // Rename a hash field and change its value; drop another; add one.
    let fields = run(&session, "HGETALL solder:h");
    let name = fields
        .rows
        .iter()
        .position(|r| r[0] == Value::Text("name".into()))
        .unwrap();
    let lang = 1 - name;
    let mut changes = Changes::default();
    changes.cells.insert((name, 0), Some("full_name".into()));
    changes.cells.insert((name, 1), Some("Ada L".into()));
    changes.deleted.insert(lang);
    changes
        .inserted
        .push([(0, Some("age".into())), (1, Some("36".into()))].into());
    save("HGETALL solder:h", changes);
    let mut after: Vec<Vec<Value>> = run(&session, "HGETALL solder:h").rows;
    after.sort_by_key(|r| r[0].display());
    assert_eq!(
        after,
        vec![
            vec![Value::Text("age".into()), Value::Text("36".into())],
            vec![Value::Text("full_name".into()), Value::Text("Ada L".into())],
        ]
    );
    // List: change b, drop a, append d.
    let mut changes = Changes::default();
    changes.cells.insert((1, 1), Some("B".into()));
    changes.deleted.insert(0);
    changes.inserted.push([(1, Some("d".into()))].into());
    save("LRANGE solder:l 0 199", changes);
    let items: Vec<String> = run(&session, "LRANGE solder:l 0 -1")
        .rows
        .iter()
        .map(|r| r[1].display())
        .collect();
    assert_eq!(items, ["B", "c", "d"]);
    // A string keeps its expiry.
    let mut changes = Changes::default();
    changes.cells.insert((0, 0), Some("hello".into()));
    save("GET solder:s", changes);
    assert_eq!(one(&session, "GET solder:s"), Value::Text("hello".into()));
    assert!(matches!(one(&session, "TTL solder:s"), Value::Int(t) if t > 900));
    run(&session, "DEL solder:h solder:l solder:s");
}

#[test]
fn mongo_document_edits() {
    use db::edit::{Changes, edit_target, statements};
    let Some(spec) = spec("SOLDER_TEST_MONGO", Engine::Mongo, false) else {
        return;
    };
    let session = block(Session::connect(spec)).unwrap();
    run(&session, "db.solder_docs.deleteMany({})");
    run(
        &session,
        "db.solder_docs.insertMany([{name: 'ada', age: 36}, {name: 'bob', age: 25}])",
    );
    let query = "db.getCollection(\"solder_docs\").find({}).sort({\"name\": 1}).limit(200)";
    let result = run(&session, query);
    let col = |name: &str| result.columns.iter().position(|c| c.name == name).unwrap();
    let target = edit_target(Engine::Mongo, &Default::default(), query, &result.columns).unwrap();
    let mut changes = Changes::default();
    changes.cells.insert((0, col("age")), Some("37".into()));
    changes.cells.insert((0, col("name")), Some("Ada".into()));
    changes.deleted.insert(1);
    changes.inserted.push(
        [
            (col("name"), Some("cy".into())),
            (col("age"), Some("3".into())),
        ]
        .into(),
    );
    let calls = statements(
        Engine::Mongo,
        &target,
        &result.columns,
        &result.rows,
        &changes,
    );
    block(session.apply(calls.clone())).unwrap();
    let after = run(
        &session,
        "db.solder_docs.find({}, {_id: 0}).sort({name: 1})",
    );
    let rows: Vec<Vec<String>> = after
        .rows
        .iter()
        .map(|r| r.iter().map(Value::display).collect())
        .collect();
    assert_eq!(rows, [["Ada", "37"], ["cy", "3"]]);
    // The same delete again matches nothing: reported, not ignored.
    let err = block(session.apply(vec![calls[1].clone()])).unwrap_err();
    assert!(err.contains("matched no document"), "{err}");
    run(&session, "db.solder_docs.deleteMany({})");
}
