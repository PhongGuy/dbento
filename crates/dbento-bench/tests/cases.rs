use dbento_bench::cases::{Case, PgUrl, Targets, measure};

#[test]
fn case_names_round_trip_in_order() {
    let names: Vec<_> = Case::ALL.iter().map(|c| c.name()).collect();
    assert_eq!(
        names,
        [
            "ttfr_pg",
            "ttfr_sqlite",
            "stream_pg",
            "stream_sqlite",
            "narrow_pg",
            "narrow_sqlite",
            "wide_pg",
            "wide_sqlite"
        ]
    );
    for c in Case::ALL {
        assert_eq!(Case::parse(c.name()), Some(c));
    }
    assert_eq!(Case::parse("ttfr"), None);
}

#[test]
fn a_postgres_url_splits_into_its_parts() {
    let u = PgUrl::parse("postgres://postgres:dbento@127.0.0.1:43461/stress").unwrap();
    assert_eq!(
        (
            u.host.as_str(),
            u.port,
            u.user.as_str(),
            u.password.as_str(),
            u.database.as_str()
        ),
        ("127.0.0.1", 43461, "postgres", "dbento", "stress")
    );
    for bad in [
        "",
        "mysql://a:b@h:1/d",
        "postgres://a@h:1/d",
        "postgres://a:b@h/d",
        "postgres://a:b@h:x/d",
        "postgres://a:b@h:1",
    ] {
        assert!(PgUrl::parse(bad).is_err(), "{bad:?}");
    }
}

#[tokio::test]
async fn a_missing_target_names_its_flag() {
    let t = Targets::new(None, None).unwrap();
    let err = measure(Case::TtfrPg, &t).await.unwrap_err();
    assert!(format!("{err:#}").contains("--pg-url"), "{err:#}");
    let err = measure(Case::TtfrSqlite, &t).await.unwrap_err();
    assert!(format!("{err:#}").contains("--sqlite"), "{err:#}");
}

#[tokio::test]
async fn every_sqlite_case_measures_through_the_core() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stress.sqlite");
    xtask::stress::build_sqlite_stress(&path, 3000, 200).unwrap();
    let t = Targets::new(None, Some(path)).unwrap();
    let ttfr = measure(Case::TtfrSqlite, &t).await.unwrap();
    assert_eq!((ttfr.case.as_str(), ttfr.rows), ("ttfr_sqlite", 1024));
    assert!(ttfr.value > 0.0);
    assert_eq!(measure(Case::StreamSqlite, &t).await.unwrap().rows, 3000);
    let narrow = measure(Case::NarrowSqlite, &t).await.unwrap();
    assert_eq!(narrow.rows, 3000);
    assert!(narrow.value >= 0.0);
    assert_eq!(measure(Case::WideSqlite, &t).await.unwrap().rows, 200);
}

#[tokio::test]
async fn every_postgres_case_measures_through_the_core() {
    let db = dbento_testkit::pg_db().await;
    let admin = db.admin().await;
    for s in xtask::stress::pg_stress_statements(3000, 200) {
        admin.batch_execute(&s).await.unwrap();
    }
    let url = format!(
        "postgres://{}:{}@{}:{}/{}",
        db.user(),
        db.password(),
        db.host(),
        db.port(),
        db.database()
    );
    let t = Targets::new(Some(PgUrl::parse(&url).unwrap()), None).unwrap();
    let ttfr = measure(Case::TtfrPg, &t).await.unwrap();
    assert_eq!(ttfr.case, "ttfr_pg");
    assert!(ttfr.rows >= 1 && ttfr.value > 0.0);
    assert_eq!(measure(Case::StreamPg, &t).await.unwrap().rows, 3000);
    assert_eq!(measure(Case::NarrowPg, &t).await.unwrap().rows, 3000);
    assert_eq!(measure(Case::WidePg, &t).await.unwrap().rows, 200);
}

#[test]
fn a_bad_postgres_url_in_the_environment_spares_sqlite_cases() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stress.sqlite");
    xtask::stress::build_sqlite_stress(&path, 10, 5).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_dbento-bench"))
        .args(["sample", "stream_sqlite", "--sqlite"])
        .arg(&path)
        .env("DBENTO_PG_17_STRESS_URL", "not a url")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn debug_output_never_shows_the_password() {
    let u = PgUrl::parse("postgres://u:hunter2secret@h:5432/db").unwrap();
    assert!(!format!("{u:?}").contains("hunter2secret"));
}
