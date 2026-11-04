//! Maps one raw DataGrip data source to a connection draft. A password, wherever the source
//! keeps it, is dropped with a warning, and every change in TLS strictness carries one.

use super::xml::{RawLocal, RawSource};
use crate::import::color::{ColorMatch, from_rgb};
use crate::import::model::{ConnectionDraft, SecretSetting, SkipReason, WarningKind};
use crate::import::mysql::{self, Driver};
use crate::import::pg;
use crate::import::plan::is_windows_path;
use crate::import::sqlite::map_path;
use crate::{Color, Engine};
use percent_encoding::percent_decode_str;
use std::collections::HashSet;
use std::path::Path;

#[cfg(test)]
mod tests;

/// What mapping needs from outside the source: the project root for `$PROJECT_DIR$`, the
/// home folder for `$USER_HOME$`, and the user to fall back on when the source names none.
pub(crate) struct MapContext<'a> {
    pub(crate) root: &'a Path,
    pub(crate) home: Option<&'a Path>,
    pub(crate) default_user: Option<&'a str>,
}

/// A draft and what changed or was dropped on the way, in report order.
#[derive(Debug)]
pub(crate) struct Mapped {
    pub(crate) draft: ConnectionDraft,
    pub(crate) warnings: Vec<WarningKind>,
}

/// Decides the engine from `driver-ref`, then the URL scheme, then the driver class. The
/// MySQL-wire forks are refused first, since their URLs and classes look like MySQL's.
pub(crate) fn detect_engine(src: &RawSource) -> Result<Engine, SkipReason> {
    match src.driver_ref.as_deref() {
        Some(d @ ("mysql_aurora_aws" | "mysql_aurora" | "tidb")) => {
            return Err(SkipReason::UnsupportedDriver(d.to_owned()));
        }
        Some("mysql.8" | "mysql" | "mariadb") => return Ok(Engine::Mysql),
        Some("postgresql" | "postgres") => return Ok(Engine::Postgres),
        Some("sqlite.xerial" | "sqlite") => return Ok(Engine::Sqlite),
        _ => {}
    }
    let url = src.url.as_deref().unwrap_or("").trim();
    let url = url.strip_prefix("jdbc:").unwrap_or(url);
    if url.starts_with("postgresql:") {
        return Ok(Engine::Postgres);
    }
    if url.starts_with("sqlite:") {
        return Ok(Engine::Sqlite);
    }
    if url.starts_with("mysql:aws:") {
        return Err(SkipReason::UnsupportedDriver("mysql:aws".to_owned()));
    }
    if url.starts_with("mysql:") || url.starts_with("mariadb:") {
        return Ok(Engine::Mysql);
    }
    match src.jdbc_driver.as_deref() {
        Some("org.postgresql.Driver") => Ok(Engine::Postgres),
        Some("com.mysql.cj.jdbc.Driver" | "com.mysql.jdbc.Driver" | "org.mariadb.jdbc.Driver") => {
            Ok(Engine::Mysql)
        }
        Some("org.sqlite.JDBC") => Ok(Engine::Sqlite),
        _ => Err(SkipReason::UnsupportedDriver(
            src.driver_ref
                .clone()
                .or_else(|| src.jdbc_driver.clone())
                .unwrap_or_else(|| "unknown".to_owned()),
        )),
    }
}

/// Maps a source and its local settings to a draft, or says why it cannot be imported.
pub(crate) fn map_source(
    src: &RawSource,
    local: Option<&RawLocal>,
    ctx: &MapContext,
) -> Result<Mapped, SkipReason> {
    if local.is_some_and(|l| l.ssh_enabled) {
        return Err(SkipReason::SshTunnel);
    }
    let url = match src.url.as_deref().map(str::trim) {
        Some(u) if !u.is_empty() => u,
        _ => return Err(SkipReason::MissingUrl),
    };
    match detect_engine(src)? {
        Engine::Postgres => map_postgres(src, url, local, ctx),
        Engine::Sqlite => map_sqlite(src, url, ctx),
        Engine::Mysql => map_mysql(src, url, local, ctx),
        Engine::Mssql => Err(SkipReason::UnsupportedDriver("mssql".to_owned())),
        Engine::Duckdb => Err(SkipReason::UnsupportedDriver("duckdb".to_owned())),
        Engine::Mongodb => Err(SkipReason::UnsupportedDriver("mongodb".to_owned())),
        Engine::Surrealdb => Err(SkipReason::UnsupportedDriver("surrealdb".to_owned())),
    }
}

/// The tag for a `databaseColors.xml` value: a palette name in any case, or 6-digit hex with
/// an optional `#`. DataGrip has no production marker, so Red and Rose are only a colour
/// choice. An empty value is no tag; any other value with no safe tag is `ColorUnknown`.
pub(crate) fn color(value: &str) -> Result<Option<Color>, WarningKind> {
    let tag = match value.trim().to_ascii_lowercase().as_str() {
        "red" | "rose" => Color::Red,
        "orange" => Color::Orange,
        "yellow" => Color::Yellow,
        "green" | "lime" => Color::Green,
        "aqua" | "teal" => Color::Teal,
        "blue" => Color::Blue,
        "violet" | "purple" => Color::Purple,
        "gray" | "grey" => Color::Gray,
        "" | "no" | "inherit" => return Ok(None),
        other => match hex(other).map(|(r, g, b)| from_rgb(r, g, b)) {
            Some(ColorMatch::Tag(c)) => c,
            Some(ColorMatch::Blank) => return Ok(None),
            _ => return Err(WarningKind::ColorUnknown(value.to_owned())),
        },
    };
    Ok(Some(tag))
}

/// A `RRGGBB` hex colour, `#` optional.
fn hex(value: &str) -> Option<(u8, u8, u8)> {
    let digits = value.strip_prefix('#').unwrap_or(value);
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

pub(crate) use crate::folder::is_invisible;

/// The base of a Dbento name: `[a-z0-9_-]` only, no leading, trailing or doubled `-`.
pub(crate) fn base_name(source_name: &str) -> String {
    let mut out = String::new();
    for c in source_name.chars().filter(|&c| !is_invisible(c)) {
        let c = if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '-'
        };
        if !(c == '-' && out.ends_with('-')) {
            out.push(c.to_ascii_lowercase());
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "datasource".to_owned()
    } else {
        out.to_owned()
    }
}

/// Takes `base`, else the first free `base-2`, `base-3`, ..., and marks it used.
pub(crate) fn assign_name(base: &str, used: &mut HashSet<String>) -> String {
    let mut name = base.to_owned();
    let mut n = 2;
    while used.contains(&name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    used.insert(name.clone());
    name
}

fn map_postgres(
    src: &RawSource,
    url: &str,
    local: Option<&RawLocal>,
    ctx: &MapContext,
) -> Result<Mapped, SkipReason> {
    let url = pg::parse_url(url)?;
    // URL parameters win over driver properties of the same name, as in pgjdbc; the
    // enabled `ssl-config` only fills in when neither names a mode.
    let mut params = url.params;
    params.extend(src.properties.iter().cloned());
    let ssl = local
        .and_then(|l| l.ssl.as_ref())
        .filter(|s| s.enabled != Some(false));
    let no_auth = local.and_then(|l| l.auth_provider.as_deref()) == Some("no-auth");
    let (pg, warnings) = pg::build(pg::PgInput {
        host: url.host,
        port: url.port,
        database: url.database,
        params,
        user: local.and_then(|l| l.user_name.as_deref()),
        default_user: ctx.default_user,
        tls_first: None,
        tls_fallback: ssl
            .and_then(|s| s.mode.as_deref())
            .filter(|m| !m.trim().is_empty())
            .map(pg::parse_mode),
        tls_ca: ssl.and_then(|s| s.ca_cert.as_deref()),
        tls_cert: ssl.and_then(|s| s.client_cert.as_deref()),
        tls_key: ssl.and_then(|s| s.client_key.as_deref()),
        secret: if no_auth {
            SecretSetting::None
        } else {
            SecretSetting::Keychain
        },
    })?;
    Ok(Mapped {
        draft: ConnectionDraft::postgres(pg, src.read_only),
        warnings,
    })
}

fn map_mysql(
    src: &RawSource,
    url: &str,
    local: Option<&RawLocal>,
    ctx: &MapContext,
) -> Result<Mapped, SkipReason> {
    // The driver family follows the same order as detection: driver-ref, scheme, class.
    let scheme = url.strip_prefix("jdbc:").unwrap_or(url);
    let driver = match src.driver_ref.as_deref() {
        Some("mariadb") => Driver::Mariadb,
        Some("mysql.8" | "mysql") => Driver::Mysql,
        _ if scheme.starts_with("mariadb:")
            || src.jdbc_driver.as_deref() == Some("org.mariadb.jdbc.Driver") =>
        {
            Driver::Mariadb
        }
        _ => Driver::Mysql,
    };
    let url = mysql::parse_url(url)?;
    // As for Postgres, URL parameters beat driver properties and the enabled `ssl-config`
    // fills in only when neither names a mode.
    let mut params = url.params;
    params.extend(src.properties.iter().cloned());
    let ssl = local
        .and_then(|l| l.ssl.as_ref())
        .filter(|s| s.enabled != Some(false));
    let no_auth = local.and_then(|l| l.auth_provider.as_deref()) == Some("no-auth");
    let (my, warnings) = mysql::build(mysql::MysqlInput {
        host: url.host,
        port: url.port,
        database: url.database,
        params,
        user: local.and_then(|l| l.user_name.as_deref()),
        default_user: ctx.default_user,
        tls_first: None,
        tls_fallback: ssl
            .and_then(|s| s.mode.as_deref())
            .filter(|m| !m.trim().is_empty())
            .map(mysql::parse_mode),
        driver,
        tls_ca: ssl.and_then(|s| s.ca_cert.as_deref()),
        tls_cert: ssl.and_then(|s| s.client_cert.as_deref()),
        tls_key: ssl.and_then(|s| s.client_key.as_deref()),
        secret: if no_auth {
            SecretSetting::None
        } else {
            SecretSetting::Keychain
        },
    })?;
    Ok(Mapped {
        draft: ConnectionDraft::mysql(my, src.read_only),
        warnings,
    })
}

fn map_sqlite(src: &RawSource, url: &str, ctx: &MapContext) -> Result<Mapped, SkipReason> {
    let url = url.strip_prefix("jdbc:").unwrap_or(url);
    let url = url.strip_prefix("sqlite:").unwrap_or(url);
    let (raw_path, query) = url.split_once('?').unwrap_or((url, ""));
    let (raw_path, uri) = match raw_path.strip_prefix("file:") {
        Some(p) => (file_uri_path(p), true),
        None => (raw_path.to_owned(), false),
    };
    let raw_path = raw_path.as_str();

    let url_names = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| p.split_once('=').map_or(p, |(k, _)| k));
    let names: Vec<&str> = url_names
        .chain(src.properties.iter().map(|(k, _)| k.as_str()))
        .collect();

    let memory_uri = uri && query.split('&').any(|p| p == "mode=memory");
    if raw_path.is_empty()
        || raw_path == ":memory:"
        || raw_path.starts_with(":resource:")
        || memory_uri
    {
        return Err(SkipReason::InMemory);
    }
    // A Windows path cannot be resolved here, so it skips macro expansion and goes verbatim.
    let (path, base) = if is_windows_path(raw_path) {
        (raw_path.to_owned(), ctx.root.to_path_buf())
    } else {
        let (expanded, mut unknown) = expand_macros(raw_path, ctx);
        let base = if Path::new(&expanded).is_absolute() {
            ctx.root.to_path_buf()
        } else {
            match src.working_dir.as_deref().filter(|w| !w.trim().is_empty()) {
                Some(wd) => {
                    let (wd, wd_unknown) = expand_macros(wd, ctx);
                    unknown = wd_unknown.or(unknown);
                    ctx.root.join(wd)
                }
                None => ctx.root.to_path_buf(),
            }
        };
        // A `$NAME$` left unexpanded is a real filename only if that file exists.
        if let Some(name) = unknown
            && !base.join(&expanded).exists()
        {
            return Err(SkipReason::UnknownMacro(name));
        }
        (expanded, base)
    };
    let (draft, warnings) = map_path(&path, &names, &base, src.read_only)?;
    Ok(Mapped { draft, warnings })
}

/// The path of a SQLite `file:` URI: an empty or `localhost` authority drops and `%HH`
/// escapes decode. Any other authority is kept, so the path reads as missing.
fn file_uri_path(rest: &str) -> String {
    let path = match rest.strip_prefix("//") {
        Some(after) => {
            let auth = after.find('/').unwrap_or(after.len());
            match &after[..auth] {
                "" | "localhost" => &after[auth..],
                _ => rest,
            }
        }
        None => rest,
    };
    percent_decode_str(path).decode_utf8_lossy().into_owned()
}

/// Replaces `$PROJECT_DIR$`, `$ProjectFileDir$` and `$USER_HOME$`. Any other `$NAME$`, or the
/// home macro with no home folder, stays as written and the first is returned, since a `$`
/// may belong to the filename itself.
fn expand_macros(s: &str, ctx: &MapContext) -> (String, Option<String>) {
    let mut out = String::new();
    let mut unknown = None;
    let mut rest = s;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let len = after.find('$').unwrap_or(0);
        let name = &after[..len];
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            // Not a macro: keep this `$` and look for one from the next character.
            out.push('$');
            rest = after;
            continue;
        }
        match (name, ctx.home) {
            ("PROJECT_DIR" | "ProjectFileDir", _) => out.push_str(&ctx.root.to_string_lossy()),
            ("USER_HOME", Some(home)) => out.push_str(&home.to_string_lossy()),
            _ => {
                unknown.get_or_insert_with(|| format!("${name}$"));
                out.push_str(&rest[start..start + len + 2]);
            }
        }
        rest = &after[len + 1..];
    }
    out.push_str(rest);
    (out, unknown)
}
