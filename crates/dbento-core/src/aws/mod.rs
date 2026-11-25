//! Region, credential and HTTP helpers shared by the AWS engines.

mod cancel;
mod creds;
mod http;
mod retry;

pub(crate) use cancel::AwsCancel;
pub(crate) use creds::{AwsCredentials, resolve_credentials, resolve_region};
pub(crate) use http::{endpoint_origin, https_client};
pub(crate) use retry::own_retry_quota;
#[cfg(test)]
pub(crate) mod replay;
