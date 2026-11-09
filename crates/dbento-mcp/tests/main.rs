//! The dbento-mcp integration suite.
//!
//! Each file beside this one is a module of this single binary, so the
//! suite links once. A new test file must be declared below, or it never runs.

mod cancel;
mod common;
mod duckdb;
mod execute;
mod execute_grant;
mod execute_pg;
mod explain;
mod history;
mod introspect;
mod list_connections;
mod mongo;
mod mssql;
mod mysql;
mod query;
mod redis;
mod saved_queries;
mod surreal;
