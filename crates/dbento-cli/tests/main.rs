//! The dbento-cli integration suite.
//!
//! Each file beside this one is a module of this single binary, so the
//! suite links once. A new test file must be declared below, or it never runs.
// Test helpers may panic on a broken fixture too; clippy's in-tests exemption covers only
// #[test] functions, not the helpers they call.
#![allow(clippy::unwrap_used)]

mod cli_chinook;
mod cli_duckdb;
mod cli_formats;
mod cli_import;
mod cli_import_beekeeper;
mod cli_import_beekeeper_e2e;
mod cli_import_dbeaver;
mod cli_import_dbeaver_e2e;
mod cli_import_e2e;
mod cli_import_mysql_e2e;
mod cli_interrupt;
mod cli_mcp;
mod cli_mongo;
mod cli_mssql_safety;
mod cli_mssql_scripts;
mod cli_mssql_torture;
mod cli_mysql;
mod cli_mysql_safety;
mod cli_mysql_torture;
mod cli_pg_torture;
mod cli_postgres;
mod cli_query;
mod cli_redis;
mod cli_run_secret;
mod cli_safety;
mod cli_surreal;
mod cli_torture;
mod cli_workspace;
mod common;
