//! The dbento-testkit integration suite.
//!
//! Each file beside this one is a module of this single binary, so the
//! suite links once. A new test file must be declared below, or it never runs.
// Test helpers may panic on a broken fixture too; clippy's in-tests exemption covers only
// #[test] functions, not the helpers they call.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod harness;
mod mongo_harness;
mod mssql_harness;
mod mysql_sweep;
mod mysql_wire;
mod redis_harness;
