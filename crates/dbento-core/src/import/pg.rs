//! The pgjdbc URL parser and the Postgres draft builder shared by the importers. Each
//! importer reads its own source format into a `PgInput`; the mapping rules live here once.

use crate::import::model::{PgDraft, SecretSetting, SkipReason, SourceTls, WarningKind};
use crate::import::tls;
use percent_encoding::percent_decode_str;
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

/// A pgjdbc URL split up: values percent-decoded (`+` is a space), names as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PgUrl {
    pub(crate) host: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) database: String,
    pub(crate) params: Vec<(String, String)>,
}

/// Parses `jdbc:postgresql:...` or `postgresql:...`. A wrong scheme, any `@`, `//h/a/b`, a
/// bad host char, a bad port or a bad escape is `InvalidUrl`; a host list is `MultiHost`.
pub(crate) fn parse_url(url: &str) -> Result<PgUrl, SkipReason> {
    let url = url.strip_prefix("jdbc:").unwrap_or(url);
    let rest = url
        .strip_prefix("postgresql:")
        .ok_or(SkipReason::InvalidUrl)?;
    // pgjdbc has no userinfo and wants `@` percent-encoded, so a raw one is a password
    // that would otherwise land in a host, database or parameter name after splitting.
    if rest.contains('@') {
        return Err(SkipReason::InvalidUrl);
    }
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, database) = match rest.strip_prefix("//") {
        Some(r) if r.matches('/').count() > 1 => return Err(SkipReason::InvalidUrl),
        Some(r) => r.split_once('/').unwrap_or((r, "")),
        None if rest.starts_with('/') => return Err(SkipReason::InvalidUrl),
        None => ("", rest),
    };
    let (host, port) = parse_authority(authority)?;
    let database = decode(database)?;
    let mut params = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        params.push((k.to_owned(), decode(v)?));
    }
    Ok(PgUrl {
        host,
        port,
        database,
        params,
    })
}

/// "" is None; ASCII digits giving 1..=65535 is Some; anything else is `InvalidUrl`.
pub(crate) fn parse_port(port: &str) -> Result<Option<u16>, SkipReason> {
    if port.is_empty() {
        return Ok(None);
    }
    if !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SkipReason::InvalidUrl);
    }
    match port.parse::<u16>() {
        Ok(0) | Err(_) => Err(SkipReason::InvalidUrl),
        Ok(port) => Ok(Some(port)),
    }
}

/// Reads a TLS mode case-insensitively with `_` as `-` (DataGrip's XML spells `VERIFY_CA`,
/// pgjdbc `verify-ca`); an unknown one is `Unrecognised`.
pub(crate) fn parse_mode(mode: &str) -> SourceTls {
    match mode.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "disable" => SourceTls::Disable,
        "allow" => SourceTls::Allow,
        "prefer" => SourceTls::Prefer,
        "require" => SourceTls::Require,
        "verify-ca" => SourceTls::VerifyCa,
        "verify-full" => SourceTls::VerifyFull,
        _ => SourceTls::Unrecognised,
    }
}

/// One Postgres source in neutral form, as an importer read it.
pub(crate) struct PgInput<'a> {
    /// None or "" means localhost, or `ServiceFile` when a `service` parameter is set.
    pub(crate) host: Option<String>,
    pub(crate) port: Option<u16>,
    /// "" means the user's name.
    pub(crate) database: String,
    /// URL parameters first, then driver properties; the first value of a name wins.
    pub(crate) params: Vec<(String, String)>,
    /// The source's own saved user, which beats a `user` parameter.
    pub(crate) user: Option<&'a str>,
    pub(crate) default_user: Option<&'a str>,
    /// A mode that beats the parameters.
    pub(crate) tls_first: Option<SourceTls>,
    /// A mode used only when the parameters give none.
    pub(crate) tls_fallback: Option<SourceTls>,
    pub(crate) tls_ca: Option<&'a str>,
    pub(crate) tls_cert: Option<&'a str>,
    pub(crate) tls_key: Option<&'a str>,
    pub(crate) secret: SecretSetting,
}

/// Postgres parameters Dbento understands; any other name is dropped with a warning.
const KNOWN: [&str; 6] = [
    "user",
    "password",
    "sslmode",
    "ssl",
    "currentSchema",
    "ApplicationName",
];

/// Builds the draft fields and the warnings, in order: parameters, TLS, `CaCertMissing`,
/// `ClientCertMissing`. Errors with `ServiceFile` or `NoUser` when no draft can be made.
pub(crate) fn build(input: PgInput) -> Result<(PgDraft, Vec<WarningKind>), SkipReason> {
    // Each name is considered once, so a password or an unknown name warns at most once.
    let mut params: BTreeMap<String, String> = BTreeMap::new();
    let mut warnings = Vec::new();
    for (name, value) in input.params {
        if params.contains_key(&name) {
            continue;
        }
        if name == "password" {
            warnings.push(WarningKind::PasswordInUrl);
        } else if !KNOWN.contains(&name.as_str()) {
            warnings.push(WarningKind::DroppedParameter(name.clone()));
        }
        params.insert(name, value);
    }

    let host = match input.host.filter(|h| !h.is_empty()) {
        Some(h) => h,
        None if params.contains_key("service") => return Err(SkipReason::ServiceFile),
        None => "localhost".to_owned(),
    };
    let non_blank = |s: Option<&str>| s.filter(|s| !s.trim().is_empty()).map(str::to_owned);
    let user = non_blank(input.user)
        .or_else(|| non_blank(params.get("user").map(String::as_str)))
        .or_else(|| non_blank(input.default_user))
        .ok_or(SkipReason::NoUser)?;
    let database = if input.database.is_empty() {
        user.clone()
    } else {
        input.database
    };

    let from = input
        .tls_first
        .or_else(|| params_tls(&params))
        .or(input.tls_fallback)
        .unwrap_or(SourceTls::Prefer);
    let tls = tls::resolve(
        from,
        &host,
        input.tls_ca,
        input.tls_cert,
        input.tls_key,
        &mut warnings,
    );

    let mut settings = BTreeMap::new();
    for (param, setting) in [
        ("currentSchema", "search_path"),
        ("ApplicationName", "application_name"),
    ] {
        if let Some(v) = params.get(param) {
            settings.insert(setting.to_owned(), v.clone());
        }
    }
    let pg = PgDraft {
        host,
        port: input.port,
        database,
        user,
        secret: input.secret,
        tls: Some(tls.tls),
        tls_ca: tls.ca,
        tls_cert: tls.cert,
        tls_key: tls.key,
        settings,
    };
    Ok((pg, warnings))
}

/// The mode the parameters give: `sslmode`, else a true or empty `ssl` meaning `verify-full`.
fn params_tls(params: &BTreeMap<String, String>) -> Option<SourceTls> {
    if let Some(mode) = params.get("sslmode") {
        return Some(parse_mode(mode));
    }
    params
        .get("ssl")
        .is_some_and(|v| v.is_empty() || v.eq_ignore_ascii_case("true"))
        .then_some(SourceTls::VerifyFull)
}

/// Splits `host[:port]` or `[v6][:port]`. An empty host is None; a list, a plain host with a
/// non-hostname char or a port that is not a `u16` makes the URL unusable.
pub(crate) fn parse_authority(
    authority: &str,
) -> Result<(Option<String>, Option<u16>), SkipReason> {
    if authority.contains(',') {
        return Err(SkipReason::MultiHost);
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(r) => {
            let (h, after) = r.split_once(']').ok_or(SkipReason::InvalidUrl)?;
            match after {
                "" => (h, None),
                _ => (
                    h,
                    Some(after.strip_prefix(':').ok_or(SkipReason::InvalidUrl)?),
                ),
            }
        }
        None => match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        },
    };
    // pgjdbc never decodes a plain host, so a `%` or other stray char is a mangled URL
    // (often an encoded password) that must not be written out as the host.
    let hostname_char = |c: char| c.is_alphanumeric() || matches!(c, '-' | '.' | '_');
    if !authority.starts_with('[') && !host.chars().all(hostname_char) {
        return Err(SkipReason::InvalidUrl);
    }
    let port = parse_port(port.unwrap_or(""))?;
    let host = (!host.is_empty()).then(|| host.to_owned());
    Ok((host, port))
}

// As java.net.URLDecoder, which pgjdbc uses: `+` is a space, then percent-decoding.
pub(crate) fn decode(s: &str) -> Result<String, SkipReason> {
    percent_decode_str(&s.replace('+', " "))
        .decode_utf8()
        .map(|c| c.into_owned())
        .map_err(|_| SkipReason::InvalidUrl)
}
