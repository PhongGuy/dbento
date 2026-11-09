//! The dbento-testkit integration suite.
//!
//! Each file beside this one is a module of this single binary, so the
//! suite links once. A new test file must be declared below, or it never runs.

mod harness;
mod mongo_harness;
mod mssql_harness;
mod mysql_sweep;
mod mysql_wire;
mod redis_harness;
