//! S3 error replies as https://docs.aws.amazon.com/AmazonS3/latest/API/ErrorResponses.html
//! describes them, classified into the failures the session reports.

use crate::ExecuteError;
use crate::aws::replay::{RawReplay as Replay, RawReply as Reply};
use crate::s3::errors::{S3Failure, classify, execute_error};
use crate::s3::replay::{client, s3_error};

fn c(r: &Replay) -> crate::s3::client::S3Client {
    client(r, Some("http://replay.test"), "eu-west-1", false)
}

#[tokio::test]
async fn a_permanent_redirect_is_wrong_region_and_never_followed() {
    let r = Replay::new(vec![
        Reply::status(301)
            .header("x-amz-bucket-region", "eu-central-1")
            .header("location", "http://elsewhere.test/b")
            .xml(&s3_error(
                "PermanentRedirect",
                "The bucket you are attempting to access must be addressed using the specified endpoint.",
            )),
    ]);
    let e = classify(c(&r).list_buckets().await.unwrap_err());
    assert!(matches!(e, S3Failure::WrongRegion { region: Some(ref x) } if x == "eu-central-1"));
    assert_eq!(r.seen().len(), 1);
}

#[tokio::test]
async fn a_bodiless_head_redirect_or_miss_still_classifies() {
    let r = Replay::new(vec![
        Reply::status(301).header("x-amz-bucket-region", "us-west-2"),
        Reply::status(404),
    ]);
    assert!(matches!(
        classify(c(&r).head_object("b", "k").await.unwrap_err()),
        S3Failure::WrongRegion { region: Some(_) }
    ));
    assert!(matches!(
        classify(c(&r).head_object("b", "k").await.unwrap_err()),
        S3Failure::NotFound { .. }
    ));
}

#[tokio::test]
async fn a_bodiless_head_403_is_access_denied() {
    let r = Replay::new(vec![Reply::status(403)]);
    assert!(matches!(
        classify(c(&r).head_object("b", "k").await.unwrap_err()),
        S3Failure::AccessDenied { .. }
    ));
}

#[tokio::test]
async fn a_bodiless_head_400_hints_at_expired_credentials() {
    let r = Replay::new(vec![Reply::status(400)]);
    let e = classify(c(&r).head_object("b", "k").await.unwrap_err());
    assert!(
        matches!(e, S3Failure::Service { ref code, ref message } if code == "HTTP 400" && message.contains("expired")),
        "{e:?}"
    );
}

#[tokio::test]
async fn codes_map_to_their_failures() {
    let r = Replay::new(vec![
        Reply::status(403).xml(&s3_error("AccessDenied", "Access Denied")),
        Reply::status(404).xml(&s3_error("NoSuchKey", "The specified key does not exist.")),
        Reply::status(403).xml(&s3_error("InvalidAccessKeyId", "no such key id")),
        Reply::status(400).xml(&s3_error("ExpiredToken", "expired")),
        Reply::status(416).xml(&s3_error(
            "InvalidRange",
            "The requested range is not satisfiable",
        )),
        Reply::dispatch_timeout(),
    ]);
    assert!(matches!(
        classify(c(&r).list_buckets().await.unwrap_err()),
        S3Failure::AccessDenied { .. }
    ));
    assert!(
        matches!(classify(c(&r).get_range("b", "k", 4).await.unwrap_err()), S3Failure::NotFound { ref code, .. } if code == "NoSuchKey")
    );
    assert!(
        matches!(classify(c(&r).list_buckets().await.unwrap_err()), S3Failure::Auth { ref code, .. } if code == "InvalidAccessKeyId")
    );
    assert!(
        matches!(classify(c(&r).list_buckets().await.unwrap_err()), S3Failure::Auth { ref code, .. } if code == "ExpiredToken")
    );
    assert!(matches!(
        classify(c(&r).get_range("b", "k", 4).await.unwrap_err()),
        S3Failure::InvalidRange
    ));
    assert!(matches!(
        classify(c(&r).list_buckets().await.unwrap_err()),
        S3Failure::Timeout
    ));
}

#[test]
fn execute_errors_carry_the_code_and_the_region_hint() {
    match execute_error(S3Failure::WrongRegion {
        region: Some("eu-central-1".into()),
    }) {
        ExecuteError::Database { code, hint, .. } => {
            assert_eq!(code, "PermanentRedirect");
            assert!(hint.unwrap().contains("region = \"eu-central-1\""));
        }
        other => panic!("{other:?}"),
    }
    match execute_error(S3Failure::NotFound {
        code: "NoSuchKey".into(),
        message: "gone".into(),
    }) {
        ExecuteError::Database { code, message, .. } => {
            assert_eq!((code.as_str(), message.as_str()), ("NoSuchKey", "gone"))
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        execute_error(S3Failure::Timeout),
        ExecuteError::Timeout
    ));
}
