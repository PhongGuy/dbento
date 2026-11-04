//! SurrealDB: the SurrealQL lexer, units and classifier, and the CBOR decoder and cell mapping.
#![allow(dead_code)] // plan 2's driver is the first caller
#[cfg(feature = "surrealdb")]
pub(crate) mod cell;
pub(crate) mod classify;
#[cfg(feature = "surrealdb")]
pub(crate) mod decode;
pub(crate) mod lex;
pub(crate) mod units;
