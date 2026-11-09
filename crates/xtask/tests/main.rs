//! The xtask integration suite.
//!
//! Each file beside this one is a module of this single binary, so the
//! suite links once. A new test file must be declared below, or it never runs.

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
