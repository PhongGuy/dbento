//! The `uri` key's rules: no password in the text, plus its user and default database.
//!
//! These rules need no driver; the driver's own syntax check runs in the core at connect.

use percent_encoding::percent_decode_str;

pub(crate) const PASSWORD_REFUSED: &str = "remove the password from `uri`; Dbento keeps it in the \
     keychain. If it holds none, write any `@` after the host as `%40`";

const NOT_UTF8: &str = "`uri` holds percent-encoding that is not UTF-8";

/// The fixed refusal of a port outside 1-65535; it never quotes the uri.
pub const BAD_PORT: &str =
    "`uri` is not a MongoDB connection string: the port must be an integer between 1 and 65535";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UriInfo {
    pub(crate) user: Option<String>,
    pub(crate) database: String,
}

/// Why a `uri` was refused, each holding the message, which never quotes the uri.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UriFault {
    /// A password or secret option is written in the uri.
    Secret(String),
    /// Any other fault: the scheme, percent-encoding or a port.
    Invalid(String),
}

/// Checks a `dbento.toml` `uri` and reads its user and default database.
pub(crate) fn check(uri: &str) -> Result<UriInfo, UriFault> {
    let after_scheme = uri
        .strip_prefix("mongodb://")
        .or_else(|| uri.strip_prefix("mongodb+srv://"))
        .ok_or_else(|| {
            UriFault::Invalid("`uri` must start with mongodb:// or mongodb+srv://".to_owned())
        })?;

    // A password can hold `/` and `?`, so no cut at either is safe: any `:` before the last `@`
    // is refused. That also refuses a port before an `@` in options, which `%40` fixes.
    if after_scheme
        .rsplit_once('@')
        .is_some_and(|(userinfo, _)| userinfo.contains(':'))
    {
        return Err(UriFault::Secret(PASSWORD_REFUSED.to_owned()));
    }

    let (before_options, options) = after_scheme.split_once('?').unwrap_or((after_scheme, ""));
    if let Some(key) = secret_option(options) {
        return Err(UriFault::Secret(format!(
            "remove `{key}` from `uri`: it holds a secret"
        )));
    }

    // Read in the driver's order: options off at the first `?`, then userinfo at the last `@`.
    let (userinfo, hosts_and_path) = before_options
        .rsplit_once('@')
        .unwrap_or(("", before_options));
    let (hosts, path) = hosts_and_path
        .split_once('/')
        .unwrap_or((hosts_and_path, ""));
    // A port the driver would refuse is refused here in fixed words: its own message quotes the
    // text, and `u:secret/` reads to it as a host and port.
    if !hosts.split(',').all(port_in_range) {
        return Err(UriFault::Invalid(BAD_PORT.to_owned()));
    }
    let user = decoded(userinfo).map_err(UriFault::Invalid)?;
    let database = decoded(path).map_err(UriFault::Invalid)?;
    Ok(UriInfo {
        user: (!user.is_empty()).then_some(user),
        database: if database.is_empty() {
            "test".to_owned()
        } else {
            database
        },
    })
}

// A host's port, when written, is 1-65535 in ASCII digits. An IPv6 host keeps its port after
// `]:`; an unclosed `[` is left to the driver.
fn port_in_range(host: &str) -> bool {
    let port = if host.starts_with('[') {
        host.split_once(']')
            .and_then(|(_, rest)| rest.strip_prefix(':'))
    } else {
        host.rsplit_once(':').map(|(_, port)| port)
    };
    port.is_none_or(|p| {
        (1..=5).contains(&p.len())
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u32>().is_ok_and(|n| (1..=65_535).contains(&n))
    })
}

fn decoded(text: &str) -> Result<String, String> {
    match percent_decode_str(text).decode_utf8() {
        Ok(decoded) => Ok(decoded.into_owned()),
        Err(_) => Err(NOT_UTF8.to_owned()),
    }
}

// The first option holding a secret, by its canonical key. Keys and values are read the way the
// driver reads them: a key lower-cased, a value percent-decoded.
fn secret_option(options: &str) -> Option<&'static str> {
    options.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        match key.to_lowercase().as_str() {
            "tlscertificatekeyfilepassword" => Some("tlsCertificateKeyFilePassword"),
            "proxypassword" => Some("proxyPassword"),
            "authmechanismproperties"
                if percent_decode_str(value)
                    .decode_utf8_lossy()
                    .contains("AWS_SESSION_TOKEN") =>
            {
                Some("authMechanismProperties")
            }
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests;
