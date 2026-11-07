//! Maps one raw Beekeeper Studio connection to a connection draft. Postgres and MySQL go
//! through the shared `import::pg` and `import::mysql` rules; SQLite takes its path from
//! `defaultDatabase`.

use super::db::RawConnection;
use crate::import::model::{ConnectionDraft, SecretSetting, SkipReason, SourceTls, WarningKind};
use crate::import::mysql::{self, Driver};
use crate::import::pg;
use crate::import::sqlite::map_path;
use crate::{Color, Engine};
use std::path::Path;

#[cfg(test)]
mod tests;

/// The data folder (root of relative SQLite paths) and the user for a connection naming none.
pub(crate) struct MapContext<'a> {
    pub(crate) root: &'a Path,
    pub(crate) default_user: Option<&'a str>,
}

/// A draft with its folder, the warnings in report order, and whether the saved password
/// may be stored (Postgres without IAM or Azure auth, or MySQL).
#[derive(Debug)]
pub(crate) struct Mapped {
    pub(crate) draft: ConnectionDraft,
    pub(crate) warnings: Vec<WarningKind>,
    pub(crate) keep_secret: bool,
}

/// `postgresql`, `psql`, `postgres` are Postgres; `mysql` and `mariadb` are MySQL; `sqlite` is
/// SQLite (exact match); anything else is UnsupportedDriver(<type>), and None is
/// UnsupportedDriver("unknown").
pub(crate) fn engine(connection_type: Option<&str>) -> Result<Engine, SkipReason> {
    match connection_type {
        Some("postgresql" | "psql" | "postgres") => Ok(Engine::Postgres),
        Some("mysql" | "mariadb") => Ok(Engine::Mysql),
        Some("sqlite") => Ok(Engine::Sqlite),
        other => Err(SkipReason::UnsupportedDriver(
            other.unwrap_or("unknown").to_owned(),
        )),
    }
}

/// Skips in order CloudWorkspace (workspace_id != -1), UnsupportedDriver, SshTunnel, then maps;
/// MySQL further skips a socket, IAM and Azure.
pub(crate) fn map_connection(raw: &RawConnection, ctx: &MapContext) -> Result<Mapped, SkipReason> {
    if raw.workspace_id != -1 {
        return Err(SkipReason::CloudWorkspace);
    }
    let engine = engine(raw.connection_type.as_deref())?;
    if raw.ssh_enabled {
        return Err(SkipReason::SshTunnel);
    }
    let mut mapped = match engine {
        Engine::Postgres => map_postgres(raw, ctx)?,
        Engine::Sqlite => {
            let path = raw.default_database.as_deref().unwrap_or("");
            let (draft, warnings) = map_path(path, &[], ctx.root, raw.read_only)?;
            Mapped {
                draft,
                warnings,
                keep_secret: false,
            }
        }
        Engine::Mysql => map_mysql(raw, ctx)?,
        Engine::Mssql => return Err(SkipReason::UnsupportedDriver("mssql".to_owned())),
        Engine::Duckdb => return Err(SkipReason::UnsupportedDriver("duckdb".to_owned())),
        Engine::Mongodb => return Err(SkipReason::UnsupportedDriver("mongodb".to_owned())),
        Engine::Surrealdb => return Err(SkipReason::UnsupportedDriver("surrealdb".to_owned())),
        Engine::Redis => return Err(SkipReason::UnsupportedDriver("redis".to_owned())),
    };
    mapped.draft = mapped.draft.with_folder(raw.folder.clone());
    match color(raw.label_color.as_deref()) {
        Ok(tag) => mapped.draft = mapped.draft.with_color(tag),
        Err(w) => mapped.warnings.push(w),
    }
    Ok(mapped)
}

/// The tag for a `labelColor`: Beekeeper's palette names map to the same words and `pink`
/// to red; `default`, empty or none is no tag, and any other value is `ColorUnknown`.
fn color(label: Option<&str>) -> Result<Option<Color>, WarningKind> {
    let tag = match label.unwrap_or("") {
        "" | "default" => None,
        "red" | "pink" => Some(Color::Red),
        "orange" => Some(Color::Orange),
        "yellow" => Some(Color::Yellow),
        "green" => Some(Color::Green),
        "blue" => Some(Color::Blue),
        "purple" => Some(Color::Purple),
        other => return Err(WarningKind::ColorUnknown(other.to_owned())),
    };
    Ok(tag)
}

/// The saved port; 0 or one outside u16 is InvalidUrl.
fn port(raw: &RawConnection) -> Result<Option<u16>, SkipReason> {
    match raw.port {
        None => Ok(None),
        Some(p) => match u16::try_from(p) {
            Ok(p) if p != 0 => Ok(Some(p)),
            _ => Err(SkipReason::InvalidUrl),
        },
    }
}

/// The source TLS mode and the CA, cert and key paths (only when `ssl`). Without `ssl` the
/// connection is plain; cert files make Beekeeper verify as `verify` unless told not to.
fn tls_source(raw: &RawConnection, verify: SourceTls) -> (SourceTls, Option<[Option<&str>; 3]>) {
    let files = [&raw.ssl_ca_file, &raw.ssl_cert_file, &raw.ssl_key_file].map(|f| f.as_deref());
    let any_file = files
        .iter()
        .any(|f| f.is_some_and(|f| !f.trim().is_empty()));
    let tls = match (raw.ssl, any_file) {
        (false, _) => SourceTls::Disable,
        (true, true) if raw.ssl_reject_unauthorized => verify,
        (true, _) => SourceTls::Require,
    };
    (tls, raw.ssl.then_some(files))
}

fn map_postgres(raw: &RawConnection, ctx: &MapContext) -> Result<Mapped, SkipReason> {
    let port = port(raw)?;
    // A socket replaces the host and has no TLS, so Beekeeper's SSL fields are ignored then.
    let (host, tls_first, files) = if raw.socket_path_enabled {
        let socket = raw
            .socket_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("/var/run/postgresql");
        (Some(socket.to_owned()), SourceTls::Disable, None)
    } else {
        let host = raw.host.as_deref().map(str::trim).map(str::to_owned);
        let (tls, files) = tls_source(raw, SourceTls::VerifyFull);
        (host, tls, files)
    };
    let file = |i: usize| files.and_then(|f| f[i]);
    let (pg, mut warnings) = pg::build(pg::PgInput {
        host,
        port,
        database: raw.default_database.clone().unwrap_or_default(),
        params: Vec::new(),
        user: raw.username.as_deref(),
        default_user: ctx.default_user,
        tls_first: Some(tls_first),
        tls_fallback: None,
        tls_ca: file(0),
        tls_cert: file(1),
        tls_key: file(2),
        secret: SecretSetting::Keychain,
    })?;

    // The saved password is not what IAM or Azure sign in with, so it is never stored then.
    let auth = if raw.iam_enabled {
        Some("aws_iam")
    } else if raw.azure_enabled {
        Some("azure")
    } else {
        None
    };
    if let Some(a) = auth {
        warnings.push(WarningKind::AuthModel(a.to_owned()));
    }
    Ok(Mapped {
        draft: ConnectionDraft::postgres(pg, raw.read_only),
        warnings,
        keep_secret: auth.is_none(),
    })
}

/// MySQL and MariaDB. A socket, IAM or Azure sign-in has no Dbento equivalent, so each is
/// skipped; Beekeeper's MySQL client checks the CA but not the host name, hence VerifyCa.
fn map_mysql(raw: &RawConnection, ctx: &MapContext) -> Result<Mapped, SkipReason> {
    if raw.socket_path_enabled {
        return Err(SkipReason::UnixSocket);
    }
    if raw.iam_enabled {
        return Err(SkipReason::UnsupportedDriver("aws_iam".to_owned()));
    }
    if raw.azure_enabled {
        return Err(SkipReason::UnsupportedDriver("azure".to_owned()));
    }
    let (tls_first, files) = tls_source(raw, SourceTls::VerifyCa);
    let file = |i: usize| files.and_then(|f| f[i]);
    let driver = if raw.connection_type.as_deref() == Some("mariadb") {
        Driver::Mariadb
    } else {
        Driver::Mysql
    };
    let (my, warnings) = mysql::build(mysql::MysqlInput {
        host: raw.host.as_deref().map(str::trim).map(str::to_owned),
        port: port(raw)?,
        database: raw.default_database.clone(),
        params: Vec::new(),
        user: raw.username.as_deref(),
        default_user: ctx.default_user,
        tls_first: Some(tls_first),
        tls_fallback: None,
        driver,
        tls_ca: file(0),
        tls_cert: file(1),
        tls_key: file(2),
        secret: SecretSetting::Keychain,
    })?;
    Ok(Mapped {
        draft: ConnectionDraft::mysql(my, raw.read_only),
        warnings,
        keep_secret: true,
    })
}
