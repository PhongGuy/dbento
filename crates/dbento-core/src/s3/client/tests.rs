use aws_sdk_s3::config::{RequestChecksumCalculation, ResponseChecksumValidation};

use crate::aws::replay::{RawReplay as Replay, RawReply as Reply};
use crate::s3::ObjectKey;
use crate::s3::client::{ClientSettings, ListCall, S3Client};
use crate::s3::replay::{client, list_page, s3_error, settings};

fn call(bucket: &str, prefix: &str) -> ListCall {
    ListCall {
        bucket: bucket.into(),
        prefix: prefix.into(),
        delimiter: Some('/'),
        token: None,
        max_keys: 1000,
    }
}

/// Path style with an endpoint: https://docs.aws.amazon.com/AmazonS3/latest/userguide/VirtualHosting.html
#[tokio::test]
async fn an_endpoint_is_path_style_and_sends_url_encoding() {
    let replay = Replay::new(vec![Reply::status(200).xml(&list_page(
        &[],
        &[],
        None,
        true,
    ))]);
    client(&replay, Some("http://replay.test"), "eu-west-1", false)
        .list_objects(&call("logs", "a/"))
        .await
        .unwrap();
    let uri = &replay.seen()[0].uri;
    assert!(uri.starts_with("http://replay.test/logs/?"), "{uri}");
    for part in [
        "list-type=2",
        "encoding-type=url",
        "delimiter=%2F",
        "prefix=a%2F",
        "max-keys=1000",
    ] {
        assert!(uri.contains(part), "{part}: {uri}");
    }
}

/// Virtual-hosted addressing: https://docs.aws.amazon.com/AmazonS3/latest/userguide/VirtualHosting.html
#[tokio::test]
async fn virtual_hosted_moves_the_bucket_into_the_host() {
    let replay = Replay::new(vec![Reply::status(200).xml(&list_page(
        &[],
        &[],
        None,
        true,
    ))]);
    client(&replay, Some("http://replay.test"), "eu-west-1", true)
        .list_objects(&call("logs", ""))
        .await
        .unwrap();
    assert!(
        replay.seen()[0]
            .uri
            .starts_with("http://logs.replay.test/?"),
        "{}",
        replay.seen()[0].uri
    );
}

/// AWS itself: https://docs.aws.amazon.com/AmazonS3/latest/userguide/VirtualHosting.html
#[tokio::test]
async fn aws_without_an_endpoint_is_virtual_hosted_in_the_region() {
    let replay = Replay::new(vec![Reply::status(200).xml(&list_page(
        &[],
        &[],
        None,
        true,
    ))]);
    client(&replay, None, "eu-west-1", false)
        .list_objects(&call("logs", ""))
        .await
        .unwrap();
    let uri = &replay.seen()[0].uri;
    assert!(
        uri.starts_with("https://logs.s3.eu-west-1.amazonaws.com/"),
        "{uri}"
    );
}

/// R2 signs for region `auto`: https://developers.cloudflare.com/r2/api/s3/api/
#[tokio::test]
async fn r2_signs_for_the_auto_region() {
    let replay = Replay::new(vec![
        Reply::status(200).xml(&crate::s3::replay::buckets(&["media"])),
    ]);
    let got = client(
        &replay,
        Some("https://acct.r2.cloudflarestorage.com"),
        "auto",
        false,
    )
    .list_buckets()
    .await
    .unwrap();
    assert_eq!(
        got.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
        ["media"]
    );
    let auth = replay.seen()[0].header("authorization").unwrap().to_owned();
    assert!(auth.contains("/auto/s3/aws4_request"), "{auth}");
}

#[tokio::test]
async fn checksum_mode_is_sent_only_on_aws() {
    let ok = || Reply::status(200).header("content-length", "2").body("hi");
    let replay = Replay::new(vec![ok(), ok()]);
    client(&replay, None, "eu-west-1", false)
        .get_object("b", "k")
        .await
        .unwrap();
    client(&replay, Some("http://replay.test"), "eu-west-1", false)
        .get_object("b", "k")
        .await
        .unwrap();
    let seen = replay.seen();
    assert_eq!(seen[0].header("x-amz-checksum-mode"), Some("ENABLED"));
    assert_eq!(seen[1].header("x-amz-checksum-mode"), None);
}

#[tokio::test]
async fn url_encoded_keys_are_decoded_plus_as_space() {
    let page = list_page(
        &["g/", "pre%01x/"],
        &[
            ("a%2Bb", 1),
            ("c+d", 2),
            ("soh%01x", 3),
            ("bidi%E2%80%AEx", 4),
        ],
        Some("t1"),
        true,
    );
    let replay = Replay::new(vec![Reply::status(200).xml(&page)]);
    let got = client(&replay, Some("http://replay.test"), "eu-west-1", false)
        .list_objects(&call("b", ""))
        .await
        .unwrap();
    assert_eq!(got.prefixes, ["g/", "pre\u{1}x/"]);
    let keys: Vec<&str> = got.objects.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(keys, ["a+b", "c d", "soh\u{1}x", "bidi\u{202e}x"]);
    assert_eq!(got.next_token.as_deref(), Some("t1"));
}

#[tokio::test]
async fn a_key_that_decodes_to_invalid_utf8_is_an_invalid_key_encoding_error() {
    let page = list_page(&[], &[("bad%FFkey", 1)], None, true);
    let replay = Replay::new(vec![Reply::status(200).xml(&page)]);
    let err = client(&replay, Some("http://replay.test"), "eu-west-1", false)
        .list_objects(&call("b", ""))
        .await
        .unwrap_err();
    let service = err.as_service_error().expect("a service error");
    assert_eq!(
        aws_sdk_s3::error::ProvideErrorMetadata::code(service),
        Some("InvalidKeyEncoding")
    );
}

#[tokio::test]
async fn a_reply_without_the_encoding_echo_is_taken_raw() {
    let replay = Replay::new(vec![Reply::status(200).xml(&list_page(
        &[],
        &[("100%25", 1)],
        None,
        false,
    ))]);
    let got = client(&replay, Some("http://replay.test"), "eu-west-1", false)
        .list_objects(&call("b", ""))
        .await
        .unwrap();
    assert_eq!(got.objects[0].key, "100%25");
}

#[tokio::test]
async fn object_metadata_strips_etag_quotes_and_formats_the_time() {
    let replay = Replay::new(vec![Reply::status(200).xml(&list_page(
        &[],
        &[("k", 7)],
        None,
        true,
    ))]);
    let got = client(&replay, Some("http://replay.test"), "eu-west-1", false)
        .list_objects(&call("b", ""))
        .await
        .unwrap();
    let meta = &got.objects[0];
    assert_eq!(
        (meta.size, meta.etag.as_deref()),
        (7, Some("d41d8cd98f00b204e9800998ecf8427e"))
    );
    assert_eq!(meta.last_modified.as_deref(), Some("2026-11-23T21:47:52Z"));
    assert_eq!(meta.storage_class.as_deref(), Some("STANDARD"));
}

#[tokio::test]
async fn a_range_read_takes_the_total_from_content_range() {
    let replay = Replay::new(vec![
        Reply::status(206)
            .header("content-range", "bytes 0-3/3145728")
            .header("content-length", "4")
            .header("content-type", "application/octet-stream")
            .body(vec![1, 2, 3, 4]),
    ]);
    let got = client(&replay, Some("http://replay.test"), "eu-west-1", false)
        .get_range("b", "big", 4)
        .await
        .unwrap();
    assert_eq!((got.bytes, got.total_size), (vec![1, 2, 3, 4], 3_145_728));
    assert_eq!(
        got.content_type.as_deref(),
        Some("application/octet-stream")
    );
    assert_eq!(replay.seen()[0].header("range"), Some("bytes=0-3"));
}

#[tokio::test]
async fn an_error_body_carries_its_code() {
    let replay = Replay::new(vec![
        Reply::status(403).xml(&s3_error("AccessDenied", "Access Denied")),
    ]);
    let err = client(&replay, Some("http://replay.test"), "eu-west-1", false)
        .list_buckets()
        .await
        .unwrap_err();
    let service = err.as_service_error().expect("a service error");
    assert_eq!(
        aws_sdk_s3::error::ProvideErrorMetadata::code(service),
        Some("AccessDenied")
    );
}

#[test]
fn an_s3_url_splits_into_bucket_and_raw_key() {
    assert_eq!(
        ObjectKey::parse("s3://b/a/%41 x"),
        Some(ObjectKey::new("b", "a/%41 x"))
    );
    assert_eq!(ObjectKey::parse("s3://b"), Some(ObjectKey::new("b", "")));
    assert_eq!(ObjectKey::parse("s3://b/"), Some(ObjectKey::new("b", "")));
    assert_eq!(ObjectKey::parse("s3:///k"), None);
    assert_eq!(ObjectKey::parse("http://b/k"), None);
}

// A region no other test uses, so the drain cannot reach their retries even if quotas
// were shared per region.
#[tokio::test(start_paused = true)]
async fn draining_one_clients_retry_quota_leaves_another_clients_retries_alone() {
    let settings = || ClientSettings {
        max_attempts: 3,
        ..settings(Some("http://replay.test"), "xx-quota-s3", false)
    };
    let throttled = || Reply::status(503).xml(&s3_error("SlowDown", "slow down"));

    // Throttle the first client until a call goes out once with no retry: its quota is empty.
    let noisy = Replay::new((0..600).map(|_| throttled()).collect());
    let noisy_client = S3Client::new(settings(), noisy.http());
    let mut drained = false;
    for _ in 0..200 {
        let before = noisy.seen().len();
        assert!(noisy_client.list_buckets().await.is_err(), "throttled");
        if noisy.seen().len() - before == 1 {
            drained = true;
            break;
        }
    }
    assert!(drained, "the first client's retry quota never ran out");

    // A second client in the same region still retries its throttled read and succeeds.
    let quiet = Replay::new(vec![
        throttled(),
        Reply::status(200).xml(&crate::s3::replay::buckets(&["b"])),
    ]);
    let sent = S3Client::new(settings(), quiet.http()).list_buckets().await;
    assert_eq!(quiet.seen().len(), 2, "the throttled read was retried");
    assert!(sent.is_ok(), "{sent:?}");
}

#[test]
fn checksums_are_only_when_required_on_a_custom_endpoint() {
    let replay = Replay::default();
    let custom = client(&replay, Some("http://replay.test"), "eu-west-1", false);
    let aws = client(&replay, None, "eu-west-1", false);
    assert_eq!(
        custom.inner.config().response_checksum_validation(),
        Some(&ResponseChecksumValidation::WhenRequired)
    );
    assert_eq!(
        custom.inner.config().request_checksum_calculation(),
        Some(&RequestChecksumCalculation::WhenRequired)
    );
    assert_eq!(
        aws.inner.config().response_checksum_validation(),
        Some(&ResponseChecksumValidation::WhenSupported)
    );
}
