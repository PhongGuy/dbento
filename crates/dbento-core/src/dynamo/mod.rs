//! DynamoDB: PartiQL scripts, their classification, item rendering and credentials.
#![allow(dead_code)] // the DynamoDB driver is the only caller of most of this
pub(crate) mod classify;
#[cfg(feature = "dynamodb")]
pub(crate) mod client;
#[cfg(feature = "dynamodb")]
pub(crate) mod columns;
pub(crate) mod creds;
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
