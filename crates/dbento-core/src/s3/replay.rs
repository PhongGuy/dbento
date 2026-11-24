//! S3 on the scripted HTTP client: replay settings, a client, and canned XML bodies.

use super::client::{ClientSettings, S3Client};
use crate::aws::AwsCredentials;
use crate::aws::replay::RawReplay;
use crate::secrets::Secret;

/// Settings for a replay client; one attempt, so a scripted failure is never retried.
pub(crate) fn settings(
    endpoint: Option<&str>,
    region: &str,
    virtual_hosted: bool,
) -> ClientSettings {
    ClientSettings {
        region: region.to_owned(),
        endpoint: endpoint.map(str::to_owned),
        virtual_hosted,
        credentials: AwsCredentials {
            access_key_id: "AKIDREPLAY".to_owned(),
            secret_access_key: Secret::new("x"),
            session_token: None,
        },
        max_attempts: 1,
    }
}

pub(crate) fn client(
    replay: &RawReplay,
    endpoint: Option<&str>,
    region: &str,
    virtual_hosted: bool,
) -> S3Client {
    S3Client::new(settings(endpoint, region, virtual_hosted), replay.http())
}

/// A ListObjectsV2 page (https://docs.aws.amazon.com/AmazonS3/latest/API/API_ListObjectsV2.html).
pub(crate) fn list_page(
    prefixes: &[&str],
    keys: &[(&str, u64)],
    next: Option<&str>,
    url_mode: bool,
) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Name>replay</Name><Prefix></Prefix><MaxKeys>1000</MaxKeys>",
    );
    xml += &format!("<KeyCount>{}</KeyCount>", prefixes.len() + keys.len());
    xml += &format!("<IsTruncated>{}</IsTruncated>", next.is_some());
    if url_mode {
        xml += "<EncodingType>url</EncodingType>";
    }
    for (key, size) in keys {
        xml += &format!(
            "<Contents><Key>{}</Key><LastModified>2026-11-23T21:47:52.000Z</LastModified>\
             <ETag>\"d41d8cd98f00b204e9800998ecf8427e\"</ETag><Size>{size}</Size>\
             <StorageClass>STANDARD</StorageClass></Contents>",
            escape(key)
        );
    }
    for prefix in prefixes {
        xml += &format!(
            "<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>",
            escape(prefix)
        );
    }
    if let Some(token) = next {
        xml += &format!(
            "<NextContinuationToken>{}</NextContinuationToken>",
            escape(token)
        );
    }
    xml + "</ListBucketResult>"
}

/// A ListBuckets answer (https://docs.aws.amazon.com/AmazonS3/latest/API/API_ListBuckets.html).
pub(crate) fn buckets(names: &[&str]) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <ListAllMyBucketsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Owner><ID>replay</ID></Owner><Buckets>",
    );
    for name in names {
        xml += &format!(
            "<Bucket><Name>{}</Name><CreationDate>2026-11-23T21:47:52.000Z</CreationDate></Bucket>",
            escape(name)
        );
    }
    xml + "</Buckets></ListAllMyBucketsResult>"
}

/// An error body (https://docs.aws.amazon.com/AmazonS3/latest/API/ErrorResponses.html).
pub(crate) fn s3_error(code: &str, message: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <Error><Code>{}</Code><Message>{}</Message><RequestId>REPLAY</RequestId></Error>",
        escape(code),
        escape(message)
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
