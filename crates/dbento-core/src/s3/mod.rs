//! S3 object browsing: the command language and its helpers.

// Most of this has no caller until the S3 driver lands.
#![allow(dead_code, unused_imports)]

pub(crate) mod browse;
#[cfg(feature = "s3")]
pub(crate) mod client;
pub(crate) mod lex;
pub(crate) mod names;
pub(crate) mod ops;
#[cfg(all(test, feature = "s3"))]
pub(crate) mod replay;

pub use browse::{ObjectKey, ObjectMeta};
