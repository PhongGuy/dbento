//! Team mode's git layer: reading what a Potluck server says when it refuses a push.

mod refusal;

pub use refusal::{Finding, Refusal, parse_refusal, redact_token, rule_text};
