//! Redis and Valkey: command lines, the server's command table, write classification, reply rows
//! and the driver.
#![allow(dead_code)] // until the driver calls these modules
pub(crate) mod browse;
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
