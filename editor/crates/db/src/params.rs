//! Connection URLs as projects write them. `.env` files carry parameters
//! meant for other drivers (Prisma's `schema=public`, `pgbouncer=true`,
//! `sslaccept`, node's `ssl={...}`) and the drivers here reject anything
//! unknown, so URLs are normalized first: what means something is
//! translated, pool parameters are dropped, and unsupported TLS settings
//! are rejected instead of silently weakening certificate verification.

use std::path::PathBuf;

use url::form_urlencoded;

/// How to secure a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tls {
    Off,
    /// Encrypt without checking the certificate.
    Unverified,
    /// Check the certificate against the Mozilla roots, or the CA in the
    /// file when there is one.
    Verified(Option<PathBuf>),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Postgres {
    /// A URL tokio-postgres accepts.
    pub url: String,
    pub tls: Tls,
    /// Named prepared statements work (not behind PgBouncer in transaction
    /// mode). Used only to learn column types.
    pub prepare: bool,
}

const PG_KEYS: &[&str] = &[
    "user",
    "password",
    "dbname",
    "options",
    "application_name",
    "sslnegotiation",
    "host",
    "hostaddr",
    "port",
    "connect_timeout",
    "tcp_user_timeout",
    "keepalives",
    "keepalives_idle",
    "keepalives_interval",
    "keepalives_retries",
    "target_session_attrs",
    "channel_binding",
    "load_balance_hosts",
];

fn split(url: &str, literal_plus: bool) -> (&str, Vec<(String, String)>) {
    let (base, query) = url.split_once('?').unwrap_or((url, ""));
    let query = query.split('#').next().unwrap_or("");
    let query = if literal_plus {
        query.replace('+', "%2B")
    } else {
        query.into()
    };
    (
        base,
        form_urlencoded::parse(query.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect(),
    )
}

/// Percent-encodes values, spaces as `%20`: tokio-postgres does not read
/// `+` as a space.
fn join(base: &str, params: &[(String, String)]) -> String {
    if params.is_empty() {
        return base.to_string();
    }
    let encode = |s: &str| -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    };
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect();
    format!("{base}?{}", query.join("&"))
}

/// libpq's rules: `require` and `prefer` encrypt without checking, unless a
/// root certificate is given; `verify-ca` and `verify-full` check (rustls
/// always checks the host name too).
pub fn postgres(url: &str) -> crate::Result<Postgres> {
    let (base, params) = split(url, true);
    let mut kept = Vec::new();
    let mut mode = None;
    let mut root: Option<String> = None;
    let mut strict = false;
    let mut options: Vec<String> = Vec::new();
    let mut prepare = true;
    for (key, value) in params {
        match key.as_str() {
            "sslmode" => mode = Some(value.to_ascii_lowercase()),
            "sslrootcert" | "sslcert" => {
                root = Some(certificate_path(&value)?.to_string_lossy().into_owned())
            }
            "sslaccept" => strict = matches!(ssl_accept(&value)?, Tls::Verified(_)),
            "schema" => {
                if value.is_empty()
                    || !value.chars().all(|character| {
                        character.is_alphanumeric() || character == '_' || character == '$'
                    })
                {
                    return Err("Unsupported schema name in connection URL".into());
                }
                options.push(format!("-c search_path=\"{value}\""));
            }
            "pgbouncer" => prepare = !boolean(&key, &value)?,
            "options" => options.insert(0, value),
            k if PG_KEYS.contains(&k) => kept.push((key, value)),
            key if tls_parameter(key) => return Err(format!("Unsupported TLS parameter: {key}")),
            _ => {}
        }
    }
    let system = root.as_deref() == Some("system");
    let mode = mode.as_deref().unwrap_or(if system || strict {
        "verify-full"
    } else {
        "prefer"
    });
    if system && mode != "verify-full" {
        return Err("sslrootcert=system requires sslmode=verify-full".into());
    }
    if strict && mode == "disable" {
        return Err("sslaccept=strict cannot be combined with sslmode=disable".into());
    }
    let root = root.filter(|root| root != "system").map(PathBuf::from);
    let (sslmode, tls) = match mode {
        "disable" => ("disable", Tls::Off),
        "verify-ca" | "verify-full" => ("require", Tls::Verified(root)),
        "require" | "prefer" | "allow" if root.is_some() || strict => {
            ("require", Tls::Verified(root))
        }
        "require" => ("require", Tls::Unverified),
        "allow" | "prefer" => ("prefer", Tls::Unverified),
        _ => return Err("Unsupported PostgreSQL sslmode".into()),
    };
    kept.push(("sslmode".into(), sslmode.into()));
    if !options.is_empty() {
        kept.push(("options".into(), options.join(" ")));
    }
    Ok(Postgres {
        url: join(base, &kept),
        tls,
        prepare,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct MySql {
    /// A URL mysql_async accepts, without TLS parameters.
    pub url: String,
    pub tls: Tls,
}

const MYSQL_KEYS: &[&str] = &[
    "compression",
    "tcp_nodelay",
    "tcp_keepalive",
    "wait_timeout",
    "max_allowed_packet",
    "socket",
    "prefer_socket",
    "secure_auth",
    "enable_cleartext_plugin",
    "client_found_rows",
    "stmt_cache_size",
];

/// TLS from whichever convention the URL uses: MySQL's `ssl-mode`, Prisma's
/// `sslaccept`, node's `ssl={"rejectUnauthorized":...}`, Go's `tls`, or
/// mysql_async's own flags. Remote defaults require a verified certificate
/// and hostname; local connections use plain text unless TLS is requested.
pub fn mysql(url: &str) -> crate::Result<MySql> {
    let url = url.replacen("mariadb://", "mysql://", 1);
    let (base, params) = split(&url, false);
    let mut kept = Vec::new();
    let mut tls: Option<Tls> = None;
    let mut root: Option<PathBuf> = None;
    let mut verify_ca = None;
    for (key, value) in params {
        let lower = value.to_ascii_lowercase();
        match key.as_str() {
            "ssl-mode" | "sslmode" | "ssl_mode" => {
                tls = Some(match lower.as_str() {
                    "disabled" | "disable" => Tls::Off,
                    "verify_ca" | "verify_identity" | "verify-ca" | "verify-full" => {
                        Tls::Verified(None)
                    }
                    "required" | "require" | "preferred" | "prefer" => Tls::Unverified,
                    _ => return Err("Unsupported MySQL ssl-mode".into()),
                })
            }
            "sslaccept" => {
                tls = Some(ssl_accept(&value)?);
            }
            "ssl" => {
                tls = Some(match lower.as_str() {
                    "false" | "0" => Tls::Off,
                    "true" | "1" => Tls::Verified(None),
                    _ => {
                        let options: serde_json::Value = serde_json::from_str(&value)
                            .map_err(|_| "Invalid MySQL ssl options".to_string())?;
                        let options = options.as_object().ok_or("Invalid MySQL ssl options")?;
                        if options.keys().any(|key| key != "rejectUnauthorized") {
                            return Err(
                                "Unsupported MySQL ssl option; use ssl-ca for a CA file".into()
                            );
                        }
                        match options.get("rejectUnauthorized") {
                            Some(serde_json::Value::Bool(false)) => Tls::Unverified,
                            Some(serde_json::Value::Bool(true)) | None => Tls::Verified(None),
                            _ => return Err("Invalid MySQL rejectUnauthorized setting".into()),
                        }
                    }
                });
            }
            "tls" => {
                tls = Some(match lower.as_str() {
                    "false" => Tls::Off,
                    "skip-verify" | "preferred" => Tls::Unverified,
                    "true" => Tls::Verified(None),
                    _ => return Err("Unsupported MySQL tls mode".into()),
                })
            }
            "require_ssl" => {
                tls = Some(if boolean(&key, &value)? {
                    Tls::Verified(None)
                } else {
                    Tls::Off
                })
            }
            "verify_ca" => verify_ca = Some(boolean(&key, &value)?),
            "verify_identity" if !boolean(&key, &value)? => return Err(
                "verify_identity=false is not supported; use a certificate for the server hostname"
                    .into(),
            ),
            "verify_identity" => {}
            "sslrootcert" | "ssl-ca" | "sslca" | "sslcert" => {
                root = Some(certificate_path(&value)?)
            }
            k if MYSQL_KEYS.contains(&k) => kept.push((key, value)),
            key if tls_parameter(key) => return Err(format!("Unsupported TLS parameter: {key}")),
            _ => {}
        }
    }
    let local = url::Url::parse(base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .is_none_or(|h| matches!(h.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]"));
    let tls = match (tls, root) {
        (Some(Tls::Off), _) => Tls::Off,
        (_, Some(root)) => Tls::Verified(Some(root)),
        (Some(tls), None) => tls,
        (None, None) if local => Tls::Off,
        (None, None) => Tls::Verified(None),
    };
    let tls = match (tls, verify_ca) {
        (Tls::Off, _) => Tls::Off,
        (_, Some(false)) => Tls::Unverified,
        (Tls::Unverified, Some(true)) => Tls::Verified(None),
        (tls, _) => tls,
    };
    Ok(MySql {
        url: join(base, &kept),
        tls,
    })
}

fn ssl_accept(value: &str) -> crate::Result<Tls> {
    match value.to_ascii_lowercase().as_str() {
        "strict" => Ok(Tls::Verified(None)),
        "accept_invalid_certs" => Ok(Tls::Unverified),
        _ => Err("Unsupported sslaccept value".into()),
    }
}

fn boolean(key: &str, value: &str) -> crate::Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("Invalid boolean connection parameter: {key}")),
    }
}

fn certificate_path(value: &str) -> crate::Result<PathBuf> {
    if value.is_empty() {
        Err("CA certificate path cannot be empty".into())
    } else {
        Ok(PathBuf::from(value))
    }
}

fn tls_parameter(key: &str) -> bool {
    key.starts_with("ssl")
        || key.starts_with("tls")
        || key.starts_with("verify_")
        || key == "built_in_roots"
}

pub struct Redis {
    pub url: String,
    pub root: Option<PathBuf>,
}

pub fn redis(url: &str) -> crate::Result<Redis> {
    let mut parsed =
        url::Url::parse(url).map_err(|_| "Invalid Redis connection URL".to_string())?;
    let mut root = None;
    let mut kept = Vec::new();
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "sslrootcert" | "ssl-ca" => root = Some(certificate_path(&value)?),
            key if tls_parameter(key) => return Err(format!("Unsupported TLS parameter: {key}")),
            _ => kept.push((key.into_owned(), value.into_owned())),
        }
    }
    if root.is_some() && (parsed.scheme() != "rediss" || parsed.fragment().is_some()) {
        return Err("A Redis CA file requires rediss:// without #insecure".into());
    }
    parsed.set_query(None);
    if !kept.is_empty() {
        parsed.query_pairs_mut().extend_pairs(kept);
    }
    Ok(Redis {
        url: parsed.into(),
        root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn postgres(url: &str) -> Postgres {
        super::postgres(url).unwrap()
    }

    fn mysql(url: &str) -> MySql {
        super::mysql(url).unwrap()
    }

    #[test]
    fn postgres_urls_from_prisma_and_libpq() {
        let pg =
            postgres("postgresql://app:pw@localhost:5432/app?schema=public&connection_limit=5");
        assert_eq!(
            pg.url,
            "postgresql://app:pw@localhost:5432/app?sslmode=prefer&options=-c%20search_path%3D%22public%22"
        );
        assert_eq!(pg.tls, Tls::Unverified);
        assert!(pg.prepare);

        let neon = postgres(
            "postgres://u:p@ep-x.eu-central-1.aws.neon.tech/db?sslmode=require&channel_binding=require",
        );
        assert_eq!(
            neon.url,
            "postgres://u:p@ep-x.eu-central-1.aws.neon.tech/db?channel_binding=require&sslmode=require"
        );
        assert_eq!(neon.tls, Tls::Unverified);

        let pooled = postgres("postgres://u:p@pooler.supabase.com:6543/postgres?pgbouncer=true");
        assert!(!pooled.prepare);
        assert_eq!(
            postgres("postgres://h/db?sslmode=verify-full").tls,
            Tls::Verified(None)
        );
        assert_eq!(
            postgres("postgres://h/db?sslmode=require&sslrootcert=/etc/rds.pem").tls,
            Tls::Verified(Some("/etc/rds.pem".into()))
        );
        assert_eq!(postgres("postgres://h/db?sslmode=disable").tls, Tls::Off);
    }

    #[test]
    fn mysql_urls_from_every_convention() {
        let local = mysql("mysql://root:pw@localhost:3306/shop?connection_limit=5");
        assert_eq!(local.url, "mysql://root:pw@localhost:3306/shop");
        assert_eq!(local.tls, Tls::Off);
        assert_eq!(
            mysql("mysql://u:p@db.example.com/shop").tls,
            Tls::Verified(None)
        );
        assert_eq!(
            mysql("mysql://u:p@aws.connect.psdb.cloud/shop?sslaccept=strict").tls,
            Tls::Verified(None)
        );
        assert_eq!(
            mysql(r#"mysql://u:p@h/db?ssl={"rejectUnauthorized":true}"#).tls,
            Tls::Verified(None)
        );
        assert_eq!(
            mysql(r#"mysql://u:p@h/db?ssl={"rejectUnauthorized":false}"#).tls,
            Tls::Unverified
        );
        assert_eq!(mysql("mysql://u:p@h/db?ssl-mode=DISABLED").tls, Tls::Off);
        assert_eq!(
            mysql("mysql://u:p@localhost/db?ssl-mode=REQUIRED").tls,
            Tls::Unverified
        );
        assert_eq!(
            mysql("mysql://u:p@h/db?sslcert=./ca.pem").tls,
            Tls::Verified(Some("./ca.pem".into()))
        );
        assert_eq!(
            mysql("mariadb://u:p@h/db?tcp_nodelay=true&tls=skip-verify"),
            MySql {
                url: "mysql://u:p@h/db?tcp_nodelay=true".into(),
                tls: Tls::Unverified
            }
        );
    }

    #[test]
    fn certificate_checks_are_not_silently_downgraded() {
        for url in [
            "postgres://h/db?sslaccept=strict",
            "postgres://h/db?sslmode=require&sslaccept=strict",
            "postgres://h/db?sslrootcert=system",
        ] {
            assert_eq!(postgres(url).tls, Tls::Verified(None));
        }
        assert_eq!(
            postgres("postgres://h/db?sslcert=ca.pem").tls,
            Tls::Verified(Some("ca.pem".into()))
        );
        for url in [
            "postgres://h/db?sslmode=verfy-full",
            "postgres://h/db?sslaccept=strct",
            "postgres://h/db?sslrootcert=system&sslmode=require",
            "postgres://h/db?sslaccept=strict&sslmode=disable",
            "postgres://h/db?sslidentity=identity.p12",
            "postgres://h/db?schema=public%20-c%20role%3Dadmin",
        ] {
            assert!(super::postgres(url).is_err(), "{url}");
        }
        for url in [
            "mysql://h/db?ssl-mode=VERFY_IDENTITY",
            "mysql://h/db?sslaccept=strct",
            "mysql://h/db?sslidentity=identity.p12",
            "mysql://h/db?ssl={\"ca\":\"certificate\"}",
        ] {
            assert!(super::mysql(url).is_err(), "{url}");
        }
        assert_eq!(mysql("mysql://h/db?require_ssl=false").tls, Tls::Off);
        for params in [
            "require_ssl=true&verify_ca=false",
            "verify_ca=false&require_ssl=true",
        ] {
            assert_eq!(
                mysql(&format!("mysql://h/db?{params}")).tls,
                Tls::Unverified
            );
        }
        assert_eq!(
            mysql(r#"mysql://h/db?ssl={"rejectUnauthorized": false}"#).tls,
            Tls::Unverified
        );
    }

    #[test]
    fn redis_ca_options_preserve_connection_settings() {
        let options =
            redis("rediss://user:password@localhost/2?protocol=resp3&sslrootcert=ca.pem").unwrap();
        assert_eq!(
            options.url,
            "rediss://user:password@localhost/2?protocol=resp3"
        );
        assert_eq!(options.root, Some("ca.pem".into()));
        for url in [
            "redis://localhost?sslrootcert=ca.pem",
            "rediss://localhost?sslrootcert=ca.pem#insecure",
            "rediss://localhost?sslrootcert=",
        ] {
            assert!(redis(url).is_err());
        }
    }
}
