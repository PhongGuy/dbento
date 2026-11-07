//! Redis and Valkey: command lines, the server's command table, write classification and reply rows.
#![allow(dead_code)] // until the driver calls these modules
#[cfg(feature = "redis")]
pub(crate) mod classify;
pub(crate) mod lex;
#[cfg(feature = "redis")]
pub(crate) mod table;
