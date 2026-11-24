//! Region, credential and HTTP helpers shared by the AWS engines.
// With only S3, nothing calls these until its driver lands.
#![cfg_attr(not(feature = "dynamodb"), allow(dead_code, unused_imports))]

mod creds;
mod http;

pub(crate) use creds::{AwsCredentials, resolve_credentials, resolve_region};
pub(crate) use http::{endpoint_origin, https_client};
#[cfg(test)]
pub(crate) mod replay;
