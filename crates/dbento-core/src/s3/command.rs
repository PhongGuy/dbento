//! The four S3 commands a statement's words can spell.

use super::ObjectKey;

/// One parsed S3 statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum S3Command {
    /// `ls`: the buckets.
    Buckets,
    /// `ls [-r] s3://bucket[/prefix]`: one level, or every key under the prefix.
    List { url: ObjectKey, recursive: bool },
    /// `stat s3://bucket/key`.
    Stat(ObjectKey),
    /// `cat s3://bucket/key`.
    Cat(ObjectKey),
}

const USAGE: &str =
    "usage: ls, ls [-r] s3://bucket[/prefix], stat s3://bucket/key, cat s3://bucket/key";

/// The command `words` spell; any other shape is an error that names every usage.
pub(crate) fn parse(words: &[String]) -> Result<S3Command, String> {
    let bad = |why: String| format!("{why}; {USAGE}");
    let url = |text: &str| {
        ObjectKey::parse(text).ok_or_else(|| bad(format!("`{text}` is not an s3://bucket URL")))
    };
    let object = |text: &str| {
        let key = url(text)?;
        if key.key.is_empty() {
            return Err(bad(format!("`{text}` names a bucket, not an object")));
        }
        Ok(key)
    };
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["ls"] => Ok(S3Command::Buckets),
        ["ls", "-r", target] => Ok(S3Command::List {
            url: url(target)?,
            recursive: true,
        }),
        ["ls", target] if !target.starts_with('-') => Ok(S3Command::List {
            url: url(target)?,
            recursive: false,
        }),
        ["stat", target] => Ok(S3Command::Stat(object(target)?)),
        ["cat", target] => Ok(S3Command::Cat(object(target)?)),
        _ => Err(bad(format!("`{}` is not an S3 command", words.join(" ")))),
    }
}

#[cfg(test)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "a test's catch-all arm only fails the test"
)]
mod tests;
