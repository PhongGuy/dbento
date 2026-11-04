//! Connecting to SQL Server with tls = "off": the server info.
mod support;

use dbento_core::{Engine, Flavor, Limits, SessionOptions};
use support::{ms_off, run, text};

#[tokio::test]
async fn off_connects_to_the_stock_certificate_and_reports_the_server() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    assert_eq!(s.engine(), Engine::Mssql);
    let server = s.server().unwrap();
    assert_eq!(server.flavor, Flavor::SqlServer);
    assert!(server.version.0 >= 16, "{:?}", server.version);
    let ran = run(&mut s, "SELECT 1 AS one", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(ran.columns, vec![("one".to_owned(), "INT".to_owned())]);
    assert_eq!(ran.rows, vec![vec![text("1")]]);
    s.close().await.unwrap();
}
