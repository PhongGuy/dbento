//! The dbento-core team-mode suite: git against temp dirs over file:// and a loopback API stub.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod api;
mod clone;
mod commit;
mod potluck;
mod pull;
mod push;
mod refusal;
mod runner;
mod support;
mod sync;
