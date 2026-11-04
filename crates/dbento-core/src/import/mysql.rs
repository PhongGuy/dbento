//! The Connector/J and MariaDB URL parser and the MySQL draft builder shared by the importers.
//! Each importer reads its own source format into a `MysqlInput`; the mapping rules live here.

use crate::import::model::{MysqlDraft, SecretSetting, SkipReason, SourceTls, WarningKind};
use crate::import::pg::{decode, parse_authority};
use crate::import::tls;

#[cfg(test)]
mod tests;

/// A MySQL or MariaDB URL split up: values decoded, names as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MysqlUrl {
    pub(crate) host: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) database: Option<String>,
    /// Userinfo first (`user`, then `("password", "")`), then the query, values decoded.
    pub(crate) params: Vec<(String, String)>,
}

/// Parses `jdbc:mysql://...` or `jdbc:mariadb://...`. Host lists, HA prefixes and
/// `mysql+srv:` are `MultiHost`; any other scheme, form or bad part is `InvalidUrl`.
pub(crate) fn parse_url(url: &str) -> Result<MysqlUrl, SkipReason> {
    let url = url.strip_prefix("jdbc:").unwrap_or(url);
    if url.starts_with("mysql+srv:") {
        return Err(SkipReason::MultiHost);
    }
    let rest = url
        .strip_prefix("mysql:")
        .or_else(|| url.strip_prefix("mariadb:"))
        .ok_or(SkipReason::InvalidUrl)?;
    const HA: [&str; 4] = [
        "loadbalance:",
        "replication:",
        "sequential:",
        "load-balance-read:",
    ];
    if HA.iter().any(|p| rest.starts_with(p)) {
        return Err(SkipReason::MultiHost);
    }
    let rest = rest.strip_prefix("//").ok_or(SkipReason::InvalidUrl)?;
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, database) = rest.split_once('/').unwrap_or((rest, ""));
    if database.contains('/') {
        return Err(SkipReason::InvalidUrl);
    }

    // The password is only noted, never held, so it cannot leak through the parsed URL.
    let mut params = Vec::new();
    let host_part = match authority.rsplit_once('@') {
        Some((userinfo, host_part)) => {
            let (user, password) = match userinfo.split_once(':') {
                Some((user, _)) => (user, true),
                None => (userinfo, false),
            };
            if !user.is_empty() {
                params.push(("user".to_owned(), decode(user)?));
            }
            if password {
                params.push(("password".to_owned(), String::new()));
            }
            host_part
        }
        None => authority,
    };
    // `address=(...)` and `(key=value)` host forms are not supported.
    if host_part.contains(['(', '=']) {
        return Err(SkipReason::InvalidUrl);
    }
    let (host, port) = parse_authority(host_part)?;
    let database = Some(decode(database)?).filter(|d| !d.is_empty());
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        params.push((k.to_owned(), decode(v)?));
    }
    Ok(MysqlUrl {
        host,
        port,
        database,
        params,
    })
}

/// MySQL `sslMode` and MariaDB `sslMode` values, plus DataGrip's `<mode>`; any case, `_` as `-`.
pub(crate) fn parse_mode(mode: &str) -> SourceTls {
    match mode.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "disable" | "disabled" | "0" | "false" => SourceTls::Disable,
        "prefer" | "preferred" => SourceTls::Prefer,
        "allow" => SourceTls::Allow,
        "require" | "required" | "trust" => SourceTls::Require,
        "verify-ca" => SourceTls::VerifyCa,
        "verify-full" | "verify-identity" | "1" | "true" => SourceTls::VerifyFull,
        _ => SourceTls::Unrecognised,
    }
}

/// The JDBC driver family, which sets the TLS default and the legacy-property rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Driver {
    Mysql,
    Mariadb,
}

impl Driver {
    /// Connector/J prefers TLS when nothing is set; MariaDB 3.x disables it.
    pub(crate) fn default_tls(self) -> SourceTls {
        match self {
            Driver::Mysql => SourceTls::Prefer,
            Driver::Mariadb => SourceTls::Disable,
        }
    }
}

/// One MySQL or MariaDB source in neutral form, as an importer read it.
pub(crate) struct MysqlInput<'a> {
    /// None or blank is localhost.
    pub(crate) host: Option<String>,
    pub(crate) port: Option<u16>,
    /// None or blank is no database.
    pub(crate) database: Option<String>,
    /// URL parameters first, then driver properties; the first value of a name wins.
    pub(crate) params: Vec<(String, String)>,
    /// The source's own saved user, which beats a `user` parameter.
    pub(crate) user: Option<&'a str>,
    pub(crate) default_user: Option<&'a str>,
    /// A mode that beats the parameters.
    pub(crate) tls_first: Option<SourceTls>,
    /// A mode used only when the parameters give none.
    pub(crate) tls_fallback: Option<SourceTls>,
    pub(crate) driver: Driver,
    pub(crate) tls_ca: Option<&'a str>,
    pub(crate) tls_cert: Option<&'a str>,
    pub(crate) tls_key: Option<&'a str>,
    pub(crate) secret: SecretSetting,
}

/// Parameter names Dbento understands, matched case-insensitively; others warn as dropped.
const KNOWN: [&str; 12] = [
    "user",
    "password",
    "sslMode",
    "useSSL",
    "useSsl",
    "requireSSL",
    "verifyServerCertificate",
    "trustServerCertificate",
    "disableSslHostnameVerification",
    "socket",
    "localSocket",
    "pipe",
];

/// The parameters kept, one value per name in any case, looked up case-insensitively.
struct Params(Vec<(String, String)>);

impl Params {
    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn truthy(&self, name: &str) -> bool {
        self.get(name).is_some_and(|v| {
            ["", "1", "true", "yes"]
                .iter()
                .any(|t| v.eq_ignore_ascii_case(t))
        })
    }
}

/// Warnings in order: parameters, then those of `tls::resolve`. Errors: UnixSocket, NoUser.
pub(crate) fn build(input: MysqlInput) -> Result<(MysqlDraft, Vec<WarningKind>), SkipReason> {
    // Each name, in any case, is considered once, so a password or an unknown name warns once.
    let mut params = Params(Vec::new());
    let mut warnings = Vec::new();
    for (name, value) in input.params {
        if params.get(&name).is_some() {
            continue;
        }
        if name.eq_ignore_ascii_case("password") {
            warnings.push(WarningKind::PasswordInUrl);
        } else if !KNOWN.iter().any(|k| k.eq_ignore_ascii_case(&name)) {
            warnings.push(WarningKind::DroppedParameter(name.clone()));
        }
        params.0.push((name, value));
    }

    let non_blank = |s: Option<&str>| s.filter(|s| !s.trim().is_empty()).map(str::to_owned);
    let host = non_blank(input.host.as_deref()).unwrap_or_else(|| "localhost".to_owned());
    // MariaDB's `localSocket` and `pipe` are socket connections too; ignoring them would
    // silently point the draft at a TCP server instead.
    let socket_param = |n| params.get(n).is_some_and(|v| !v.trim().is_empty());
    if host.starts_with('/')
        || params.get("socket").is_some()
        || socket_param("localSocket")
        || socket_param("pipe")
    {
        return Err(SkipReason::UnixSocket);
    }
    let user = non_blank(input.user)
        .or_else(|| non_blank(params.get("user")))
        .or_else(|| non_blank(input.default_user))
        .ok_or(SkipReason::NoUser)?;
    let database = non_blank(input.database.as_deref());

    let from = input
        .tls_first
        .or_else(|| params.get("sslMode").map(parse_mode))
        .or_else(|| legacy_tls(&params, input.driver))
        .or(input.tls_fallback)
        .unwrap_or(input.driver.default_tls());
    let tls = tls::resolve(
        from,
        &host,
        input.tls_ca,
        input.tls_cert,
        input.tls_key,
        &mut warnings,
    );

    let my = MysqlDraft {
        host,
        port: input.port,
        database,
        user,
        secret: input.secret,
        tls: Some(tls.tls),
        tls_ca: tls.ca,
        tls_cert: tls.cert,
        tls_key: tls.key,
    };
    Ok((my, warnings))
}

/// The mode the pre-`sslMode` driver properties give, or None when they say nothing.
fn legacy_tls(params: &Params, driver: Driver) -> Option<SourceTls> {
    match driver {
        Driver::Mysql => {
            params.get("useSSL")?;
            Some(if !params.truthy("useSSL") {
                SourceTls::Disable
            } else if params.truthy("verifyServerCertificate") {
                SourceTls::VerifyCa
            } else if params.truthy("requireSSL") {
                SourceTls::Require
            } else {
                SourceTls::Prefer
            })
        }
        Driver::Mariadb => params.truthy("useSSL").then(|| {
            if params.truthy("trustServerCertificate") {
                SourceTls::Require
            } else if params.truthy("disableSslHostnameVerification") {
                SourceTls::VerifyCa
            } else {
                SourceTls::VerifyFull
            }
        }),
    }
}
