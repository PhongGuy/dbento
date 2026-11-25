//! The S3 harness on Moto.
#![cfg(feature = "s3")]
use dbento_testkit::{
    BASE_KEYS, DySource, HOSTILE_KEYS, PAGED, S3_SECRET_VAR, THREE_MIB, big_byte, s3_moto,
    s3_source,
};

#[test]
fn the_s3_url_wins_then_the_dynamodb_url_then_a_container() {
    let both = |k: &str| match k {
        "DBENTO_S3_URL" => Some("http://127.0.0.1:1".to_owned()),
        "DBENTO_DYNAMO_URL" => Some("http://127.0.0.1:2".to_owned()),
        _ => None,
    };
    assert_eq!(
        s3_source(both),
        DySource::Url("http://127.0.0.1:1".to_owned())
    );
    let dynamo = |k: &str| (k == "DBENTO_DYNAMO_URL").then(|| "http://127.0.0.1:2".to_owned());
    assert_eq!(
        s3_source(dynamo),
        DySource::Url("http://127.0.0.1:2".to_owned())
    );
    assert_eq!(s3_source(|_| Some(String::new())), DySource::Container);
}

async fn keys(s3: &dbento_testkit::S3Moto, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut token = None;
    loop {
        let page = s3
            .client()
            .list_objects_v2()
            .bucket(s3.bucket())
            .prefix(prefix)
            .encoding_type(aws_sdk_s3::types::EncodingType::Url)
            .set_continuation_token(token)
            .send()
            .await
            .unwrap();
        out.extend(page.contents().iter().map(|o| o.key().unwrap().to_owned()));
        token = page.next_continuation_token().map(str::to_owned);
        if token.is_none() {
            return out;
        }
    }
}

#[tokio::test]
async fn the_base_seed_holds_every_key_and_the_hostile_ones_head_back() {
    let s3 = s3_moto().await;
    assert_eq!(keys(&s3, "").await.len(), BASE_KEYS);
    for key in HOSTILE_KEYS {
        let head = s3
            .client()
            .head_object()
            .bucket(s3.bucket())
            .key(key)
            .send()
            .await;
        assert_eq!(head.unwrap().content_length(), Some(1), "{key:?}");
    }
    let big = s3
        .client()
        .get_object()
        .bucket(s3.bucket())
        .key("big/three-mib.bin")
        .send()
        .await
        .unwrap();
    let bytes = big.body.collect().await.unwrap().into_bytes();
    assert_eq!(bytes.len(), THREE_MIB);
    assert!(bytes.iter().enumerate().all(|(i, b)| *b == big_byte(i)));
}

#[tokio::test]
async fn seed_paged_adds_exactly_the_paged_keys_and_handles_get_their_own_bucket() {
    let a = s3_moto().await;
    let b = s3_moto().await;
    assert_ne!(a.bucket(), b.bucket());
    assert!(
        a.bucket().len() <= 63
            && a.bucket()
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    );
    a.seed_paged().await;
    let many = keys(&a, "many/").await;
    assert_eq!(many.len(), PAGED);
    assert_eq!(
        (many[0].as_str(), many[PAGED - 1].as_str()),
        ("many/00000", "many/02499")
    );
    assert!(keys(&b, "many/").await.is_empty());
}

#[tokio::test]
async fn the_connection_toml_scopes_to_the_bucket_and_reads_the_secret_from_env() {
    let s3 = s3_moto().await;
    let toml = s3.connection_toml("assets", "read_only = true");
    assert!(toml.contains("engine = \"s3\""), "{toml}");
    assert!(
        toml.contains(&format!("buckets = [\"{}\"]", s3.bucket())),
        "{toml}"
    );
    assert!(
        toml.contains(&format!("secret = {{ env = \"{S3_SECRET_VAR}\" }}")),
        "{toml}"
    );
    assert!(
        toml.contains(&format!("endpoint = \"{}\"", s3.endpoint())),
        "{toml}"
    );
    assert!(toml.trim_end().ends_with("read_only = true"), "{toml}");
}
