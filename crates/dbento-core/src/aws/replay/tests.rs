use std::time::{Duration, Instant};

use aws_smithy_runtime_api::client::http::HttpConnector;
use aws_smithy_runtime_api::client::orchestrator::HttpRequest;
use aws_smithy_types::body::SdkBody;

use super::{RawReplay, RawReply};

fn request(method: &str, uri: &str, body: &str) -> HttpRequest {
    let mut r = HttpRequest::new(SdkBody::from(body));
    r.set_method(method).unwrap();
    r.set_uri(uri).unwrap();
    r.headers_mut().insert("x-test", "1");
    r
}

#[tokio::test]
async fn requests_are_recorded_and_replies_come_in_order_with_their_headers() {
    let replay = RawReplay::new(vec![
        RawReply::status(200)
            .header("x-amz-bucket-region", "eu-central-1")
            .body("first"),
        RawReply::status(404).xml("<Error/>"),
    ]);
    let one = replay
        .call(request("GET", "http://replay.test/b?list-type=2", ""))
        .await
        .unwrap();
    let two = replay
        .call(request("PUT", "http://replay.test/b/k", "data"))
        .await
        .unwrap();
    assert_eq!(
        (
            one.status().as_u16(),
            one.headers().get("x-amz-bucket-region")
        ),
        (200, Some("eu-central-1"))
    );
    assert_eq!(one.body().bytes(), Some(&b"first"[..]));
    assert_eq!(
        (two.status().as_u16(), two.headers().get("content-type")),
        (404, Some("application/xml"))
    );
    let seen = replay.seen();
    assert_eq!(
        (seen[0].method.as_str(), seen[0].uri.as_str()),
        ("GET", "http://replay.test/b?list-type=2")
    );
    assert_eq!(
        (seen[1].body.as_slice(), seen[1].header("X-Test")),
        (&b"data"[..], Some("1"))
    );
}

#[tokio::test]
async fn a_dispatch_timeout_and_running_out_are_connector_errors() {
    let replay = RawReplay::new(vec![RawReply::dispatch_timeout()]);
    assert!(
        replay
            .call(request("GET", "http://replay.test/", ""))
            .await
            .unwrap_err()
            .is_timeout()
    );
    assert!(
        replay
            .call(request("GET", "http://replay.test/", ""))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_delayed_reply_waits_before_answering() {
    let replay = RawReplay::new(vec![
        RawReply::status(200).after(Duration::from_millis(150)),
    ]);
    let start = Instant::now();
    replay
        .call(request("GET", "http://replay.test/", ""))
        .await
        .unwrap();
    assert!(start.elapsed() >= Duration::from_millis(150));
}
