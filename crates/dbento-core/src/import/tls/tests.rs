//! The SourceTls to TlsMode rule and the certificate path checks shared by Postgres and MySQL.
use super::*;
use crate::TlsMode;
use crate::import::model::{SourceTls, WarningKind};

fn mode(from: SourceTls, host: &str) -> (TlsMode, Vec<WarningKind>) {
    let mut w = Vec::new();
    let f = resolve(from, host, None, None, None, &mut w);
    (f.tls, w)
}

#[test]
fn each_source_mode_maps_to_off_or_verify_with_its_warning() {
    use SourceTls::*;
    let (on, off) = (TlsMode::Verify, TlsMode::Off);
    for (from, host, want, warn) in [
        (Disable, "db.example", off, None),
        (VerifyFull, "localhost", on, None),
        (
            Allow,
            "localhost",
            off,
            Some(WarningKind::TlsWeaker { from: Allow }),
        ),
        (
            Prefer,
            "127.0.0.1",
            off,
            Some(WarningKind::TlsWeaker { from: Prefer }),
        ),
        (
            Prefer,
            "db.example",
            on,
            Some(WarningKind::TlsStricter { from: Prefer }),
        ),
        (
            Require,
            "localhost",
            on,
            Some(WarningKind::TlsStricter { from: Require }),
        ),
        (
            VerifyCa,
            "db.example",
            on,
            Some(WarningKind::TlsStricter { from: VerifyCa }),
        ),
        (
            Unrecognised,
            "db.example",
            on,
            Some(WarningKind::TlsStricter { from: Unrecognised }),
        ),
    ] {
        let want_w: Vec<WarningKind> = warn.into_iter().collect();
        assert_eq!(mode(from, host), (want, want_w), "{from:?} {host}");
    }
}

#[test]
fn certificate_paths_survive_only_under_verify_when_absolute_and_present() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = |n: &str| {
        let p = dir.path().join(n);
        std::fs::write(&p, "x").unwrap();
        p.to_str().unwrap().to_owned()
    };
    let (ca, cert, key) = (file("ca.pem"), file("c.pem"), file("c.key"));
    let mut w = Vec::new();
    let f = resolve(
        SourceTls::VerifyFull,
        "db.example",
        Some(&ca),
        Some(&cert),
        Some(&key),
        &mut w,
    );
    assert_eq!(
        (f.ca.as_deref(), f.cert.as_deref(), f.key.as_deref()),
        (Some(ca.as_str()), Some(cert.as_str()), Some(key.as_str()))
    );
    assert!(w.is_empty(), "{w:?}");
    // Off keeps no path and says so.
    let mut w = Vec::new();
    let f = resolve(
        SourceTls::Disable,
        "db.example",
        Some(&ca),
        Some(&cert),
        Some(&key),
        &mut w,
    );
    assert_eq!((f.ca, f.cert, f.key), (None, None, None));
    assert_eq!(
        w,
        [WarningKind::CaCertMissing, WarningKind::ClientCertMissing]
    );
    // A relative CA, or a cert without its key, is dropped.
    let mut w = Vec::new();
    let f = resolve(
        SourceTls::VerifyFull,
        "db.example",
        Some("relative/ca.pem"),
        Some(&cert),
        None,
        &mut w,
    );
    assert_eq!((f.ca, f.cert, f.key), (None, None, None));
    assert_eq!(
        w,
        [WarningKind::CaCertMissing, WarningKind::ClientCertMissing]
    );
    // Blank paths are no paths.
    let mut w = Vec::new();
    resolve(
        SourceTls::VerifyFull,
        "db.example",
        Some("  "),
        Some(""),
        Some(" "),
        &mut w,
    );
    assert!(w.is_empty(), "{w:?}");
}
