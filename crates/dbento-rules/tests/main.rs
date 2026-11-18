//! The dbento-rules integration suite.
//!
//! Each file beside this one is a module of this single binary, so the suite links once. A new
//! test file must be declared below, or it never runs.

// Test helpers panic by design; a failed unwrap or expect there is a failed test.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_assert_message
)]

mod common;
mod contract;
mod detect;
mod paths;
