//! The xtask integration suite.
//!
//! Each file beside this one is a module of this single binary, so the
//! suite links once. A new test file must be declared below, or it never runs.
#![warn(clippy::wildcard_enum_match_arm)]
// Test helpers may panic on a broken fixture too; clippy's in-tests exemption covers only
// #[test] functions, not the helpers they call.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod app_bench;
mod bench;
mod datasets;
mod db;
mod demo;
mod dependencies;
mod icons;
mod install;
mod notices;
mod packager_config;
mod packager_metadata;
mod stress;
mod test_binaries;
mod windows_check;
