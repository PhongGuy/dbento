//! The one door to the S3 API: an SDK client with one method per read call in [`S3Op`].
//!
//! [`S3Op`]: super::ops::S3Op

use std::borrow::Cow;
use std::time::Duration;

use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::config::{
    BehaviorVersion, Credentials, Region, RequestChecksumCalculation, ResponseChecksumValidation,
    SharedHttpClient,
};
use aws_sdk_s3::error::{ErrorMetadata, SdkError};
use aws_sdk_s3::operation::get_object::GetObjectError;
use aws_sdk_s3::operation::head_object::HeadObjectError;
use aws_sdk_s3::operation::list_buckets::ListBucketsError;
use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error;
use aws_sdk_s3::primitives::{ByteStream, DateTime, DateTimeFormat, SdkBody};
use aws_sdk_s3::types::EncodingType;
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_runtime_api::http::StatusCode;
use percent_encoding::percent_decode_str;

use super::browse::ObjectMeta;
use crate::aws::{
    AwsCredentials, endpoint_origin, own_retry_quota, resolve_credentials, resolve_region,
};
use crate::secrets::Secret;
use crate::session::ConnectError;
use crate::workspace::ConnectionConfig;

/// How long opening a TCP connection may take before the call fails.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The most keys one ListObjectsV2 page may ask for; AWS caps there, some stores do not.
const MAX_KEYS: i32 = 1000;

/// Everything a client is built from; `endpoint` is already an origin (see [`endpoint_origin`]).
pub(crate) struct ClientSettings {
    pub(crate) region: String,
    pub(crate) endpoint: Option<String>,
    pub(crate) virtual_hosted: bool,
    pub(crate) credentials: AwsCredentials,
    pub(crate) max_attempts: u32,
}

impl ClientSettings {
    /// Region, credentials, endpoint origin and addressing of `conn`; `env` stands in for the
    /// process environment.
    pub(crate) fn from_connection(
        conn: &ConnectionConfig,
        secret: Option<&Secret>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConnectError> {
        let open = |message: String| ConnectError::Open { message };
        let region = resolve_region(conn, &env).map_err(open)?;
        let credentials = resolve_credentials(conn, secret, &env).map_err(open)?;
        let endpoint = match conn.endpoint() {
            Some(text) => Some(
                endpoint_origin(text)
                    .ok_or_else(|| open(format!("endpoint `{text}` is not a URL with a host")))?,
            ),
            None => None,
        };
        Ok(ClientSettings {
            region,
            endpoint,
            virtual_hosted: conn.virtual_hosted(),
            credentials,
            max_attempts: 3,
        })
    }
}

/// The SDK's failure for one call with the raw response it came from, boxed because it runs to
/// hundreds of bytes and would bloat every `Result`.
pub(crate) type Sdk<E> = Box<SdkError<E, HttpResponse>>;

/// One ListObjectsV2 page request; `prefix` empty lists from the bucket root.
pub(crate) struct ListCall {
    pub(crate) bucket: String,
    pub(crate) prefix: String,
    pub(crate) delimiter: Option<char>,
    pub(crate) token: Option<String>,
    pub(crate) max_keys: u32,
}

/// One page of a listing with keys already decoded.
#[derive(Debug)]
pub(crate) struct Listed {
    pub(crate) prefixes: Vec<String>,
    pub(crate) objects: Vec<ObjectMeta>,
    pub(crate) next_token: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Bucket {
    pub(crate) name: String,
    pub(crate) created: Option<String>,
}

/// The head of an object: at most the bytes asked for, and the object's whole size.
#[derive(Debug)]
pub(crate) struct RangeRead {
    pub(crate) bytes: Vec<u8>,
    pub(crate) total_size: u64,
    pub(crate) content_type: Option<String>,
}

/// A whole object's body, unread; `length` is the GET Content-Length.
pub(crate) struct Body {
    pub(crate) meta: ObjectMeta,
    pub(crate) length: Option<u64>,
    pub(crate) stream: ByteStream,
}

#[derive(Clone)]
pub(crate) struct S3Client {
    inner: aws_sdk_s3::Client,
}

impl S3Client {
    /// Builds the client over `http`; nothing is sent until the first call. A custom endpoint is
    /// a third-party store: path style unless virtual hosting is asked for, and checksums only
    /// where required. AWS itself keeps virtual hosting and its default checksums.
    pub(crate) fn new(settings: ClientSettings, http: SharedHttpClient) -> Self {
        let ClientSettings {
            region,
            endpoint,
            virtual_hosted,
            credentials,
            max_attempts,
        } = settings;
        let credentials = Credentials::new(
            credentials.access_key_id,
            credentials.secret_access_key.expose(),
            credentials.session_token.map(|t| t.expose().to_owned()),
            None,
            "dbento",
        );
        let timeouts = TimeoutConfig::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build();
        let mut config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::v2026_11_10())
            .retry_partition(own_retry_quota("s3", &region))
            .region(Region::new(region))
            .credentials_provider(credentials)
            .retry_config(RetryConfig::standard().with_max_attempts(max_attempts))
            .timeout_config(timeouts)
            .disable_s3_express_session_auth(true)
            .http_client(http);
        config = match endpoint {
            Some(endpoint) => config
                .endpoint_url(endpoint)
                .force_path_style(!virtual_hosted)
                .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
                .response_checksum_validation(ResponseChecksumValidation::WhenRequired),
            None => config
                .request_checksum_calculation(RequestChecksumCalculation::WhenSupported)
                .response_checksum_validation(ResponseChecksumValidation::WhenSupported),
        };
        S3Client {
            inner: aws_sdk_s3::Client::from_conf(config.build()),
        }
    }

    /// `S3Op::ListBuckets`.
    pub(crate) async fn list_buckets(&self) -> Result<Vec<Bucket>, Sdk<ListBucketsError>> {
        let out = self.inner.list_buckets().send().await?;
        Ok(out
            .buckets()
            .iter()
            .filter_map(|bucket| {
                Some(Bucket {
                    name: bucket.name()?.to_owned(),
                    created: bucket.creation_date().and_then(rfc3339),
                })
            })
            .collect())
    }

    /// `S3Op::ListObjectsV2`, always in url encoding so control bytes in keys survive the XML.
    /// Keys are decoded only when the reply echoes that encoding.
    pub(crate) async fn list_objects(
        &self,
        call: &ListCall,
    ) -> Result<Listed, Sdk<ListObjectsV2Error>> {
        let out = self
            .inner
            .list_objects_v2()
            .bucket(&call.bucket)
            .set_prefix((!call.prefix.is_empty()).then(|| call.prefix.clone()))
            .set_delimiter(call.delimiter.map(String::from))
            .set_continuation_token(call.token.clone())
            .max_keys(i32::try_from(call.max_keys).map_or(MAX_KEYS, |n| n.min(MAX_KEYS)))
            .encoding_type(EncodingType::Url)
            .send()
            .await?;
        let url_mode = out.encoding_type() == Some(&EncodingType::Url);
        let key = |raw: &str| {
            if url_mode {
                decode_key(raw).ok_or_else(|| invalid_key_encoding(raw))
            } else {
                Ok(raw.to_owned())
            }
        };
        let prefixes = out
            .common_prefixes()
            .iter()
            .filter_map(|p| p.prefix())
            .map(key)
            .collect::<Result<_, _>>()?;
        let objects = out
            .contents()
            .iter()
            .filter_map(|object| Some((object, object.key()?)))
            .map(|(object, raw)| {
                Ok(ObjectMeta {
                    key: key(raw)?,
                    size: size(object.size()),
                    last_modified: object.last_modified().and_then(rfc3339),
                    etag: object.e_tag().map(unquote),
                    storage_class: object.storage_class().map(|c| c.as_str().to_owned()),
                    content_type: None,
                })
            })
            .collect::<Result<_, Sdk<ListObjectsV2Error>>>()?;
        Ok(Listed {
            prefixes,
            objects,
            next_token: out.next_continuation_token().map(str::to_owned),
        })
    }

    /// `S3Op::HeadObject`.
    pub(crate) async fn head_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<ObjectMeta, Sdk<HeadObjectError>> {
        let out = self
            .inner
            .head_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await?;
        Ok(ObjectMeta {
            key: key.to_owned(),
            size: size(out.content_length()),
            last_modified: out.last_modified().and_then(rfc3339),
            etag: out.e_tag().map(unquote),
            storage_class: out.storage_class().map(|c| c.as_str().to_owned()),
            content_type: out.content_type().map(str::to_owned),
        })
    }

    /// `S3Op::GetObject` for the first `max_bytes` bytes (at least one). The total size comes
    /// from `Content-Range`, else `Content-Length`; anything a server sends past the range is
    /// dropped.
    pub(crate) async fn get_range(
        &self,
        bucket: &str,
        key: &str,
        max_bytes: u32,
    ) -> Result<RangeRead, Sdk<GetObjectError>> {
        let want = max_bytes.max(1);
        let out = self
            .inner
            .get_object()
            .bucket(bucket)
            .key(key)
            .range(format!("bytes=0-{}", want - 1))
            .send()
            .await?;
        let total_size = out
            .content_range()
            .and_then(|range| range.rsplit_once('/'))
            .and_then(|(_, total)| total.parse().ok())
            .unwrap_or_else(|| size(out.content_length()));
        let content_type = out.content_type().map(str::to_owned);
        let limit = usize::try_from(want).unwrap_or(usize::MAX);
        let mut stream = out.body;
        let mut bytes = Vec::new();
        while bytes.len() < limit {
            match stream.next().await {
                Some(Ok(chunk)) => bytes.extend_from_slice(&chunk),
                Some(Err(error)) => {
                    return Err(Box::new(SdkError::dispatch_failure(ConnectorError::io(
                        error.into(),
                    ))));
                }
                None => break,
            }
        }
        bytes.truncate(limit);
        Ok(RangeRead {
            bytes,
            total_size,
            content_type,
        })
    }

    /// `S3Op::GetObject` for the whole object, its body left for the caller to stream.
    pub(crate) async fn get_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<Body, Sdk<GetObjectError>> {
        let out = self
            .inner
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await?;
        let length = out.content_length().and_then(|n| u64::try_from(n).ok());
        let meta = ObjectMeta {
            key: key.to_owned(),
            size: size(out.content_length()),
            last_modified: out.last_modified().and_then(rfc3339),
            etag: out.e_tag().map(unquote),
            storage_class: out.storage_class().map(|c| c.as_str().to_owned()),
            content_type: out.content_type().map(str::to_owned),
        };
        Ok(Body {
            meta,
            length,
            stream: out.body,
        })
    }
}

/// A url-encoded key as S3 writes it: `+` is a space, then percent escapes, then UTF-8.
fn decode_key(raw: &str) -> Option<String> {
    let spaced = raw.replace('+', " ");
    percent_decode_str(&spaced)
        .decode_utf8()
        .ok()
        .map(Cow::into_owned)
}

/// A listed key that is not UTF-8 once decoded, reported as a service error so it classifies
/// like any other S3 error code.
fn invalid_key_encoding(raw: &str) -> Sdk<ListObjectsV2Error> {
    let meta = ErrorMetadata::builder()
        .code("InvalidKeyEncoding")
        .message(format!("the listed key `{raw}` is not UTF-8 once decoded"))
        .build();
    let status = StatusCode::try_from(200).unwrap_or_else(|_| unreachable!("200 is a status"));
    Box::new(SdkError::service_error(
        ListObjectsV2Error::generic(meta),
        HttpResponse::new(status, SdkBody::empty()),
    ))
}

fn size(bytes: Option<i64>) -> u64 {
    bytes.and_then(|n| u64::try_from(n).ok()).unwrap_or(0)
}

fn rfc3339(time: &DateTime) -> Option<String> {
    time.fmt(DateTimeFormat::DateTime).ok()
}

fn unquote(etag: &str) -> String {
    etag.strip_prefix('"')
        .and_then(|e| e.strip_suffix('"'))
        .unwrap_or(etag)
        .to_owned()
}

#[cfg(test)]
mod tests;
