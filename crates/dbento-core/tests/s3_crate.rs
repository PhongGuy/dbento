//! What Dbento relies on in aws-sdk-s3 1.158.0, checked without a server.
#![cfg(feature = "s3")]
use aws_sdk_s3::config::{
    BehaviorVersion, Credentials, Region, RequestChecksumCalculation, ResponseChecksumValidation,
};
use aws_smithy_http_client::tls::{self, rustls_provider::CryptoMode};

// The knobs the S3 client sets: third-party stores get path style and checksums only when
// required; AWS keeps its default checksums. Directory-bucket session auth is always off.
fn client(endpoint: Option<&str>) -> aws_sdk_s3::Client {
    let http = aws_smithy_http_client::Builder::new()
        .tls_provider(tls::Provider::Rustls(CryptoMode::Ring))
        .build_https();
    let mut conf = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::v2026_11_10())
        .region(Region::new("auto"))
        .credentials_provider(Credentials::new("AKIDTEST", "secret", None, None, "dbento"))
        .disable_s3_express_session_auth(true)
        .http_client(http);
    if let Some(endpoint) = endpoint {
        conf = conf
            .endpoint_url(endpoint)
            .force_path_style(true)
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .response_checksum_validation(ResponseChecksumValidation::WhenRequired);
    } else {
        conf = conf
            .request_checksum_calculation(RequestChecksumCalculation::WhenSupported)
            .response_checksum_validation(ResponseChecksumValidation::WhenSupported);
    }
    aws_sdk_s3::Client::from_conf(conf.build())
}

#[test]
fn a_client_builds_outside_a_runtime_with_the_third_party_store_knobs() {
    let c = client(Some("http://127.0.0.1:5000"));
    let conf = c.config();
    assert_eq!(conf.region().map(Region::as_ref), Some("auto"));
    assert_eq!(
        conf.request_checksum_calculation(),
        Some(&RequestChecksumCalculation::WhenRequired)
    );
    assert_eq!(
        conf.response_checksum_validation(),
        Some(&ResponseChecksumValidation::WhenRequired)
    );
    let aws = client(None);
    assert_eq!(
        aws.config().request_checksum_calculation(),
        Some(&RequestChecksumCalculation::WhenSupported)
    );
}

#[test]
fn the_four_read_calls_take_the_inputs_the_driver_sends() {
    let c = client(None);
    let _ = c.list_buckets();
    let list = c
        .list_objects_v2()
        .bucket("b")
        .prefix("logs/")
        .delimiter("/")
        .continuation_token("t")
        .max_keys(1000);
    assert_eq!(list.get_delimiter().as_deref(), Some("/"));
    let head = c.head_object().bucket("b").key("k");
    assert_eq!(head.get_key().as_deref(), Some("k"));
    let get = c.get_object().bucket("b").key("k").range("bytes=0-1048575");
    assert_eq!(get.get_range().as_deref(), Some("bytes=0-1048575"));
}
