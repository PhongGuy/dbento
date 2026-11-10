//! DynamoDB: PartiQL scripts, their classification, item rendering and credentials.
// Without the driver, most of this has no caller.
#![cfg_attr(not(feature = "dynamodb"), allow(dead_code))]
#[cfg(feature = "dynamodb")]
pub(crate) mod catalog;
pub(crate) mod classify;
#[cfg(feature = "dynamodb")]
pub(crate) mod client;
#[cfg(feature = "dynamodb")]
pub(crate) mod columns;
pub(crate) mod creds;
#[cfg(feature = "dynamodb")]
pub(crate) mod driver;
#[cfg(feature = "dynamodb")]
pub(crate) mod errors;
pub(crate) mod lex;
pub(crate) mod ops;
#[cfg(feature = "dynamodb")]
pub(crate) mod render;
#[cfg(test)]
#[cfg(feature = "dynamodb")]
pub(crate) mod replay;
pub(crate) mod statement;
#[cfg(feature = "dynamodb")]
pub(crate) mod stream;
