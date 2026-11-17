//! The update client and the manifest check. The client sends only a fixed header set, never
//! decodes bodies, and caps every body on the wire. It is blocking, so callers run it on
//! tokio's blocking pool.

use std::time::Duration;

use ureq::tls::{RootCerts, TlsConfig};
use ureq::{Agent, Body, http::Response};
use url::Url;

use super::{Available, CheckError, Timeouts, UpdateConfig, allowed, evaluate, verify_manifest};

const MANIFEST_CAP: u64 = 64 * 1024;
const SIGNATURE_CAP: u64 = 4 * 1024;
const MAX_REDIRECTS: u32 = 5;

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

/// An `http` URL that still passes the URL rule: a loopback override for local testing.
fn local_override(url: &str) -> bool {
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
