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
