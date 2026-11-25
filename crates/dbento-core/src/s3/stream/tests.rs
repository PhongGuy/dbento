//! Listing pages on the scripted client
//! (https://docs.aws.amazon.com/AmazonS3/latest/API/API_ListObjectsV2.html).

use crate::aws::replay::{RawReplay as Replay, RawReply as Reply};
use crate::s3::driver::S3Session;
use crate::s3::replay::{list_page, s3_error, session};
use crate::session::Driver;
use crate::{ExecuteError, Limits, OnTruncate};

/// Three ListObjectsV2 pages of five keys, chained by the tokens `t1` and `t2`.
fn pages() -> Vec<Reply> {
    vec![
        Reply::status(200).xml(&list_page(&[], &[("k1", 1), ("k2", 1)], Some("t1"), true)),
        Reply::status(200).xml(&list_page(&[], &[("k3", 1), ("k4", 1)], Some("t2"), true)),
        Reply::status(200).xml(&list_page(&[], &[("k5", 1)], None, true)),
    ]
}

/// The continuation token each request after the connect ping carried.
fn tokens(r: &Replay) -> Vec<Option<String>> {
    r.seen()
        .iter()
        .skip(1)
        .map(|s| {
            s.uri
                .split(['?', '&'])
                .find_map(|p| p.strip_prefix("continuation-token="))
                .map(str::to_owned)
        })
        .collect()
}

#[tokio::test]
async fn park_then_extend_resumes_from_the_buffer_then_the_token() {
    // The first reply answers the connect ping.
    let mut replies = vec![Reply::status(200).xml(&list_page(&[], &[], None, true))];
    replies.extend(pages());
    let r = Replay::new(replies);
    let mut s = session(&r, Some(&["b"])).await.unwrap();
    let mut script = s.start_script("ls -r 's3://b/'");
    S3Session::prepare_next(&mut script).await.unwrap().unwrap();
    let l = Limits {
        max_rows: Some(3),
        on_truncate: OnTruncate::Park,
        ..Limits::default()
    };
    let (_, mut stream) = S3Session::execute(&mut script, l).await.unwrap();
    let first = crate::s3::stream::tests_support::drain(&mut stream).await;
    assert_eq!(first.0, ["k1", "k2", "k3"], "the first three keys");
    assert!(first.1.truncated, "the row cap cut the listing");
    S3Session::extend(&mut stream, None).unwrap();
    let rest = crate::s3::stream::tests_support::drain(&mut stream).await;
    assert_eq!(rest.0, ["k4", "k5"], "the parked key, then the last page");
    assert!(!rest.1.truncated, "the extended listing ran to its end");
    assert_eq!(
        tokens(&r),
        [None, Some("t1".to_owned()), Some("t2".to_owned())],
        "each page resumes from the previous token"
    );
}

#[tokio::test]
async fn a_stale_token_says_list_again() {
    let r = Replay::new(vec![
        Reply::status(200).xml(&list_page(&[], &[], None, true)),
        Reply::status(200).xml(&list_page(&[], &[("k1", 1)], Some("t1"), true)),
        Reply::status(400).xml(&s3_error(
            "InvalidArgument",
            "The continuation token provided is incorrect",
        )),
    ]);
    let mut s = session(&r, Some(&["b"])).await.unwrap();
    match crate::s3::driver::tests_support::run_one(&mut s, "ls 's3://b/'").await {
        Err(ExecuteError::Database { hint, .. }) => {
            assert!(hint.unwrap().contains("list again"), "the stale-token hint");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_prefix_and_an_object_named_like_it_both_list() {
    let r = Replay::new(vec![
        Reply::status(200).xml(&list_page(&[], &[], None, true)),
        Reply::status(200).xml(&list_page(&["a/"], &[("a/", 0), ("b", 1)], None, true)),
    ]);
    let mut s = session(&r, Some(&["b"])).await.unwrap();
    let rows = crate::s3::driver::tests_support::run_one(&mut s, "ls 's3://b/'")
        .await
        .unwrap();
    let got: Vec<(Option<String>, Option<String>)> = rows
        .into_iter()
        .map(|r| (r[0].clone(), r[1].clone()))
        .collect();
    assert_eq!(
        got,
        [
            (Some("prefix".into()), Some("a/".into())),
            (Some("object".into()), Some("a/".into())),
            (Some("object".into()), Some("b".into())),
        ],
        "a prefix lists before the object of the same name"
    );
}
