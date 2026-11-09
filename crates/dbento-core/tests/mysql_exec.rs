//! The MySQL driver on both flavors: connect, refusals, errors and a streamed result.

use crate::support::{my_fixtures, run, text};
use dbento_core::{
    ConnectError, Engine, ExecuteError, Flavor, IntrospectError, Limits, RelationRef, Secret,
    SessionOptions, connect,
};
use dbento_testkit::MyFlavor;

#[tokio::test]
async fn a_session_reports_its_flavor_and_a_wrong_password_its_number() {
    for (label, fx) in my_fixtures("").await {
        let s = fx.session(SessionOptions::default()).await;
        assert_eq!(s.engine(), Engine::Mysql, "{label}");
        let server = s.server().unwrap();
        let want = match fx.db.flavor() {
            MyFlavor::Mysql => Flavor::Mysql,
            MyFlavor::Mariadb => Flavor::Mariadb,
        };
        assert_eq!(server.flavor, want, "{label}");
        assert!(server.version.0 >= 8, "{label}: {:?}", server.version);
        assert!(
            s.capabilities().schemas && !s.capabilities().file,
            "{label}"
        );
        s.close().await.unwrap();
        let wrong = connect(
            &fx.ws,
            fx.conn(),
            Some(Secret::new("not-the-password")),
            SessionOptions::default(),
        );
        match wrong.await {
            Err(ConnectError::Open { message, .. }) => assert!(
                message.contains("1045") && message.contains(&fx.conn().display_target()),
                "{label}: {message}"
            ),
            Err(other) => panic!("{label}: {other:?}"),
            Ok(_) => panic!("{label}: connected"),
        }
    }
}

#[tokio::test]
async fn a_server_error_carries_the_error_number_and_sqlstate() {
    for (label, fx) in my_fixtures("").await {
        let mut s = fx.session(SessionOptions::default()).await;
        match run(&mut s, "SELECT * FROM missing_t", Limits::default())
            .await
            .remove(0)
        {
            Err(ExecuteError::Database {
                code,
                sqlstate,
                message,
                ..
            }) => {
                assert_eq!(
                    (code.as_str(), sqlstate.as_deref()),
                    ("1146", Some("42S02")),
                    "{label}"
                );
                assert!(message.contains("missing_t"), "{label}: {message}");
            }
            other => panic!("{label}: {:?}", other.err()),
        }
        let ran = run(&mut s, "SELECT 7", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert_eq!(ran.rows, vec![vec![text("7")]], "{label}");
    }
}

#[tokio::test]
async fn edit_targets_stay_unsupported_on_mysql() {
    for (label, fx) in my_fixtures("").await {
        let mut s = fx.session(SessionOptions::default()).await;
        let rel = RelationRef::new(fx.db.database(), "t");
        assert!(
            matches!(
                s.edit_target(&rel).await,
                Err(IntrospectError::Unsupported { .. })
            ),
            "{label}"
        );
        assert_eq!(
            run(&mut s, "SELECT 1", Limits::default())
                .await
                .remove(0)
                .unwrap()
                .rows,
            vec![vec![text("1")]]
        );
    }
}
