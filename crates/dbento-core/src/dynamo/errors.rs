//! DynamoDB failures as the session reports them: statement errors and the connect ping's.

use aws_sdk_dynamodb::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};

use super::client::CallError;
use super::ops::DynamoOp;
use crate::session::{ConnectError, ExecuteError};

/// The hint on every throttling error.
pub(crate) const THROTTLED_HINT: &str = "the table's capacity is exhausted; retry later";
/// The hint on a ValidationException for a request that carried a `NextToken`.
pub(crate) const STALE_TOKEN_HINT: &str = "run the query again";

const THROTTLING: [&str; 3] = [
    "ProvisionedThroughputExceededException",
    "RequestLimitExceeded",
    "ThrottlingException",
];
const AUTH: [&str; 4] = [
    "UnrecognizedClientException",
    "InvalidClientTokenId",
    "InvalidSignatureException",
    "ExpiredTokenException",
];

/// What went wrong, independent of whether a statement or the ping asked.
enum Failure {
    Refused(DynamoOp),
    Timeout(String),
    Unreachable(String),
    Service { code: String, message: String },
    Http(u16),
    Other(String),
}

/// Sorts a call failure: a coded reply first, then a codeless reply by its status, then the
/// transport. A dispatch that timed out counts as a timeout, any other as unreachable.
fn classify<E>(error: CallError<E>) -> Failure
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
{
    let error = match error {
        CallError::Refused(op) => return Failure::Refused(op),
        CallError::Sdk(error) => *error,
    };
    if let Some(code) = error.code() {
        return Failure::Service {
            code: code.to_owned(),
            message: error.message().unwrap_or_default().to_owned(),
        };
    }
    if let Some(response) = error.raw_response() {
        return Failure::Http(response.status().as_u16());
    }
    let cause = DisplayErrorContext(&error).to_string();
    match &error {
        SdkError::TimeoutError(_) => Failure::Timeout(cause),
        SdkError::DispatchFailure(failure) if failure.is_timeout() => Failure::Timeout(cause),
        SdkError::DispatchFailure(_) => Failure::Unreachable(cause),
        SdkError::ConstructionFailure(_)
        | SdkError::ResponseError(_)
        | SdkError::ServiceError(_) => Failure::Other(cause),
        // SdkError is non-exhaustive; a variant added later is still a transport failure.
        _ => Failure::Other(cause),
    }
}

/// SDK and gate failures of a statement or catalog call.
pub(crate) fn execute_error<E>(error: CallError<E>) -> ExecuteError
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
{
    match classify(error) {
        Failure::Refused(op) => ExecuteError::ReadOnlyViolation {
            message: format!("{} is not allowed on a read-only connection", op.name()),
        },
        Failure::Timeout(_) => ExecuteError::Timeout,
        Failure::Service { code, message } => {
            let throttled = THROTTLING.contains(&code.as_str());
            let error = ExecuteError::database(code, message);
            if throttled {
                with_hint(error, THROTTLED_HINT)
            } else {
                error
            }
        }
        Failure::Http(status) => ExecuteError::database(
            format!("HTTP {status}"),
            format!("the endpoint answered HTTP {status} with a body that is not DynamoDB JSON"),
        ),
        Failure::Unreachable(cause) | Failure::Other(cause) => {
            ExecuteError::database("transport", cause)
        }
    }
}

/// A ValidationException on a request that carried a NextToken gains STALE_TOKEN_HINT.
pub(crate) fn stale_token(error: ExecuteError) -> ExecuteError {
    if let ExecuteError::Database { code, .. } = &error
        && code == "ValidationException"
    {
        return with_hint(error, STALE_TOKEN_HINT);
    }
    error
}

fn with_hint(mut error: ExecuteError, text: &str) -> ExecuteError {
    if let ExecuteError::Database { hint, .. } = &mut error {
        *hint = Some(text.to_owned());
    }
    error
}

/// The ping's failure; None for AccessDeniedException, since the keys are valid.
pub(crate) fn connect_error<E>(error: CallError<E>, target: &str) -> Option<ConnectError>
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
{
    let message = match classify(error) {
        Failure::Service { code, .. } if code == "AccessDeniedException" => return None,
        Failure::Service { code, message } if AUTH.contains(&code.as_str()) => {
            format!("{target}: {code}: {message}; check `access_key_id` and its secret")
        }
        Failure::Service { code, message } => format!("{target}: {code}: {message}"),
        Failure::Timeout(cause) | Failure::Unreachable(cause) => {
            format!("{target}: cannot reach the endpoint: {cause}")
        }
        Failure::Http(status) => format!(
            "{target}: the endpoint answered HTTP {status} with a body that is not DynamoDB JSON"
        ),
        Failure::Refused(op) => format!("{target}: {} was refused", op.name()),
        Failure::Other(cause) => format!("{target}: {cause}"),
    };
    Some(ConnectError::Open { message })
}

#[cfg(test)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "a test's catch-all arm only prints the unexpected error"
)]
mod tests;
