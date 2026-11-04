//! MongoDB: the mongosh-subset lexer, splitter, parser and classifier, and the `uri` rules.
#![cfg_attr(not(test), allow(dead_code))] // plan 1b's driver is the first caller
#[cfg(feature = "mongodb")]
pub(crate) mod classify;
pub(crate) mod lex;
#[cfg(feature = "mongodb")]
pub(crate) mod parse;
pub(crate) mod split;
pub(crate) mod uri;
#[cfg(feature = "mongodb")]
pub(crate) mod value;
