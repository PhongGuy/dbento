//! A retry quota per client, shared by the AWS engines.

// Both SDKs re-export the same aws-smithy-runtime types; take them from whichever is built.
#[cfg(feature = "dynamodb")]
use aws_sdk_dynamodb::config::retry::{RetryPartition, TokenBucket};
#[cfg(all(feature = "s3", not(feature = "dynamodb")))]
use aws_sdk_s3::config::retry::{RetryPartition, TokenBucket};

/// A retry quota this client alone draws on, named `dbento-<service>-<region>`. The SDK's
/// default partition is one bucket per region for the whole process, so one connection's
/// failures could refuse every other's retries. The costs are those the SDK's Retry Behavior
/// 2.1 gives its default bucket.
pub(crate) fn own_retry_quota(service: &str, region: &str) -> RetryPartition {
    let bucket = TokenBucket::builder()
        .retry_cost(14)
        .throttling_retry_cost(5)
        .timeout_retry_cost(14)
        .build();
    RetryPartition::custom(format!("dbento-{service}-{region}"))
        .token_bucket(bucket)
        .build()
}
