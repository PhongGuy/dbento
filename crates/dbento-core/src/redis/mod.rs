//! Redis and Valkey: command lines, the server's command table, write classification, reply rows
//! and the driver.
#![cfg_attr(not(feature = "redis"), allow(dead_code))] // the driver is the only caller
pub(crate) mod browse;
pub(crate) mod catalog;
#[cfg(feature = "redis")]
pub(crate) mod classify;
#[cfg(feature = "redis")]
pub(crate) mod driver;
pub(crate) mod lex;
#[cfg(feature = "redis")]
pub(crate) mod probe;
#[cfg(feature = "redis")]
pub(crate) mod shape;
#[cfg(feature = "redis")]
pub(crate) mod stream;
#[cfg(feature = "redis")]
pub(crate) mod table;
