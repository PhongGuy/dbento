//! DynamoDB: PartiQL scripts, their classification, item rendering and credentials.
#![allow(dead_code)] // the DynamoDB driver is the only caller of most of this
pub(crate) mod classify;
pub(crate) mod lex;
pub(crate) mod ops;
pub(crate) mod statement;
