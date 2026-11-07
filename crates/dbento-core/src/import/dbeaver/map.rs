//! Maps one raw DBeaver connection to a connection draft. Postgres and MySQL go through the
//! shared `import::pg` and `import::mysql` rules; SQLite takes its path from the database field
//! or the URL.

use super::project::{RawConnection, RawHandler};
use crate::import::color::{ColorMatch, from_rgb};
use crate::import::datagrip::clean_folder;
use crate::import::model::{ConnectionDraft, SecretSetting, SkipReason, SourceTls, WarningKind};
use crate::import::mysql::{self, Driver};
use crate::import::pg;
use crate::import::sqlite::map_path;
use crate::{Color, Engine};
use std::collections::BTreeMap;
use std::path::Path;

#[cfg(test)]
mod tests;

/// What mapping needs from outside the connection: the project root for relative SQLite
/// paths, the user to fall back on when the connection names none, and the project's
/// connection type colours by type id.
pub(crate) struct MapContext<'a> {
    pub(crate) root: &'a Path,
    pub(crate) default_user: Option<&'a str>,
    pub(crate) connection_types: &'a BTreeMap<String, String>,
}

/// A draft with its folder, what changed on the way in report order, and whether the
/// saved password may be stored (the `auth-model` is absent or `native`).
#[derive(Debug)]
pub(crate) struct Mapped {
    pub(crate) draft: ConnectionDraft,
    pub(crate) warnings: Vec<WarningKind>,
    pub(crate) keep_secret: bool,
}

/// Allowlist: postgresql/postgres-jdbc, postgresql/postgresql, generic/postgresql;
/// sqlite/sqlite_jdbc, generic/sqlite_{jdbc,xerial,zentus,ch_werner};
/// mysql/{mysql8,mysql5,mysql3,mariaDB}.
/// Anything else is UnsupportedDriver("<provider>/<driver>").
pub(crate) fn detect_engine(provider: &str, driver: &str) -> Result<Engine, SkipReason> {
    match (provider, driver) {
        ("postgresql", "postgres-jdbc" | "postgresql") | ("generic", "postgresql") => {
            Ok(Engine::Postgres)
        }
        ("sqlite", "sqlite_jdbc")
        | ("generic", "sqlite_jdbc" | "sqlite_xerial" | "sqlite_zentus" | "sqlite_ch_werner") => {
            Ok(Engine::Sqlite)
        }
        ("mysql", "mysql8" | "mysql5" | "mysql3" | "mariaDB") => Ok(Engine::Mysql),
        _ => Err(SkipReason::UnsupportedDriver(format!(
            "{provider}/{driver}"
        ))),
    }
}

/// Maps a connection to a draft, or says why it cannot be imported. Skips are checked in
/// order: engine, SSH tunnel, proxy, then a `${...}` placeholder in a field that is used.
pub(crate) fn map_connection(raw: &RawConnection, ctx: &MapContext) -> Result<Mapped, SkipReason> {
    let engine = detect_engine(&raw.provider, &raw.driver)?;
    if handler(raw, "ssh_tunnel").is_some() {
        return Err(SkipReason::SshTunnel);
    }
    if handler(raw, "socks_proxy").is_some() {
        return Err(SkipReason::Proxy);
    }
    let folder = raw
        .folder
        .as_deref()
        .and_then(|f| clean_folder(std::iter::once(f)));
    let mut mapped = match engine {
        Engine::Postgres => map_postgres(raw, ctx)?,
        Engine::Sqlite => map_sqlite(raw, ctx)?,
        Engine::Mysql => map_mysql(raw, ctx)?,
        Engine::Mssql => return Err(SkipReason::UnsupportedDriver("mssql".to_owned())),
        Engine::Duckdb => return Err(SkipReason::UnsupportedDriver("duckdb".to_owned())),
        Engine::Mongodb => return Err(SkipReason::UnsupportedDriver("mongodb".to_owned())),
        Engine::Surrealdb => return Err(SkipReason::UnsupportedDriver("surrealdb".to_owned())),
        Engine::Redis => return Err(SkipReason::UnsupportedDriver("redis".to_owned())),
    };
    mapped.draft = mapped.draft.with_folder(folder);
    match color(raw, ctx.connection_types) {
        Ok(tag) => mapped.draft = mapped.draft.with_color(tag),
        Err(w) => mapped.warnings.push(w),
    }
    Ok(mapped)
}

/// The connection's tag. The built-in `prod` type is red whatever its colours; otherwise the
/// connection's own colour wins, then its type's colour from the file, then DBeaver's built-in
/// default. A value that is not `R,G,B`, or maps to no safe tag, is `ColorUnknown`.
fn color(
    raw: &RawConnection,
    types: &BTreeMap<String, String>,
) -> Result<Option<Color>, WarningKind> {
    let kind = raw.config.connection_type.as_deref().map(str::trim);
    if kind == Some("prod") {
        return Ok(Some(Color::Red));
    }
    let builtin = |id: &str| match id {
        "dev" => Some("255,255,255"),
        "test" => Some("214,250,207"),
        _ => None,
    };
    let type_color =
        kind.and_then(|k| non_blank(types.get(k).map(String::as_str)).or_else(|| builtin(k)));
    let Some(value) = non_blank(raw.config.color.as_deref()).or(type_color) else {
        return Ok(None);
    };
    match rgb(value).map(|(r, g, b)| from_rgb(r, g, b)) {
        Some(ColorMatch::Tag(c)) => Ok(Some(c)),
        Some(ColorMatch::Blank) => Ok(None),
        _ => Err(WarningKind::ColorUnknown(value.to_owned())),
    }
}

/// DBeaver's `R,G,B` decimal colour string.
fn rgb(value: &str) -> Option<(u8, u8, u8)> {
    let mut parts = value.split(',').map(|p| p.trim().parse::<u8>().ok());
    match (parts.next()??, parts.next()??, parts.next()??, parts.next()) {
        (r, g, b, None) => Some((r, g, b)),
        _ => None,
    }
}

/// The connection's URL in URL mode, checked for placeholders, or None in MANUAL mode.
/// A stale URL is ignored in MANUAL mode, and fields of the other mode are never read.
fn url_text(raw: &RawConnection) -> Result<Option<&str>, SkipReason> {
    if !url_mode(raw) {
        return Ok(None);
    }
    let url = raw.config.url.as_deref().map(str::trim).unwrap_or("");
    no_macro(url)?;
    if url.is_empty() {
        return Err(SkipReason::MissingUrl);
    }
    Ok(Some(url))
}

/// MANUAL mode's host, port and database fields, trimmed and checked for placeholders.
fn manual_fields(raw: &RawConnection) -> Result<(String, Option<u16>, String), SkipReason> {
    let cfg = &raw.config;
    let field = |f: &Option<String>| f.as_deref().unwrap_or("").trim().to_owned();
    let (host, port, database) = (field(&cfg.host), field(&cfg.port), field(&cfg.database));
    for f in [&host, &port, &database] {
        no_macro(f)?;
    }
    Ok((host, pg::parse_port(&port)?, database))
}

fn properties(raw: &RawConnection) -> impl Iterator<Item = (String, String)> + '_ {
    raw.config
        .properties
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
}

/// An `auth-model` other than `native`, whose saved password must not be stored.
fn foreign_auth(raw: &RawConnection) -> Option<&str> {
    raw.config
        .auth_model
        .as_deref()
        .filter(|a| !a.trim().is_empty() && *a != "native")
}

fn map_postgres(raw: &RawConnection, ctx: &MapContext) -> Result<Mapped, SkipReason> {
    let (host, port, database, params) = match url_text(raw)? {
        Some(url) => {
            let url = pg::parse_url(url)?;
            let mut params = url.params;
            params.extend(properties(raw));
            (url.host, url.port, url.database, params)
        }
        None => {
            let (host, port, database) = manual_fields(raw)?;
            (Some(host), port, database, properties(raw).collect())
        }
    };

    // DBeaver's SSL handler beats any `sslmode` property; enabled without a mode it verifies.
    let ssl = handler(raw, "postgre_ssl");
    let tls_first = ssl.map(|h| {
        non_blank(h.properties.get("sslMode").map(String::as_str))
            .map_or(SourceTls::VerifyFull, pg::parse_mode)
    });
    let path = |new: &str, legacy: &str| {
        ssl.and_then(|h| {
            non_blank(h.properties.get(new).map(String::as_str))
                .or_else(|| non_blank(h.properties.get(legacy).map(String::as_str)))
        })
    };
    let (pg, mut warnings) = pg::build(pg::PgInput {
        host,
        port,
        database,
        params,
        user: raw.user.as_deref(),
        default_user: ctx.default_user,
        tls_first,
        tls_fallback: None,
        tls_ca: path("ssl.ca.cert", "rootCert"),
        tls_cert: path("ssl.client.cert", "clientCert"),
        tls_key: path("ssl.client.key", "clientKey"),
        secret: SecretSetting::Keychain,
    })?;

    let auth = foreign_auth(raw);
    if let Some(a) = auth {
        warnings.push(WarningKind::AuthModel(a.to_owned()));
    }
    Ok(Mapped {
        draft: ConnectionDraft::postgres(pg, raw.read_only),
        warnings,
        keep_secret: auth.is_none(),
    })
}

fn map_mysql(raw: &RawConnection, ctx: &MapContext) -> Result<Mapped, SkipReason> {
    let (host, port, database, params) = match url_text(raw)? {
        Some(url) => {
            let url = mysql::parse_url(url)?;
            let mut params = url.params;
            params.extend(properties(raw));
            (url.host, url.port, url.database, params)
        }
        None => {
            let (host, port, database) = manual_fields(raw)?;
            (Some(host), port, Some(database), properties(raw).collect())
        }
    };
    let driver = if raw.driver == "mariaDB" {
        Driver::Mariadb
    } else {
        Driver::Mysql
    };

    // DBeaver's SSL handler only fills in when no property names a mode; without one the
    // connection is plain, whatever the driver's own default.
    let ssl = handler(raw, "mysql_ssl");
    let prop = |name: &str| ssl.and_then(|h| non_blank(h.properties.get(name).map(String::as_str)));
    let on = |name: &str| prop(name).is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
    let tls_fallback = Some(match (ssl, driver) {
        (None, _) => SourceTls::Disable,
        (Some(_), Driver::Mysql) if on("ssl.verify.server") => SourceTls::VerifyCa,
        (Some(_), Driver::Mysql) if on("ssl.require") => SourceTls::Require,
        (Some(_), Driver::Mysql) => SourceTls::Prefer,
        (Some(_), Driver::Mariadb) if on("ssl.verify.server") => SourceTls::VerifyFull,
        (Some(_), Driver::Mariadb) => SourceTls::Require,
    });
    let (my, mut warnings) = mysql::build(mysql::MysqlInput {
        host,
        port,
        database,
        params,
        user: raw.user.as_deref(),
        default_user: ctx.default_user,
        tls_first: None,
        tls_fallback,
        driver,
        tls_ca: prop("ssl.ca.cert"),
        tls_cert: prop("ssl.client.cert"),
        tls_key: prop("ssl.client.key"),
        secret: SecretSetting::Keychain,
    })?;

    let auth = foreign_auth(raw);
    if let Some(a) = auth {
        warnings.push(WarningKind::AuthModel(a.to_owned()));
    }
    Ok(Mapped {
        draft: ConnectionDraft::mysql(my, raw.read_only),
        warnings,
        keep_secret: auth.is_none(),
    })
}

fn map_sqlite(raw: &RawConnection, ctx: &MapContext) -> Result<Mapped, SkipReason> {
    let cfg = &raw.config;
    let database = cfg.database.as_deref().unwrap_or("").trim();
    // As DBeaver's generic provider: URL mode, or MANUAL with a blank database, reads the URL.
    let raw_path = if url_mode(raw) || database.is_empty() {
        let url = cfg.url.as_deref().unwrap_or("").trim();
        url.strip_prefix("jdbc:sqlite:").unwrap_or(url)
    } else {
        database
    };
    no_macro(raw_path)?;
    let (raw_path, query) = raw_path.split_once('?').unwrap_or((raw_path, ""));

    let names: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| p.split_once('=').map_or(p, |(k, _)| k))
        .chain(cfg.properties.keys().map(String::as_str))
        .collect();
    let (draft, warnings) = map_path(raw_path, &names, ctx.root, raw.read_only)?;
    Ok(Mapped {
        draft,
        warnings,
        keep_secret: true,
    })
}

/// The named network handler, when it is enabled.
fn handler<'a>(raw: &'a RawConnection, name: &str) -> Option<&'a RawHandler> {
    raw.config.handlers.get(name).filter(|h| h.enabled)
}

fn url_mode(raw: &RawConnection) -> bool {
    raw.config
        .configuration_type
        .as_deref()
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("URL"))
}

fn non_blank(s: Option<&str>) -> Option<&str> {
    s.filter(|s| !s.trim().is_empty())
}

/// DBeaver expands `${...}` variables at connect time, which an import cannot, so a field
/// holding one is `UnknownMacro`. The detail is `${name}` only for a closed placeholder with a
/// plain name, else the bare `${`, so no other field text (perhaps a password) reaches a report.
fn no_macro(field: &str) -> Result<(), SkipReason> {
    let Some(start) = field.find("${") else {
        return Ok(());
    };
    let after = &field[start + 2..];
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');
    let detail = match after.split_once('}') {
        Some((name, _)) if !name.is_empty() && name.chars().all(plain) => format!("${{{name}}}"),
        _ => "${".to_owned(),
    };
    Err(SkipReason::UnknownMacro(detail))
}
