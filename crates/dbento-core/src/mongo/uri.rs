//! The `uri` key's rules: no password in the text, plus its user and default database.
//!
//! The password rule needs no crate, so a build without the `mongodb` feature still refuses one.

use percent_encoding::percent_decode_str;

pub(crate) const PASSWORD_REFUSED: &str = "remove the password from `uri`; Dbento keeps it in the \
     keychain. If it holds none, write any `@` after the host as `%40`";

const NOT_UTF8: &str = "`uri` holds percent-encoding that is not UTF-8";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UriInfo {
    pub(crate) user: Option<String>,
    pub(crate) database: String,
}

/// Checks a `dbento.toml` `uri` and reads its user and default database; the error is the message.
pub(crate) fn check(uri: &str) -> Result<UriInfo, String> {
    let after_scheme = uri
        .strip_prefix("mongodb://")
        .or_else(|| uri.strip_prefix("mongodb+srv://"))
        .ok_or("`uri` must start with mongodb:// or mongodb+srv://")?;

    // A password can hold `/` and `?`, so no cut at either is safe: any `:` before the last `@`
    // is refused. That also refuses a port before an `@` in options, which `%40` fixes.
    if after_scheme
        .rsplit_once('@')
        .is_some_and(|(userinfo, _)| userinfo.contains(':'))
    {
        return Err(PASSWORD_REFUSED.to_owned());
    }

    let (before_options, options) = after_scheme.split_once('?').unwrap_or((after_scheme, ""));
    if let Some(key) = secret_option(options) {
        return Err(format!("remove `{key}` from `uri`: it holds a secret"));
    }

    // Read in the driver's order: options off at the first `?`, then userinfo at the last `@`.
    let (userinfo, hosts_and_path) = before_options
        .rsplit_once('@')
        .unwrap_or(("", before_options));
    let (_, path) = hosts_and_path
        .split_once('/')
        .unwrap_or((hosts_and_path, ""));
    let user = decoded(userinfo)?;
    let database = decoded(path)?;

    driver_syntax(uri)?;
    Ok(UriInfo {
        user: (!user.is_empty()).then_some(user),
        database: if database.is_empty() {
            "test".to_owned()
        } else {
            database
        },
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

// Driver messages quote the text, even with no `@` (`u:secret/` reads as a host and port), so only
// the port error passes, in fixed words.
#[cfg(feature = "mongodb")]
fn driver_syntax(uri: &str) -> Result<(), String> {
    use mongodb::error::ErrorKind;
    use mongodb::options::ConnectionString;

    let Err(error) = ConnectionString::parse(uri) else {
        return Ok(());
    };
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "ErrorKind has too many variants to list; the rest share one arm"
    )]
    match *error.kind {
        ErrorKind::InvalidArgument { message, .. }
            if !uri.contains('@') && message.contains("the port must be an integer") =>
        {
            Err(
                "`uri` is not a MongoDB connection string: the port must be an integer between 1 \
                 and 65535"
                    .to_owned(),
            )
        }
        _ => Err(
            "`uri` is not a MongoDB connection string (the driver's reason is hidden \
                  because the text may hold a password)"
                .to_owned(),
        ),
    }
}

#[cfg(not(feature = "mongodb"))]
fn driver_syntax(_uri: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests;
