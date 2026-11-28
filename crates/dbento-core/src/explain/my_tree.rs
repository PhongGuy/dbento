//! MySQL's `EXPLAIN ANALYZE` tree text read into a pre-order node list, one node per line.
//! Each line is an indented `-> label`, optionally followed by an estimate and an actual.

use super::plan::{Actual, Plan, PlanNode, PlanParseError, settle_self_time};

/// Parses the tree text. Fails on an empty text, a line without `-> ` after its indent, an
/// indent that is not a multiple of 4, or an indent more than one level below the line before.
pub(super) fn parse(raw: &str) -> Result<Plan, PlanParseError> {
    let fail = |message: String| Err(PlanParseError { message });
    let text = raw.trim_end();
    if text.is_empty() {
        return fail("the plan output is empty".to_owned());
    }
    let mut nodes: Vec<PlanNode> = Vec::new();
    // The index of the latest node at each depth; a node's parent is the entry one level up.
    let mut open: Vec<usize> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let body = line.trim_start_matches(' ');
        let indent = line.len() - body.len();
        let Some(rest) = body.strip_prefix("-> ") else {
            return fail(format!("line {} does not start with \"-> \"", n + 1));
        };
        if indent % 4 != 0 || indent / 4 > open.len() {
            return fail(format!("line {} is indented out of the tree", n + 1));
        }
        let depth = indent / 4;
        let (label, cost, actual) = split_line(rest);
        if label.is_empty() {
            return fail(format!("line {} has no label", n + 1));
        }
        let parent = depth.checked_sub(1).map(|up| open[up]);
        let mut node = PlanNode::bare(parent, depth, label.to_owned());
        if let Some(cost) = cost {
            node.startup_cost = cost.startup;
            node.total_cost = Some(cost.total);
            node.rows = Some(cost.rows);
        }
        node.actual = actual;
        open.truncate(depth);
        open.push(nodes.len());
        nodes.push(node);
    }
    settle_self_time(&mut nodes);
    let execution_ms = execution_ms(&nodes);
    Ok(Plan {
        analyzed: nodes.iter().any(|n| n.actual.is_some()),
        nodes,
        planning_ms: None,
        execution_ms,
    })
}

/// A lower bound on the run: root 0 plus each `Select #N` root that no node outside its own
/// subtree names as `(select #N)`, since a referenced one already runs inside that node.
/// `None` when no counted node has an actual.
fn execution_ms(nodes: &[PlanNode]) -> Option<f64> {
    let labels: Vec<String> = nodes
        .iter()
        .map(|n| n.node_type.to_ascii_lowercase())
        .collect();
    let starts: Vec<usize> = (0..nodes.len()).filter(|&i| nodes[i].depth == 0).collect();
    let mut total = None;
    for (k, &start) in starts.iter().enumerate() {
        let tree = start..starts.get(k + 1).copied().unwrap_or(nodes.len());
        let counted = start == 0
            || select_number(&labels[start]).is_some_and(|n| {
                let mention = format!("(select #{n})");
                !labels
                    .iter()
                    .enumerate()
                    .any(|(i, label)| !tree.contains(&i) && label.contains(mention.as_str()))
            });
        if let Some(ms) = counted.then(|| root_ms(&nodes[tree])).flatten() {
            *total.get_or_insert(0.0) += ms;
        }
    }
    total
}

/// The `N` of a label starting `select #N`, all its digits.
fn select_number(label: &str) -> Option<&str> {
    let rest = label.strip_prefix("select #")?;
    let digits = &rest[..rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len())];
    (!digits.is_empty()).then_some(digits)
}

/// A root's time: its own spent (a header's is its children's sum), raised to the largest
/// spent in its subtree, since a parent can print less than one child ran.
fn root_ms(tree: &[PlanNode]) -> Option<f64> {
    let spent = |n: &PlanNode| n.actual.map(|a| a.total_ms * a.loops as f64);
    let own = tree.first().and_then(spent).or_else(|| {
        tree.iter()
            .filter(|n| n.depth == 1)
            .filter_map(spent)
            .reduce(|a, b| a + b)
    });
    let largest = tree.iter().filter_map(spent).reduce(f64::max);
    own.into_iter().chain(largest).reduce(f64::max)
}

/// The optimizer's estimate from `(cost=a..b rows=n)` or `(cost=b rows=n)`.
struct Cost {
    startup: Option<f64>,
    total: f64,
    rows: f64,
}

/// Peels the actual and then the cost off the end of a line, right to left. A tail that
/// does not parse, such as `(cost=` inside a string literal, stays in the label.
fn split_line(rest: &str) -> (&str, Option<Cost>, Option<Actual>) {
    let mut label = rest.trim_end();
    let mut actual = None;
    if let Some(head) = label.strip_suffix("(never executed)") {
        if let Some(head) = after_gap(head) {
            label = head;
            actual = Some(never());
        }
    } else if let Some((head, a)) = tail(label, "(actual time=", parse_actual) {
        label = head;
        actual = Some(a);
    }
    let mut cost = None;
    if let Some((head, c)) = tail(label, "(cost=", parse_cost) {
        label = head;
        cost = Some(c);
    }
    (label, cost, actual)
}

/// Splits `text` at its last `open` when the tail from there parses and a space separates
/// it from the head; returns the head without those spaces.
fn tail<'a, T>(text: &'a str, open: &str, read: fn(&str) -> Option<T>) -> Option<(&'a str, T)> {
    let at = text.rfind(open)?;
    let value = read(text[at + open.len()..].strip_suffix(')')?)?;
    Some((after_gap(&text[..at])?, value))
}

/// The head before a run of spaces that must end it, or `None` when there is no gap.
fn after_gap(head: &str) -> Option<&str> {
    let trimmed = head.trim_end_matches(' ');
    (trimmed.len() < head.len()).then_some(trimmed)
}

/// Reads `a..b rows=n` or `b rows=n`, the inside of `(cost=...)`.
fn parse_cost(inner: &str) -> Option<Cost> {
    let (costs, rows) = inner.split_once(" rows=")?;
    let rows = rows.parse().ok()?;
    match costs.split_once("..") {
        Some((a, b)) => Some(Cost {
            startup: Some(a.parse().ok()?),
            total: b.parse().ok()?,
            rows,
        }),
        None => Some(Cost {
            startup: None,
            total: costs.parse().ok()?,
            rows,
        }),
    }
}

/// Reads `a..b rows=n loops=k`, the inside of `(actual time=...)`; MySQL averages the
/// time and rows per loop, as `Actual` keeps them.
fn parse_actual(inner: &str) -> Option<Actual> {
    let (time, rest) = inner.split_once(" rows=")?;
    let (rows, loops) = rest.split_once(" loops=")?;
    let (first, last) = time.split_once("..")?;
    first.parse::<f64>().ok()?;
    Some(Actual {
        total_ms: last.parse().ok()?,
        rows: rows.parse().ok()?,
        loops: loops.parse().ok()?,
        self_ms: 0.0,
    })
}

/// A node MySQL never ran.
fn never() -> Actual {
    Actual {
        total_ms: 0.0,
        rows: 0.0,
        loops: 0,
        self_ms: 0.0,
    }
}
