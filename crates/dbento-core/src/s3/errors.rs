//! S3 failures as the session reports them, sorted from the SDK's errors.

use aws_sdk_s3::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};

use super::browse::ObjectBrowseError;
use super::client::Sdk;
use crate::session::ExecuteError;

/// The hint when ListBuckets is refused and the connection names no buckets.
pub(crate) const BUCKETS_HINT: &str =
    "this key cannot list buckets; set buckets = [\"...\"] on the connection";
/// The hint when a continuation token is refused.
pub(crate) const STALE_TOKEN_HINT: &str = "the listing changed or expired; list again";

const AUTH: [&str; 4] = [
    "InvalidAccessKeyId",
    "SignatureDoesNotMatch",
    "ExpiredToken",
    "InvalidToken",
];

/// What went wrong with one S3 call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum S3Failure {
    /// No such key or bucket; `code` is `NotFound` for a bodiless HEAD.
    NotFound {
        code: String,
        message: String,
    },
    AccessDenied {
        message: String,
    },
    /// The bucket lives in another region, which the reply may name.
    WrongRegion {
        region: Option<String>,
    },
    Auth {
        code: String,
        message: String,
    },
    InvalidRange,
    /// Any other error code, or `HTTP <status>` for a reply without one.
    Service {
        code: String,
        message: String,
    },
    Timeout,
    Transport {
        message: String,
    },
}

/// Sorts a call failure by its error code first, since a code can ride on a synthetic reply
/// whose status means nothing; then a codeless reply by its status; then the transport.
#[allow(
    clippy::boxed_local,
    reason = "every client call fails with the boxed error, so callers pass it straight through"
)]
pub(crate) fn classify<E>(error: Sdk<E>) -> S3Failure
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
{
    let error = *error;
    let status = error.raw_response().map(|r| r.status().as_u16());
    let region = || {
        error
            .raw_response()
            .and_then(|r| r.headers().get("x-amz-bucket-region"))
            .map(str::to_owned)
    };
    if let Some(code) = error.code() {
        let code = code.to_owned();
        let message = error.message().unwrap_or_default().to_owned();
        return match code.as_str() {
            "NoSuchKey" | "NoSuchBucket" | "NotFound" => S3Failure::NotFound { code, message },
            "AccessDenied" => S3Failure::AccessDenied { message },
            "PermanentRedirect" => S3Failure::WrongRegion { region: region() },
            "InvalidRange" => S3Failure::InvalidRange,
            c if AUTH.contains(&c) => S3Failure::Auth { code, message },
            _ if status == Some(301) => S3Failure::WrongRegion { region: region() },
            _ => S3Failure::Service { code, message },
        };
    }
    if let Some(status) = status {
        return match status {
            301 => S3Failure::WrongRegion { region: region() },
            404 => S3Failure::NotFound {
                code: "NotFound".to_owned(),
                message: "the bucket or key does not exist".to_owned(),
            },
            416 => S3Failure::InvalidRange,
            _ => S3Failure::Service {
                code: format!("HTTP {status}"),
                message: format!("the endpoint answered HTTP {status} without an S3 error"),
            },
        };
    }
    let message = DisplayErrorContext(&error).to_string();
    match &error {
        SdkError::TimeoutError(_) => S3Failure::Timeout,
        SdkError::DispatchFailure(failure) if failure.is_timeout() => S3Failure::Timeout,
        SdkError::DispatchFailure(_)
        | SdkError::ConstructionFailure(_)
        | SdkError::ResponseError(_)
        | SdkError::ServiceError(_) => S3Failure::Transport { message },
        // SdkError is non-exhaustive; a variant added later is still a transport failure.
        _ => S3Failure::Transport { message },
    }
}

/// The hint for a bucket in another region, naming the region when the reply did.
pub(crate) fn wrong_region_hint(region: Option<&str>) -> String {
    match region {
        Some(r) => format!("the bucket is in {r}; set region = \"{r}\" on the connection"),
        None => "the bucket is in another region; set region on the connection".to_owned(),
    }
}

/// A statement's error for `failure`: the S3 code and message, with a hint where one helps.
pub(crate) fn execute_error(failure: S3Failure) -> ExecuteError {
    let (code, message, hint) = match failure {
        S3Failure::Timeout => return ExecuteError::Timeout,
        S3Failure::NotFound { code, message } | S3Failure::Service { code, message } => {
            (code, message, None)
        }
        S3Failure::AccessDenied { message } => ("AccessDenied".to_owned(), message, None),
        S3Failure::WrongRegion { region } => (
            "PermanentRedirect".to_owned(),
            "the bucket must be addressed in another region".to_owned(),
            Some(wrong_region_hint(region.as_deref())),
        ),
        S3Failure::Auth { code, message } => (
            code,
            message,
            Some("check `access_key_id` and its secret".to_owned()),
        ),
        S3Failure::InvalidRange => (
            "InvalidRange".to_owned(),
            "the requested range is not satisfiable".to_owned(),
            None,
        ),
        S3Failure::Transport { message } => ("transport".to_owned(), message, None),
    };
    let mut error = ExecuteError::database(code, message);
    if let ExecuteError::Database { hint: slot, .. } = &mut error {
        *slot = hint;
    }
    error
}

/// The object browser's error for `failure` on `key`, given as `s3://bucket/key`.
pub(crate) fn browse_error(failure: S3Failure, key: &str) -> ObjectBrowseError {
    let database =
        |code: String, message: String, hint: Option<String>| ObjectBrowseError::Database {
            code,
            message,
            hint,
        };
    match failure {
        S3Failure::NotFound { .. } => ObjectBrowseError::NotFound {
            key: key.to_owned(),
        },
        S3Failure::AccessDenied { message } => ObjectBrowseError::AccessDenied { message },
        S3Failure::WrongRegion { region } => ObjectBrowseError::WrongRegion {
            hint: wrong_region_hint(region.as_deref()),
        },
        S3Failure::Auth { code, message } => database(
            code,
            message,
            Some("check `access_key_id` and its secret".to_owned()),
        ),
        S3Failure::InvalidRange => database(
            "InvalidRange".to_owned(),
            "the requested range is not satisfiable".to_owned(),
            None,
        ),
        S3Failure::Service { code, message } => database(code, message, None),
        S3Failure::Timeout => ObjectBrowseError::Timeout,
        S3Failure::Transport { message } => database("transport".to_owned(), message, None),
    }
}

#[cfg(test)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "a test's catch-all arm only prints the unexpected error"
)]
mod tests;
