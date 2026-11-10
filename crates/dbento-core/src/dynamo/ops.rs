//! The closed set of DynamoDB API calls the driver may send, and which a read-only session allows.

use super::statement::Kind;

/// Every API call the driver makes; a statement carries its kind for the read-only check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DynamoOp {
    ListTables,
    DescribeTable,
    Scan,
    ExecuteStatement(Kind),
}

impl DynamoOp {
    /// The AWS action name, as an IAM policy spells it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            DynamoOp::ListTables => "ListTables",
            DynamoOp::DescribeTable => "DescribeTable",
            DynamoOp::Scan => "Scan",
            DynamoOp::ExecuteStatement(_) => "ExecuteStatement",
        }
    }
}

/// A writable session may send anything; a read-only one only the catalog calls, scans and selects.
pub(crate) fn allowed(op: DynamoOp, read_only: bool) -> bool {
    !read_only
        || match op {
            DynamoOp::ListTables | DynamoOp::DescribeTable | DynamoOp::Scan => true,
            DynamoOp::ExecuteStatement(kind) => kind == Kind::Select,
        }
}

#[cfg(test)]
mod tests;
