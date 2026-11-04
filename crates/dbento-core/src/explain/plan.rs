//! Plans as a pre-order node list, parsed from Postgres `FORMAT JSON` output or from the
//! rows of SQLite's `EXPLAIN QUERY PLAN`.

use std::collections::HashMap;
use std::fmt;

use serde::Deserialize;
use serde::de::{MapAccess, Visitor};
use serde_json::Value;

/// What the database printed for an explain, and the plan read from it when it parses.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ExplainOutput {
    /// The database's output as text, shown in the Raw view.
    pub raw: String,
    /// The plan, or why `raw` could not be read as one.
    pub plan: Result<Plan, PlanParseError>,
}

impl ExplainOutput {
    /// Keeps Postgres's JSON text and parses it.
    pub fn from_postgres(raw: String) -> ExplainOutput {
        let plan = Plan::from_postgres_json(&raw);
        ExplainOutput { raw, plan }
    }

    /// Builds the output from SQLite's `(id, parent, notused, detail)` rows; `raw` holds
    /// each row tab-separated, one per line.
    pub fn from_sqlite(rows: &[(i64, i64, i64, String)]) -> ExplainOutput {
        let raw = rows
            .iter()
            .map(|(id, parent, notused, detail)| format!("{id}\t{parent}\t{notused}\t{detail}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tree: Vec<_> = rows
            .iter()
            .map(|(id, parent, _, detail)| (*id, *parent, detail.clone()))
            .collect();
        ExplainOutput {
            raw,
            plan: Ok(Plan::from_sqlite_rows(&tree)),
        }
    }
}

/// Postgres output that is not a plan.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct PlanParseError {
    /// What was wrong with the output.
    pub message: String,
}

impl PlanParseError {
    fn new(message: impl Into<String>) -> PlanParseError {
        PlanParseError {
            message: message.into(),
        }
    }
}

/// A plan's nodes in pre-order: each parent comes before its children.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// The nodes, the root first.
    pub nodes: Vec<PlanNode>,
    /// Postgres's planning time, under ANALYZE.
    pub planning_ms: Option<f64>,
    /// Postgres's execution time, under ANALYZE.
    pub execution_ms: Option<f64>,
    /// Some node carries actual timings.
    pub analyzed: bool,
}

/// One plan node. SQLite nodes carry only `node_type`, its detail line.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct PlanNode {
    /// The parent's index in `Plan::nodes`; `None` for a root.
    pub parent: Option<usize>,
    /// The number of ancestors.
    pub depth: usize,
    /// Postgres's `Node Type`, or SQLite's detail line.
    pub node_type: String,
    /// The table scanned.
    pub relation: Option<String>,
    /// The index used.
    pub index: Option<String>,
    /// The name the query gives the relation.
    pub alias: Option<String>,
    /// Inner, Left, Semi and the like, for a join.
    pub join_type: Option<String>,
    /// The estimated cost before the first row.
    pub startup_cost: Option<f64>,
    /// The estimated cost of all rows.
    pub total_cost: Option<f64>,
    /// The estimated row count.
    pub rows: Option<f64>,
    /// The estimated row width in bytes.
    pub width: Option<u64>,
    /// Measured figures, under ANALYZE.
    pub actual: Option<Actual>,
    /// Shared buffer blocks found in cache, under BUFFERS.
    pub shared_hit: Option<u64>,
    /// Shared buffer blocks read from disk, under BUFFERS.
    pub shared_read: Option<u64>,
    /// Every other key, as text, in the order Postgres sent them.
    pub detail: Vec<(String, String)>,
}

/// A node's measured figures.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Actual {
    /// Time per loop, in milliseconds.
    pub total_ms: f64,
    /// Rows per loop.
    pub rows: f64,
    /// How many times the node ran.
    pub loops: u64,
    /// Time over all loops spent in this node and not its children, never below 0.
    pub self_ms: f64,
}

impl Plan {
    /// Parses the JSON `EXPLAIN (FORMAT JSON)` prints. Fails when the top level is not a
    /// non-empty array, its first element has no object `Plan`, or a node has no string
    /// `Node Type`.
    pub fn from_postgres_json(raw: &str) -> Result<Plan, PlanParseError> {
        let top: Vec<Top> =
            serde_json::from_str(raw).map_err(|e| PlanParseError::new(e.to_string()))?;
        let first = top
            .into_iter()
            .next()
            .ok_or_else(|| PlanParseError::new("the plan output is an empty array"))?;
        let root = first
            .plan
            .ok_or_else(|| PlanParseError::new("the plan output has no Plan object"))?;
        let mut nodes = Vec::new();
        flatten(root, None, 0, &mut nodes)?;
        settle_self_time(&mut nodes);
        Ok(Plan {
            analyzed: nodes.iter().any(|n| n.actual.is_some()),
            nodes,
            planning_ms: first.planning_ms,
            execution_ms: first.execution_ms,
        })
    }

    /// Nests SQLite's `(id, parent, detail)` rows, which arrive parent first. A parent of 0
    /// or one never seen makes the row a root.
    pub fn from_sqlite_rows(rows: &[(i64, i64, String)]) -> Plan {
        let mut seen: HashMap<i64, usize> = HashMap::new();
        let mut nodes: Vec<PlanNode> = Vec::with_capacity(rows.len());
        for (id, parent, detail) in rows {
            let parent = seen.get(parent).copied().filter(|_| *parent != 0);
            let depth = parent.map_or(0, |p| nodes[p].depth + 1);
            seen.insert(*id, nodes.len());
            nodes.push(PlanNode::bare(parent, depth, detail.clone()));
        }
        Plan {
            nodes,
            planning_ms: None,
            execution_ms: None,
            analyzed: false,
        }
    }

    /// The node's self time as a share of the execution time, at most 1. `None` without
    /// an actual or a positive execution time.
    pub fn self_share(&self, ix: usize) -> Option<f64> {
        let actual = self.nodes.get(ix)?.actual?;
        let total = self.execution_ms.filter(|ms| *ms > 0.0)?;
        Some((actual.self_ms / total).min(1.0))
    }
}

impl PlanNode {
    fn bare(parent: Option<usize>, depth: usize, node_type: String) -> PlanNode {
        PlanNode {
            parent,
            depth,
            node_type,
            relation: None,
            index: None,
            alias: None,
            join_type: None,
            startup_cost: None,
            total_cost: None,
            rows: None,
            width: None,
            actual: None,
            shared_hit: None,
            shared_read: None,
            detail: Vec::new(),
        }
    }
}

/// The first element of Postgres's output array; other keys (JIT, Triggers) are ignored.
#[derive(Deserialize)]
struct Top {
    #[serde(rename = "Plan")]
    plan: Option<RawNode>,
    #[serde(rename = "Planning Time")]
    planning_ms: Option<f64>,
    #[serde(rename = "Execution Time")]
    execution_ms: Option<f64>,
}

/// A node object with its keys in the order received; serde_json's own map sorts them.
struct RawNode {
    fields: Vec<(String, Value)>,
    plans: Vec<RawNode>,
}

impl<'de> Deserialize<'de> for RawNode {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct NodeVisitor;
        impl<'de> Visitor<'de> for NodeVisitor {
            type Value = RawNode;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a plan node object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawNode, A::Error> {
                let mut node = RawNode {
                    fields: Vec::new(),
                    plans: Vec::new(),
                };
                while let Some(key) = map.next_key::<String>()? {
                    if key == "Plans" {
                        node.plans = map.next_value()?;
                    } else {
                        node.fields.push((key, map.next_value()?));
                    }
                }
                Ok(node)
            }
        }
        de.deserialize_map(NodeVisitor)
    }
}

/// Pushes `raw` and then its children depth first, so each index is a pre-order position.
fn flatten(
    raw: RawNode,
    parent: Option<usize>,
    depth: usize,
    out: &mut Vec<PlanNode>,
) -> Result<(), PlanParseError> {
    let mut fields = raw.fields;
    let mut take = |key: &str| {
        fields
            .iter()
            .position(|(k, _)| k == key)
            .map(|i| fields.remove(i).1)
    };
    let text = |v: Option<Value>| match v {
        Some(Value::String(s)) => Some(s),
        _ => None,
    };
    let node_type = text(take("Node Type"))
        .ok_or_else(|| PlanParseError::new("a plan node has no string Node Type"))?;
    let mut node = PlanNode::bare(parent, depth, node_type);
    node.relation = text(take("Relation Name"));
    node.index = text(take("Index Name"));
    node.alias = text(take("Alias"));
    node.join_type = text(take("Join Type"));
    node.startup_cost = take("Startup Cost").and_then(|v| v.as_f64());
    node.total_cost = take("Total Cost").and_then(|v| v.as_f64());
    node.rows = take("Plan Rows").and_then(|v| v.as_f64());
    node.width = take("Plan Width").and_then(|v| v.as_u64());
    let total_ms = take("Actual Total Time").and_then(|v| v.as_f64());
    let actual_rows = take("Actual Rows").and_then(|v| v.as_f64());
    let loops = take("Actual Loops").and_then(|v| v.as_u64());
    node.actual = total_ms.map(|total_ms| Actual {
        total_ms,
        rows: actual_rows.unwrap_or(0.0),
        loops: loops.unwrap_or(1),
        self_ms: 0.0,
    });
    node.shared_hit = take("Shared Hit Blocks").and_then(|v| v.as_u64());
    node.shared_read = take("Shared Read Blocks").and_then(|v| v.as_u64());
    node.detail = fields
        .into_iter()
        .map(|(k, v)| (k, detail_text(v)))
        .collect();

    let ix = out.len();
    out.push(node);
    for child in raw.plans {
        flatten(child, Some(ix), depth + 1, out)?;
    }
    Ok(())
}

/// Sets each node's self time: its time over all loops less its children's, at least 0.
fn settle_self_time(nodes: &mut [PlanNode]) {
    let spent = |n: &PlanNode| n.actual.map_or(0.0, |a| a.total_ms * a.loops as f64);
    let mut own: Vec<f64> = nodes.iter().map(spent).collect();
    for n in nodes.iter() {
        if let Some(p) = n.parent {
            own[p] -= spent(n);
        }
    }
    for (n, own) in nodes.iter_mut().zip(own) {
        if let Some(a) = n.actual.as_mut() {
            a.self_ms = own.max(0.0);
        }
    }
}

/// A detail value as shown: strings as themselves, string lists joined, the rest as JSON.
fn detail_text(v: Value) -> String {
    match v {
        Value::String(s) => s,
        Value::Array(items) if items.iter().all(Value::is_string) => items
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}
