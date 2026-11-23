//! HTTP plumbing shared by the AWS engines.

use aws_smithy_http_client::tls::{self, rustls_provider::CryptoMode};
use aws_smithy_runtime_api::client::http::SharedHttpClient;

/// An HTTPS client on ring rustls, the one every real connection uses.
pub(crate) fn https_client() -> SharedHttpClient {
    aws_smithy_http_client::Builder::new()
        .tls_provider(tls::Provider::Rustls(CryptoMode::Ring))
        .build_https()
}

/// The WHATWG-normalised origin of an endpoint, or `None` when it is not a URL with a host.
pub(crate) fn endpoint_origin(text: &str) -> Option<String> {
    let origin = url::Url::parse(text).ok()?.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

#[cfg(test)]
mod tests;
