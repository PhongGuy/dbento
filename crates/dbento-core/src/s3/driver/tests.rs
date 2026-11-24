use crate::ConnectError;
use crate::aws::replay::{RawReplay as Replay, RawReply as Reply};
use crate::s3::replay::{s3_error, session};

use super::text_or_bytes;

#[tokio::test]
async fn list_buckets_denied_still_connects_and_ls_names_the_buckets_key() {
    let denied = || Reply::status(403).xml(&s3_error("AccessDenied", "Access Denied"));
    let r = Replay::new(vec![denied(), denied()]);
    let mut s = session(&r, None).await.unwrap();
    let e = super::tests_support::run_one(&mut s, "ls")
        .await
        .unwrap_err();
    match e {
        crate::ExecuteError::Database { code, hint, .. } => {
            assert_eq!(code, "AccessDenied");
            assert!(hint.unwrap().contains("buckets"));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_rejected_key_refuses_the_connect_with_a_hint() {
    let r = Replay::new(vec![Reply::status(403).xml(&s3_error(
        "SignatureDoesNotMatch",
        "The request signature we calculated does not match",
    ))]);
    match session(&r, None).await {
        Err(ConnectError::Open { message }) => {
            assert!(message.contains("access_key_id"), "{message}");
        }
        other => panic!("{:?}", other.map(|_| ())),
    }
}

#[tokio::test]
async fn a_scoped_connection_pings_its_first_bucket_not_list_buckets() {
    let r = Replay::new(vec![Reply::status(200).xml(&crate::s3::replay::list_page(
        &[],
        &[],
        None,
        true,
    ))]);
    session(&r, Some(&["logs", "media"])).await.unwrap();
    let uri = &r.seen()[0].uri;
    assert!(
        uri.starts_with("http://replay.test/logs/?") && uri.contains("max-keys=1"),
        "{uri}"
    );
}

#[test]
fn valid_utf8_is_text_and_a_cut_mid_character_drops_only_the_tail() {
    assert_eq!(
        text_or_bytes(b"hello".to_vec(), false),
        Ok("hello".to_owned())
    );
    let cut = "日本".as_bytes()[..4].to_vec(); // 日 plus one byte of 本
    assert_eq!(text_or_bytes(cut.clone(), true), Ok("日".to_owned()));
    assert_eq!(text_or_bytes(cut.clone(), false), Err(cut));
    assert_eq!(text_or_bytes(vec![0xff, b'a'], true), Err(vec![0xff, b'a']));
}
