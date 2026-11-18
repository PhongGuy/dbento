//! The safety-relaxation rules: a push that weakens a production safeguard of a connection that
//! exists on both sides of `dbento.toml`.

use std::collections::BTreeMap;

use dbento_workspace::{ConnectionConfig, Workspace};
use potluck_kind::{Code, Finding};

use crate::detail::{MAX_DETAIL, cap, segment};

// A production safeguard: the rule that reports dropping it, its key, and whether a connection
// holds it in effect.
struct Safeguard {
    rule: &'static str,
    key: &'static str,
    held: fn(&ConnectionConfig) -> bool,
}

// Report order: read_only first, then the production colour.
const SAFEGUARDS: [Safeguard; 2] = [
    Safeguard {
        rule: "relax_read_only",
        key: "read_only",
        held: ConnectionConfig::read_only,
    },
    Safeguard {
        rule: "relax_color",
        key: "color",
        held: ConnectionConfig::is_production,
    },
];

/// One finding per relaxed safeguard, `relax_read_only` before `relax_color`, each listing the
/// relaxed connections' keys in name order.
pub(crate) fn relaxations(old: &Workspace, new: &Workspace) -> Vec<Finding> {
    let new_by_name: BTreeMap<&str, &ConnectionConfig> =
        new.connections().iter().map(|c| (c.name(), c)).collect();
    let shared: BTreeMap<&str, (&ConnectionConfig, &ConnectionConfig)> = old
        .connections()
        .iter()
        .filter_map(|o| Some((o.name(), (o, *new_by_name.get(o.name())?))))
        .collect();

    SAFEGUARDS
        .iter()
        .filter_map(|guard| {
            let entries: Vec<String> = shared
                .iter()
                .filter(|(_, (o, n))| (guard.held)(o) && !(guard.held)(n))
                .map(|(name, _)| format!("connections.{}.{}", segment(name), guard.key))
                .collect();
            (!entries.is_empty()).then(|| {
                Finding::new(Code::SafetyRelaxation, guard.rule, "dbento.toml")
                    .with_detail(&join_within_cap(&entries))
            })
        })
        .collect()
}

// Whole entries only, so a reader never sees half a key; a lone oversize entry is cut instead.
fn join_within_cap(entries: &[String]) -> String {
    let mut out = String::new();
    for entry in entries {
        let added = if out.is_empty() {
            entry.len()
        } else {
            entry.len() + 1
        };
        if out.len() + added > MAX_DETAIL {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(entry);
    }
    if out.is_empty() {
        return entries.first().map_or_else(String::new, |e| cap(e.clone()));
    }
    out
}
