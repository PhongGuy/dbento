//! The SourceTls to TlsMode rule and the certificate path checks, shared by the Postgres and
//! MySQL draft builders so both engines import TLS the same way.

use crate::TlsMode;
use crate::import::model::{SourceTls, WarningKind};
use crate::workspace::is_local_host;
use std::path::Path;

#[cfg(test)]
mod tests;

/// The TLS fields of a draft: the mode and the certificate paths that survived.
#[derive(Debug)]
pub(crate) struct TlsFields {
    pub(crate) tls: TlsMode,
    pub(crate) ca: Option<String>,
    pub(crate) cert: Option<String>,
    pub(crate) key: Option<String>,
}

/// Maps the source mode to Off or Verify and keeps a certificate path only under Verify when
/// it is absolute and exists. Pushes, in order: the TlsWeaker/TlsStricter warning,
/// CaCertMissing, ClientCertMissing.
pub(crate) fn resolve(
    from: SourceTls,
    host: &str,
    ca: Option<&str>,
    cert: Option<&str>,
    key: Option<&str>,
    warnings: &mut Vec<WarningKind>,
) -> TlsFields {
    let tls = match from {
        SourceTls::Disable => TlsMode::Off,
        SourceTls::VerifyFull => TlsMode::Verify,
        SourceTls::Allow | SourceTls::Prefer if is_local_host(host) => {
            warnings.push(WarningKind::TlsWeaker { from });
            TlsMode::Off
        }
        SourceTls::Allow
        | SourceTls::Prefer
        | SourceTls::Require
        | SourceTls::VerifyCa
        | SourceTls::Unrecognised => {
            warnings.push(WarningKind::TlsStricter { from });
            TlsMode::Verify
        }
    };

    let non_blank = |s: Option<&str>| s.filter(|s| !s.trim().is_empty()).map(str::to_owned);
    let usable =
        |p: &str| tls == TlsMode::Verify && Path::new(p).is_absolute() && Path::new(p).exists();
    let ca = match non_blank(ca) {
        Some(p) if usable(&p) => Some(p),
        Some(_) => {
            warnings.push(WarningKind::CaCertMissing);
            None
        }
        None => None,
    };
    // A cert without its key is useless, so the pair is kept whole or dropped whole.
    let (cert, key) = match (non_blank(cert), non_blank(key)) {
        (Some(cert), Some(key)) if usable(&cert) && usable(&key) => (Some(cert), Some(key)),
        (None, None) => (None, None),
        _ => {
            warnings.push(WarningKind::ClientCertMissing);
            (None, None)
        }
    };
    TlsFields { tls, ca, cert, key }
}
