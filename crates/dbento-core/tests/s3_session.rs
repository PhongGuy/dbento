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
