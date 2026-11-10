//! Whether a PartiQL statement writes, judged from its first keyword alone.

use super::statement::{Kind, PartiqlStatement};
use crate::session::{Classification, Writes};

/// A select reads, an insert, update or delete writes, and anything else is unknown.
pub(crate) fn partiql_classification(s: &PartiqlStatement) -> Classification {
    let writes = match s.kind {
        Kind::Select => Writes::No,
        Kind::Insert | Kind::Update | Kind::Delete => Writes::Yes,
        Kind::Other => Writes::Unknown,
    };
    Classification {
        writes,
        unguarded: Vec::new(),
        transaction_control: false,
        categories: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
