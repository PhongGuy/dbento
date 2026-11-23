//! S3 object browsing: the command language and its helpers.

// Most of this has no caller until the S3 driver lands.
#![allow(dead_code, unused_imports)]

pub(crate) mod lex;
pub(crate) mod names;
pub(crate) mod ops;
