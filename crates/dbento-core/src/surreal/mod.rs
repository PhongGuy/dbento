//! SurrealDB: the SurrealQL lexer, units and classifier, the CBOR decoder and cell mapping, and
//! the WebSocket RPC client.
#![allow(dead_code)] // plan 2's driver is the first caller
#[cfg(feature = "surrealdb")]
pub(crate) mod cancel;
#[cfg(feature = "surrealdb")]
pub(crate) mod cell;
pub(crate) mod classify;
#[cfg(feature = "surrealdb")]
pub(crate) mod decode;
#[cfg(feature = "surrealdb")]
pub(crate) mod driver;
pub(crate) mod lex;
#[cfg(feature = "surrealdb")]
pub(crate) mod login;
#[cfg(feature = "surrealdb")]
pub(crate) mod rpc;
#[cfg(feature = "surrealdb")]
pub(crate) mod serve;
#[cfg(feature = "surrealdb")]
pub(crate) mod shape;
pub(crate) mod units;
#[cfg(feature = "surrealdb")]
pub(crate) mod wire;
