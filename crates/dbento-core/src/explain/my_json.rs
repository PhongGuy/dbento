//! The ordered walk of MySQL's and MariaDB's `EXPLAIN FORMAT=JSON` output into a pre-order
//! node list.
//! Every object becomes a node, so no subtree is ever dropped; scalars become detail.

use serde_json::{Map, Value};

use super::plan::{Actual, Plan, PlanNode, PlanParseError, detail_text, settle_self_time};

/// Which server printed the JSON; it decides the node names and the mapped keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JsonFlavor {
    /// MySQL's format version 1.
    Mysql,
    /// MariaDB's, where `ANALYZE FORMAT=JSON` adds `r_*` actuals.
    Mariadb,
}

/// Objects folded into their node's detail as `outer.inner` keys instead of becoming nodes.
const FOLDED: &[&str] = &["cost_info", "r_engine_stats", "query_optimization"];

/// MariaDB keys that mark a node as measured.
const TIMED: &[&str] = &["r_loops", "r_total_time_ms", "r_table_time_ms"];

/// MariaDB keys that become the node's actual when it is measured, and so leave detail.
const MEASURED: &[&str] = &["r_loops", "r_rows", "r_total_time_ms"];

/// Parses `raw` into a plan. Fails when it is not JSON, the top is not an object, or the
/// top has no object `query_block`.
pub(super) fn parse(raw: &str, flavor: JsonFlavor) -> Result<Plan, PlanParseError> {
    let fail = |message: String| PlanParseError { message };
    let top: Value = serde_json::from_str(raw).map_err(|e| fail(e.to_string()))?;
    let Value::Object(mut top) = top else {
        return Err(fail("the plan output is not a JSON object".to_owned()));
    };
    let Some(Value::Object(block)) = top.shift_remove("query_block") else {
        let hint = match flavor {
            JsonFlavor::Mysql => "; set explain_json_format_version to 1",
            JsonFlavor::Mariadb => "",
        };
        return Err(fail(format!(
            "the plan output has no query_block object{hint}"
        )));
    };
    // MariaDB times the optimizer and the whole statement; MySQL's JSON has neither key.
    let execution_ms = block.get("r_total_time_ms").and_then(number);
    let planning_ms = match top.get_mut("query_optimization") {
        Some(Value::Object(opt)) => opt
            .shift_remove("r_total_time_ms")
            .as_ref()
            .and_then(number),
        Some(_) | None => None,
    };
    let mut nodes = Vec::new();
    walk("query_block", block, None, 0, flavor, &mut nodes);
    // Keys beside query_block describe the whole statement, so the root keeps them.
    if let Some(root) = nodes.first_mut() {
        for (k, v) in top {
            match (k.as_str(), v) {
                (name, Value::Object(inner)) if FOLDED.contains(&name) => fold(root, &k, inner),
                (_, v) => push_detail(&mut root.detail, k, v),
            }
        }
    }
    settle_self_time(&mut nodes);
    Ok(Plan {
        analyzed: nodes.iter().any(|n| n.actual.is_some()),
        nodes,
        planning_ms,
        execution_ms,
    })
}

/// Pushes the node `obj` found under `key`, then its children depth first. Object values
/// and arrays of objects are children; everything else is detail, in the order received.
fn walk(
    key: &str,
    obj: Map<String, Value>,
    parent: Option<usize>,
    depth: usize,
    flavor: JsonFlavor,
    out: &mut Vec<PlanNode>,
) {
    let mut node = PlanNode::bare(parent, depth, node_name(key, &obj));
    let mariadb = flavor == JsonFlavor::Mariadb;
    node.actual = if mariadb { mariadb_actual(&obj) } else { None };
    let measured = node.actual.is_some();
    let mut children: Vec<Child> = Vec::new();
    for (k, v) in obj {
        match (k.as_str(), v) {
            (name, Value::Object(inner)) if FOLDED.contains(&name) => fold(&mut node, &k, inner),
            ("table_name", Value::String(s)) if node.relation.is_none() => node.relation = Some(s),
            ("key", Value::String(s)) if node.index.is_none() => node.index = Some(s),
            ("rows_examined_per_scan", v @ Value::Number(_)) if !mariadb && node.rows.is_none() => {
                node.rows = v.as_f64();
            }
            ("rows", v @ Value::Number(_)) if mariadb && node.rows.is_none() => {
                node.rows = v.as_f64();
            }
            ("cost", v) if mariadb && node.total_cost.is_none() && number(&v).is_some() => {
                node.total_cost = number(&v);
            }
            (name, _) if measured && MEASURED.contains(&name) => {}
            (_, Value::Object(inner)) => children.push(Child::One(k, inner)),
            (_, Value::Array(items)) if is_node_list(&items) => {
                children.push(Child::List(k, items))
            }
            (_, v) => push_detail(&mut node.detail, k, v),
        }
    }
    let ix = out.len();
    out.push(node);
    for child in children {
        match child {
            Child::One(k, inner) => walk(&k, inner, Some(ix), depth + 1, flavor, out),
            Child::List(k, items) => walk_list(&k, items, ix, depth + 1, flavor, out),
        }
    }
}

/// A child found while reading a node, walked once the node itself is pushed.
enum Child {
    One(String, Map<String, Value>),
    List(String, Vec<Value>),
}

/// An array of objects under `key` is one node with a child per element. An element whose
/// only key holds an object is that key's node; any other element is a numbered node.
fn walk_list(
    key: &str,
    items: Vec<Value>,
    parent: usize,
    depth: usize,
    flavor: JsonFlavor,
    out: &mut Vec<PlanNode>,
) {
    let ix = out.len();
    out.push(PlanNode::bare(Some(parent), depth, humanize(key)));
    for (n, item) in items.into_iter().enumerate() {
        let Value::Object(obj) = item else {
            continue;
        };
        let single = obj.len() == 1 && obj.values().all(Value::is_object);
        if !single {
            let name = format!("{} #{}", humanize(key), n + 1);
            walk_numbered(name, obj, ix, depth + 1, flavor, out);
        } else if let Some((k, Value::Object(inner))) = obj.into_iter().next() {
            walk(&k, inner, Some(ix), depth + 1, flavor, out);
        }
    }
}

/// Walks an array element that is not a single named node, under its numbered name.
fn walk_numbered(
    name: String,
    obj: Map<String, Value>,
    parent: usize,
    depth: usize,
    flavor: JsonFlavor,
    out: &mut Vec<PlanNode>,
) {
    let start = out.len();
    walk("", obj, Some(parent), depth, flavor, out);
    if let Some(node) = out.get_mut(start) {
        node.node_type = name;
    }
}

/// A measured MariaDB node's figures. Its times are totals over `r_loops`, so they are divided
/// per loop; `r_rows` is already per loop. A node that never ran has all zeros.
fn mariadb_actual(obj: &Map<String, Value>) -> Option<Actual> {
    if !TIMED.iter().any(|k| obj.contains_key(*k)) {
        return None;
    }
    let get = |k: &str| obj.get(k).and_then(number);
    let loops = get("r_loops").map_or(1, |l| l.max(0.0) as u64);
    if loops == 0 {
        return Some(Actual {
            total_ms: 0.0,
            rows: 0.0,
            loops: 0,
            self_ms: 0.0,
        });
    }
    let spent = get("r_total_time_ms").unwrap_or_else(|| {
        get("r_table_time_ms").unwrap_or(0.0) + get("r_other_time_ms").unwrap_or(0.0)
    });
    Some(Actual {
        total_ms: spent / loops as f64,
        rows: get("r_rows").unwrap_or(0.0),
        loops,
        self_ms: 0.0,
    })
}

/// Every element is an object, and there is at least one.
fn is_node_list(items: &[Value]) -> bool {
    !items.is_empty() && items.iter().all(Value::is_object)
}

/// Folds `inner` into the node's detail as `prefix.key`, nested objects recursively. A
/// parseable `query_cost` or `prefix_cost` directly under `cost_info` is the total cost.
fn fold(node: &mut PlanNode, prefix: &str, inner: Map<String, Value>) {
    for (k, v) in inner {
        let path = format!("{prefix}.{k}");
        let cost = prefix == "cost_info" && (k == "query_cost" || k == "prefix_cost");
        if cost
            && node.total_cost.is_none()
            && let Some(c) = number(&v)
        {
            node.total_cost = Some(c);
            continue;
        }
        match v {
            Value::Object(nested) => fold(node, &path, nested),
            v @ (Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_)) => push_detail(&mut node.detail, path, v),
        }
    }
}

/// A number, or a string holding one, as MySQL prints its costs.
fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

fn push_detail(detail: &mut Vec<(String, String)>, key: String, v: Value) {
    detail.push((key, detail_text(v)));
}

/// A `table` object is named by its write flag or else its access type; any other key by
/// itself, with spaces for underscores and the first letter upper-cased.
fn node_name(key: &str, obj: &Map<String, Value>) -> String {
    if key == "table" {
        table_name(obj)
    } else {
        humanize(key)
    }
}

fn table_name(obj: &Map<String, Value>) -> String {
    let flagged = |flag: &str| match obj.get(flag) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_u64() == Some(1),
        Some(Value::Null | Value::String(_) | Value::Array(_) | Value::Object(_)) | None => false,
    };
    let writes = [
        ("insert", "Insert"),
        ("update", "Update"),
        ("delete", "Delete"),
        ("replace", "Replace"),
    ];
    if let Some((_, name)) = writes.iter().find(|(flag, _)| flagged(flag)) {
        return (*name).to_owned();
    }
    let Some(access) = obj.get("access_type").and_then(Value::as_str) else {
        return "Table".to_owned();
    };
    let named = match access {
        "ALL" => "Full scan",
        "index" => "Index scan",
        "range" => "Range scan",
        "ref" => "Index lookup",
        "eq_ref" => "Unique lookup",
        "const" => "Constant row",
        "system" => "System row",
        "ref_or_null" => "Index lookup or null",
        "fulltext" => "Fulltext lookup",
        "index_merge" => "Index merge",
        "unique_subquery" => "Unique subquery",
        "index_subquery" => "Index subquery",
        other => return format!("Table {other}"),
    };
    named.to_owned()
}

/// `grouping_operation` as "Grouping operation".
fn humanize(key: &str) -> String {
    let spaced = key.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => spaced,
    }
}
