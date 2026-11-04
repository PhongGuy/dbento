//! MongoDB: the mongosh-subset lexer, parser and classifier, the `uri` rules and the driver.
#![cfg_attr(not(feature = "mongodb"), allow(dead_code))] // the driver is the only caller
#[cfg(feature = "mongodb")]
pub(crate) mod catalog;
#[cfg(feature = "mongodb")]
pub(crate) mod classify;
#[cfg(feature = "mongodb")]
pub(crate) mod columns;
#[cfg(feature = "mongodb")]
pub(crate) mod driver;
#[cfg(feature = "mongodb")]
pub(crate) mod exec;
pub(crate) mod lex;
#[cfg(feature = "mongodb")]
pub(crate) mod parse;
#[cfg(feature = "mongodb")]
pub(crate) mod render;
pub(crate) mod split;
#[cfg(feature = "mongodb")]
pub(crate) mod stream;
pub(crate) mod uri;
#[cfg(feature = "mongodb")]
pub(crate) mod value;
