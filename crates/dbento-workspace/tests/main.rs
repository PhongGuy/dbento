//! The dbento-workspace integration suite.
//!
//! Each file beside this one is a module of this single binary, so the
//! suite links once. A new test file must be declared below, or it never runs.
// Test helpers may panic on a broken fixture too; clippy's in-tests exemption covers only
// #[test] functions, not the helpers they call.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![warn(clippy::wildcard_enum_match_arm)]

mod duration;
mod folder_path;
mod same_login;
mod secret_errors;
mod session_binding;
mod ssh;
mod workspace;
mod workspace_extra;
mod workspace_files;
mod workspace_folders;
