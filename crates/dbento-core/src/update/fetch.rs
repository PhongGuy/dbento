//! The update client, the manifest check and the artifact download. The client sends only a
//! fixed header set, never decodes bodies, and caps every body on the wire. It is blocking, so
//! callers run it on tokio's blocking pool.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest as _, Sha256};
use ureq::tls::{RootCerts, TlsConfig};
use ureq::{Agent, Body, http::Response};
use url::Url;

use super::{
    Available, CheckError, DownloadError, Format, Staged, Timeouts, UpdateConfig, allowed,
    evaluate, verify_manifest,
};

const MANIFEST_CAP: u64 = 64 * 1024;
const SIGNATURE_CAP: u64 = 4 * 1024;
const MAX_REDIRECTS: u32 = 5;
const CHUNK: usize = 64 * 1024;

/// Fetches the manifest (at most 64 KiB) and the manifest URL plus `.sig` (at most 4 KiB),
/// verifies the signature, then evaluates. `Ok(None)` when no newer stable release exists.
/// A non-success manifest status is [`CheckError::Http`], a non-success signature status is
/// [`CheckError::ManifestSignature`], a body over its cap is [`CheckError::ManifestInvalid`],
/// a time limit is [`CheckError::Timeout`] and any other transport failure is
/// [`CheckError::Offline`].
pub async fn check(cfg: &UpdateConfig) -> Result<Option<Available>, CheckError> {
    let cfg = cfg.clone();
    tokio::task::spawn_blocking(move || check_blocking(&cfg))
        .await
        .map_err(|e| CheckError::Offline(e.to_string()))?
}

fn check_blocking(cfg: &UpdateConfig) -> Result<Option<Available>, CheckError> {
    // Only a loopback override may speak plain http; every other chain is refused at any http hop.
    let https_only = !local_override(&cfg.manifest_url);
    let t = &cfg.timeouts;

    let response = get(&agent(t, t.manifest, https_only), &cfg.manifest_url)?;
    if !response.status().is_success() {
        return Err(CheckError::Http(response.status().as_u16()));
    }
    let manifest = read_capped(response, MANIFEST_CAP, "the manifest")?;

    // The signature sits beside the configured URL, wherever the manifest was redirected to.
    let sig_url = format!("{}.sig", cfg.manifest_url);
    let response = get(&agent(t, t.manifest, https_only), &sig_url)?;
    if !response.status().is_success() {
        return Err(CheckError::ManifestSignature(format!(
            "the signature answered HTTP {}",
            response.status().as_u16()
        )));
    }
    let signature = read_capped(response, SIGNATURE_CAP, "the signature")?;

    // The bytes verified are the bytes parsed; nothing is read from an unverified manifest.
    verify_manifest(&manifest, &signature, &cfg.keys)?;
    evaluate(&manifest, cfg)
}

/// Streams the artifact to `<name>.part` in `staging_dir`, then renames it to `<name>`:
/// `dbento-update-<version>` plus the suffix its installer needs. The `.part` is created
/// exclusively (mode 0600 on Unix) before any request, or the result is
/// [`DownloadError::NotWritable`]. A body longer or shorter than listed is
/// [`DownloadError::SizeMismatch`], a wrong digest [`DownloadError::HashMismatch`], a 404
/// [`DownloadError::ArtifactMissing`] and any other non-success [`DownloadError::Http`]. Every
/// failure removes the `.part`. `progress(received, size)` runs after each chunk is written.
pub async fn download(
    a: &Available,
    staging_dir: &Path,
    progress: impl FnMut(u64, u64) + Send + 'static,
) -> Result<Staged, DownloadError> {
    let a = a.clone();
    let dir = staging_dir.to_path_buf();
    tokio::task::spawn_blocking(move || download_blocking(&a, &dir, progress))
        .await
        .map_err(|e| DownloadError::Offline(e.to_string()))?
}

fn download_blocking(
    a: &Available,
    dir: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<Staged, DownloadError> {
    let name = staged_name(a);
    let path = dir.join(&name);
    let part = dir.join(format!("{name}.part"));
    // A `.part` left by an earlier crash is never resumed: its bytes were never verified.
    let _ = std::fs::remove_file(&part);
    // Create exclusively, so a link planted after the remove is refused rather than followed.
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options
        .open(&part)
        .map_err(|_| DownloadError::NotWritable(dir.to_path_buf()))?;
    let renamed = stream(a, file, &part, progress)
        .and_then(|()| std::fs::rename(&part, &path).map_err(|e| io(&path, &e)));
    if let Err(e) = renamed {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    Ok(Staged {
        path,
        version: a.version.clone(),
    })
}

/// The staged file name; installers recognise an AppImage, app bundle or `.exe` by its suffix.
fn staged_name(a: &Available) -> String {
    let suffix = match a.format {
        Format::AppImage => ".AppImage",
        Format::App => ".app.tar.gz",
        Format::Nsis => ".exe",
        Format::Binary if cfg!(windows) => ".exe",
        Format::Binary => "",
    };
    format!("dbento-update-{}{suffix}", a.version)
}

/// Writes the body into `file`, never past `a.size` bytes, then checks its length and digest.
fn stream(
    a: &Available,
    mut file: File,
    part: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), DownloadError> {
    let https_only = !local_override(&a.url);
    let response = agent(&a.timeouts, a.timeouts.artifact, https_only)
        .get(&a.url)
        .call()
        .map_err(download_transport)?;
    let status = response.status();
    if status.as_u16() == 404 {
        return Err(DownloadError::ArtifactMissing);
    }
    if !status.is_success() {
        return Err(DownloadError::Http(status.as_u16()));
    }
    // ureq's reader has no limit of its own, so this loop is the size cap.
    let mut reader = response.into_body().into_reader();
    let mut buf = vec![0u8; CHUNK];
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            // A body read error wraps the ureq error, so a timeout stays a timeout.
            Err(e) => return Err(download_transport(ureq::Error::from(e))),
        };
        let got = received + n as u64;
        if got > a.size {
            return Err(DownloadError::SizeMismatch {
                expected: a.size,
                got,
            });
        }
        let chunk = buf.get(..n).unwrap_or_default();
        file.write_all(chunk).map_err(|e| io(part, &e))?;
        hasher.update(chunk);
        received = got;
        progress(received, a.size);
    }
    if received != a.size {
        return Err(DownloadError::SizeMismatch {
            expected: a.size,
            got: received,
        });
    }
    file.sync_all().map_err(|e| io(part, &e))?;
    if hex::encode(hasher.finalize()) != a.sha256 {
        return Err(DownloadError::HashMismatch);
    }
    Ok(())
}

fn io(path: &Path, e: &std::io::Error) -> DownloadError {
    DownloadError::Io {
        path: PathBuf::from(path),
        message: e.to_string(),
    }
}

/// The download's twin of [`transport`]: a time limit is `Timeout`, anything else `Offline`.
fn download_transport(e: ureq::Error) -> DownloadError {
    if matches!(e, ureq::Error::Timeout(_)) {
        DownloadError::Timeout
    } else {
        DownloadError::Offline(e.to_string())
    }
}

/// An `http` URL that still passes the URL rule: a loopback override for local testing.
pub(crate) fn local_override(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| u.scheme() == "http" && allowed(&u))
}

/// The updater's agent: a fixed header set, at most five redirects, platform roots, and
/// `total` as the whole request's time limit. Without `https_only` the environment proxy is
/// dropped too, so loopback traffic never leaves the machine.
pub(crate) fn agent(timeouts: &Timeouts, total: Duration, https_only: bool) -> Agent {
    let mut config = Agent::config_builder()
        .http_status_as_error(false)
        .https_only(https_only)
        .max_redirects(MAX_REDIRECTS)
        .user_agent(concat!("dbento/", env!("CARGO_PKG_VERSION")))
        .accept("*/*")
        .accept_encoding("identity")
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_connect(Some(timeouts.connect))
        .timeout_global(Some(total));
    if !https_only {
        config = config.proxy(None);
    }
    Agent::new_with_config(config.build())
}

fn get(agent: &Agent, url: &str) -> Result<Response<Body>, CheckError> {
    agent.get(url).call().map_err(transport)
}

/// Maps a request or body error: a time limit is `Timeout`, anything else (a refused http
/// hop or too many redirects included) means the server could not be reached.
pub(crate) fn transport(e: ureq::Error) -> CheckError {
    if matches!(e, ureq::Error::Timeout(_)) {
        CheckError::Timeout
    } else {
        CheckError::Offline(e.to_string())
    }
}

/// The whole body, refused as soon as it outgrows `cap` bytes.
fn read_capped(response: Response<Body>, cap: u64, what: &str) -> Result<Vec<u8>, CheckError> {
    response
        .into_body()
        .into_with_config()
        .limit(cap)
        .read_to_vec()
        .map_err(|e| {
            if matches!(e, ureq::Error::BodyExceedsLimit(_)) {
                CheckError::ManifestInvalid(format!("{what} is larger than {cap} bytes"))
            } else {
                transport(e)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::{agent, transport};
    use crate::update::{CheckError, Timeouts};
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // The rule, not DNS or a refused connection, stops the request: the server sees nothing.
    #[tokio::test]
    async fn an_https_only_agent_refuses_a_plain_http_hop_before_sending() {
        let s = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200))
            .mount(&s)
            .await;
        let url = format!("{}/latest.json", s.uri());
        let t = Timeouts::default();
        let err = tokio::task::spawn_blocking(move || agent(&t, t.manifest, true).get(&url).call())
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(err, ureq::Error::RequireHttpsOnly(_)), "{err:?}");
        assert!(matches!(transport(err), CheckError::Offline(_)));
        assert!(
            s.received_requests().await.unwrap().is_empty(),
            "the request was sent"
        );
    }
}
