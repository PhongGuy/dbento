//! S3 sessions on Moto.
#![cfg(feature = "s3")]
use dbento_core::{Secret, SessionOptions, Workspace, connect};
use dbento_testkit::{S3_SECRET, S3Moto, s3_moto};

use crate::support::{Cell, read, text};

pub struct S3Fixture {
    pub s3: S3Moto,
    #[allow(
        dead_code,
        reason = "holds the workspace folder for the fixture's lifetime"
    )]
    pub dir: tempfile::TempDir,
    pub ws: Workspace,
}

pub async fn s3fx(extra: &str) -> S3Fixture {
    let s3 = s3_moto().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{}", s3.connection_toml("moto", extra)),
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    S3Fixture { s3, dir, ws }
}

impl S3Fixture {
    pub async fn session(&self) -> dbento_core::Session {
        connect(
            &self.ws,
            self.ws.declared("moto").unwrap(),
            Some(Secret::new(S3_SECRET)),
            SessionOptions::default(),
        )
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn ls_lists_the_configured_bucket_and_schemas_agree() {
    let fx = s3fx("").await;
    let mut s = fx.session().await;
    assert!(s.server().is_none());
    let got = read(&mut s, "ls").await;
    assert_eq!(
        got.columns,
        [
            ("bucket".to_owned(), "text".to_owned()),
            ("created".to_owned(), "timestamp".to_owned())
        ]
    );
    assert_eq!(got.rows, [vec![text(fx.s3.bucket()), Cell::Null]]);
    let names: Vec<String> = s
        .schemas()
        .await
        .unwrap()
        .iter()
        .map(|x| x.name().to_owned())
        .collect();
    assert_eq!(names, [fx.s3.bucket()]);
}

#[tokio::test]
async fn ls_r_over_2500_keys_parks_and_extends_to_every_key_in_order() {
    let fx = s3fx("").await;
    fx.s3.seed_paged().await;
    let mut s = fx.session().await;
    let mut script = s.script(&format!("ls -r 's3://{}/many/'", fx.s3.bucket()));
    let pending = script.next().await.unwrap().unwrap();
    let mut l = dbento_core::Limits::default();
    l.max_rows = Some(1_200);
    l.on_truncate = dbento_core::OnTruncate::Park;
    let mut stream = pending.execute(l).await.unwrap();
    let mut keys = Vec::new();
    for (more, expected, cut) in [
        (None, 1_200, true),
        (Some(None), dbento_testkit::PAGED, false),
    ] {
        if let Some(more) = more {
            stream.extend(more).await.unwrap();
        }
        while let Some(b) = stream.next_batch().await.unwrap() {
            for r in 0..b.len() {
                if let dbento_core::CellRef::Text(k) = b.cell(r, 1) {
                    keys.push(k.to_owned());
                }
            }
        }
        assert_eq!(
            (keys.len(), stream.outcome().unwrap().truncated),
            (expected, cut),
            "keys so far and whether the listing was cut"
        );
    }
    let want: Vec<String> = (0..dbento_testkit::PAGED)
        .map(|i| format!("many/{i:05}"))
        .collect();
    assert_eq!(keys, want, "every key once, in listing order");
}

#[tokio::test]
async fn a_delimited_listing_shows_prefixes_and_decodes_hostile_keys() {
    let fx = s3fx("").await;
    let mut s = fx.session().await;
    let root = read(&mut s, &format!("ls 's3://{}/'", fx.s3.bucket())).await;
    let prefixes: Vec<&Cell> = root
        .rows
        .iter()
        .filter(|r| r[0] == text("prefix"))
        .map(|r| &r[1])
        .collect();
    assert_eq!(
        prefixes,
        [
            "big/", "bin/", "deep/", "empty/", "hostile/", "json/", "text/", "unicode/"
        ]
        .map(text)
        .iter()
        .collect::<Vec<_>>(),
        "the top-level prefixes in order"
    );
    let hostile = read(&mut s, &format!("ls -r 's3://{}/hostile/'", fx.s3.bucket())).await;
    let mut keys: Vec<Cell> = hostile.rows.iter().map(|r| r[1].clone()).collect();
    let mut want: Vec<Cell> = dbento_testkit::HOSTILE_KEYS.map(text).to_vec();
    keys.sort_by_key(|c| format!("{c:?}"));
    want.sort_by_key(|c| format!("{c:?}"));
    assert_eq!(keys, want, "every hostile key decodes to its raw bytes");
}

#[tokio::test]
async fn a_new_statement_ends_a_parked_listing() {
    let fx = s3fx("").await;
    let mut s = fx.session().await;
    let mut script = s.script(&format!("ls -r 's3://{}/'\nls", fx.s3.bucket()));
    let pending = script.next().await.unwrap().unwrap();
    let mut l = dbento_core::Limits::default();
    l.max_rows = Some(1);
    l.on_truncate = dbento_core::OnTruncate::Park;
    let mut stream = pending.execute(l).await.unwrap();
    while stream.next_batch().await.unwrap().is_some() {}
    drop(stream);
    let next = crate::support::collect(
        script.next().await.unwrap().unwrap(),
        dbento_core::Limits::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        next.rows.len(),
        1,
        "the bucket listing after the parked one"
    );
}

fn b(fx: &S3Fixture, key: &str) -> String {
    format!("'s3://{}/{}'", fx.s3.bucket(), key)
}

#[tokio::test]
async fn stat_reads_the_head_metadata() {
    let fx = s3fx("").await;
    let mut s = fx.session().await;
    let got = read(&mut s, &format!("stat {}", b(&fx, "json/doc.json"))).await;
    let names: Vec<&str> = got.columns.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(
        names,
        [
            "key",
            "size",
            "last_modified",
            "etag",
            "storage_class",
            "content_type"
        ]
    );
    assert_eq!(got.rows[0][0], text("json/doc.json"));
    assert_eq!(got.rows[0][1], text("26"));
    assert_eq!(got.rows[0][5], text("application/json"));
}

#[tokio::test]
async fn cat_is_text_for_utf8_bytes_for_binary_and_empty_for_a_zero_object() {
    let fx = s3fx("").await;
    let mut s = fx.session().await;
    let t = read(&mut s, &format!("cat {}", b(&fx, "text/hello.txt"))).await;
    assert_eq!(
        t.rows[0],
        [text("hello, bento\n"), text("13"), text("false")]
    );
    let bin = read(&mut s, &format!("cat {}", b(&fx, "bin/blob.bin"))).await;
    assert_eq!(bin.rows[0][0], Cell::Bytes((0..=255u8).collect()));
    let empty = read(&mut s, &format!("cat {}", b(&fx, "empty/zero"))).await;
    assert_eq!(empty.rows[0], [text(""), text("0"), text("false")]);
}

#[tokio::test]
async fn cat_caps_at_one_mib_and_under_the_byte_limit() {
    let fx = s3fx("").await;
    let mut s = fx.session().await;
    let big = read(&mut s, &format!("cat {}", b(&fx, "big/three-mib.bin"))).await;
    let Cell::Bytes(bytes) = &big.rows[0][0] else {
        panic!("{:?}", big.rows[0][0])
    };
    assert_eq!(bytes.len(), 1 << 20);
    assert!(
        bytes
            .iter()
            .enumerate()
            .all(|(i, x)| *x == dbento_testkit::big_byte(i))
    );
    assert_eq!(&big.rows[0][1..], [text("3145728"), text("true")]);
    let mut l = dbento_core::Limits::default();
    l.max_bytes = Some(1_000);
    let small =
        crate::support::one(&mut s, &format!("cat {}", b(&fx, "big/three-mib.bin")), l).await;
    let Cell::Bytes(bytes) = &small.rows[0][0] else {
        panic!()
    };
    assert_eq!((bytes.len(), small.rows.len()), (936, 1));
}

#[tokio::test]
async fn a_missing_key_is_no_such_key_and_keys_need_quotes_only_for_specials() {
    let fx = s3fx("").await;
    let mut s = fx.session().await;
    let e = crate::support::failure(&mut s, &format!("stat {}", b(&fx, "nope"))).await;
    assert_eq!(crate::support::code_of(e), "NoSuchKey");
    let quoted = dbento_core::quote_literal(
        dbento_core::Engine::S3,
        &format!("s3://{}/hostile/quote'and;semi", fx.s3.bucket()),
    );
    let got = read(&mut s, &format!("cat {quoted}")).await;
    assert_eq!(got.rows[0][0], text("x"));
}
