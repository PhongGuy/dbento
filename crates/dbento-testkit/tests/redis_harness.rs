//! The Redis harness: users, ACLs and the seed.
#![cfg(feature = "redis")]

use dbento_testkit::{
    BIG_LIST_LEN, BIG_STRING_LEN, BINARY_KEY, BINARY_VALUE, RdUser, SEED_KEYS, redis_db,
};

#[tokio::test]
async fn the_seed_holds_every_kind() {
    let db = redis_db().await;
    assert_eq!(db.int(&["DBSIZE"]).await, SEED_KEYS as i64);
    for (key, kind) in [
        ("str", "string"),
        ("utf8", "string"),
        ("int", "string"),
        ("hash", "hash"),
        ("list", "list"),
        ("set", "set"),
        ("zset", "zset"),
        ("stream", "stream"),
        ("ttl", "string"),
        ("big:hash", "hash"),
        ("big:set", "set"),
        ("big:zset", "zset"),
        ("big:stream", "stream"),
        ("many:1999", "string"),
    ] {
        assert_eq!(
            db.text(&["TYPE", key]).await.as_deref(),
            Some(kind),
            "{key}"
        );
    }
    assert_eq!(db.int(&["LLEN", "big:list"]).await, BIG_LIST_LEN as i64);
    assert_eq!(
        db.int(&["STRLEN", "big:string"]).await,
        BIG_STRING_LEN as i64
    );
    let ttl = db.int(&["PTTL", "ttl"]).await;
    assert!(ttl > 0 && ttl <= 3_600_000, "{ttl}");
    assert_eq!(db.int(&["PTTL", "str"]).await, -1);
    match db.cmd_raw(&[b"GET", BINARY_KEY]).await {
        redis::Value::BulkString(v) => assert_eq!(v, BINARY_VALUE),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_reader_reads_and_the_writer_writes() {
    let db = redis_db().await;
    let mut ro = db.connection(RdUser::Reader).await;
    let got: redis::Value = redis::cmd("GET")
        .arg("str")
        .query_async(&mut ro)
        .await
        .unwrap();
    assert_eq!(got, redis::Value::BulkString(b"hello".to_vec()));
    let refused = redis::cmd("SET")
        .arg("str")
        .arg("x")
        .query_async::<redis::Value>(&mut ro)
        .await
        .unwrap_err();
    assert_eq!(refused.code(), Some("NOPERM"), "{refused}");
    let mut w = db.connection(RdUser::Writer).await;
    let () = redis::cmd("SET")
        .arg("w")
        .arg("1")
        .query_async(&mut w)
        .await
        .unwrap();
    assert_eq!(db.text(&["GET", "w"]).await.as_deref(), Some("1"));
}

#[tokio::test]
async fn a_created_user_has_its_rules() {
    let db = redis_db().await;
    let (name, pw) = db.create_user("probe", "~* &* -@all +@read +hello").await;
    assert!(name.starts_with("probe") && !pw.is_empty(), "{name}");
    // Value's Debug prints UTF-8 bulk strings as text.
    let listed = format!("{:?}", db.cmd(&["ACL", "GETUSER", &name]).await);
    assert!(
        listed.contains("+@read") && listed.contains("on"),
        "{listed}"
    );
}
