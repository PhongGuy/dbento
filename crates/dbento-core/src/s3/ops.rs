//! The closed set of S3 API calls the driver may send.

/// Every S3 API call the driver may send; all four only read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum S3Op {
    ListBuckets,
    ListObjectsV2,
    HeadObject,
    GetObject,
}

impl S3Op {
    /// The API name: "ListBuckets", "ListObjectsV2", "HeadObject", "GetObject".
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::ListBuckets => "ListBuckets",
            Self::ListObjectsV2 => "ListObjectsV2",
            Self::HeadObject => "HeadObject",
            Self::GetObject => "GetObject",
        }
    }

    /// The IAM action a policy grants for it; HeadObject is covered by s3:GetObject.
    pub(crate) fn iam_action(self) -> &'static str {
        match self {
            Self::ListBuckets => "s3:ListAllMyBuckets",
            Self::ListObjectsV2 => "s3:ListBucket",
            Self::HeadObject | Self::GetObject => "s3:GetObject",
        }
    }
}

#[cfg(test)]
mod tests;
