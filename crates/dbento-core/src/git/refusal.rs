//! Reading a refused push's stderr as the Potluck hook's findings.

/// What a Potluck server's hook said when it refused a push.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Refusal {
    /// The findings the hook listed, in the order it sent them.
    pub findings: Vec<Finding>,
    /// Findings beyond the listed ones.
    pub more: u32,
    /// Every other `potluck-` line, verbatim: server errors and lines this client cannot read.
    pub other: Vec<String>,
}

impl Refusal {
    /// Whether the server failed rather than refused: no finding was listed or counted.
    pub fn is_server_error(&self) -> bool {
        self.findings.is_empty() && self.more == 0
    }

    /// One line of text per finding, then `and N more` when more were counted, then each
    /// `other` line as received.
    pub fn rows(&self) -> Vec<String> {
        let findings = self.findings.iter().map(|f| {
            let place = if f.path == "/" { "no file" } else { &f.path };
            let mut row = format!("{place} on {}: {}", f.git_ref, rule_text(&f.rule));
            if let Some(detail) = &f.detail {
                row.push_str(&format!(" ({detail})"));
            }
            row
        });
        let more = (self.more > 0).then(|| format!("and {} more", self.more));
        findings
            .chain(more)
            .chain(self.other.iter().cloned())
            .collect()
    }
}

/// One thing the server's hook refused, as its `potluck-reject` line names it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Finding {
    /// The kind of refusal, such as `secret_value` or `format_unsupported`.
    pub code: String,
    /// The ref the commit was pushed to.
    pub git_ref: String,
    /// The file holding it, relative to the repository; `/` when it is no one file.
    pub path: String,
    /// The rule that matched.
    pub rule: String,
    /// What the server added about it: names and key paths, never values.
    pub detail: Option<String>,
}

impl Finding {
    /// A finding of kind `code` in `path` on `git_ref`, matched by `rule`, without detail.
    pub fn new(
        code: impl Into<String>,
        git_ref: impl Into<String>,
        path: impl Into<String>,
        rule: impl Into<String>,
    ) -> Finding {
        Finding {
            code: code.into(),
            git_ref: git_ref.into(),
            path: path.into(),
            rule: rule.into(),
            detail: None,
        }
    }

    /// This finding with `detail` attached.
    pub fn with_detail(self, detail: impl Into<String>) -> Finding {
        Finding {
            detail: Some(detail.into()),
            ..self
        }
    }
}

/// The refusal in a push's `stderr`, or `None` when no `remote: potluck-*` line is there.
///
/// Git relays the hook's lines prefixed and space-padded, so each is trimmed on both sides. A
/// `potluck-reject <code> <ref> <path> <rule>` line with percent-encoded ref and path is a
/// finding; a `potluck-reject-detail` line right after one attaches to it; `potluck-reject-more
/// <n>` counts the unlisted ones. Every other `potluck-` line, malformed ones included, lands in
/// `other` verbatim so nothing the server said is lost.
pub fn parse_refusal(stderr: &str) -> Option<Refusal> {
    let mut refusal = Refusal {
        findings: Vec::new(),
        more: 0,
        other: Vec::new(),
    };
    let mut seen = false;
    // Set only on the line right after a kept finding, so a detail never reaches an older one.
    let mut open = false;
    for line in stderr.lines() {
        let Some(line) = line.trim().strip_prefix("remote: ") else {
            continue;
        };
        let line = line.trim();
        if !line.starts_with("potluck-") {
            continue;
        }
        seen = true;
        let after_finding = std::mem::take(&mut open);
        let kept = if let Some(text) = line.strip_prefix("potluck-reject-detail ") {
            attach_detail(&mut refusal.findings, after_finding, text.trim())
        } else if let Some(count) = line.strip_prefix("potluck-reject-more ") {
            count.trim().parse().map(|n| refusal.more = n).is_ok()
        } else if line.starts_with("potluck-reject ") {
            let found = reject_line(line);
            open = found.is_some();
            found.map(|f| refusal.findings.push(f)).is_some()
        } else {
            false
        };
        if !kept {
            refusal.other.push(line.to_owned());
        }
    }
    seen.then_some(refusal)
}

/// Attaches a decoded `text` to the last finding when it came on the line just before;
/// false when there is none or the text does not decode.
fn attach_detail(findings: &mut [Finding], after_finding: bool, text: &str) -> bool {
    match (after_finding, findings.last_mut(), percent_decode(text)) {
        (true, Some(last), Some(detail)) => {
            last.detail = Some(detail);
            true
        }
        _ => false,
    }
}

/// The finding in a `potluck-reject` line: exactly five space-separated fields, with ref and
/// path percent-decoded; `None` for any other shape or a malformed escape.
fn reject_line(line: &str) -> Option<Finding> {
    let parts: Vec<&str> = line.split(' ').collect();
    let [_, code, git_ref, path, rule] = parts.as_slice() else {
        return None;
    };
    Some(Finding::new(
        *code,
        percent_decode(git_ref)?,
        percent_decode(path)?,
        *rule,
    ))
}

/// `text` with each `%XX` (either hex case) turned into its byte, read as UTF-8 with invalid
/// bytes shown as U+FFFD. A `%` not followed by two hex digits yields `None`.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        if byte == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            // from_str_radix accepts a leading `+`, which is not a hex digit.
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(byte);
            i += 1;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// The plain-language text for a Potluck rule name, or the name itself when it is unknown.
pub fn rule_text(rule: &str) -> &str {
    match rule {
        "password" => "a password",
        "secret" => "a secret value",
        "parse" => "not a valid workspace file",
        "invalid" => "an invalid connection",
        "invalid_name" => "an invalid name",
        "invalid_folder" => "an invalid folder",
        "unsupported_version" => "an unsupported workspace version",
        "database_file" => "a database file",
        "key_file" => "a key or certificate file",
        "too_large" => "a file over the size limit",
        "relax_read_only" => "relaxes read-only on a connection",
        "relax_color" => "relaxes a connection's color tag",
        "format_unsupported" => "a workspace format this server does not accept",
        "default_branch_missing" => "the workspace's default branch is missing, so push it first",
        "branch_delete" => "deletes the default branch",
        "force_push" => "rewrites the default branch's history",
        "ref_namespace" => "a ref outside branches and tags",
        other => other,
    }
}

const TOKEN_PREFIX: &str = "plk_";
const REDACTED: &str = "plk_[redacted]";

/// `text` with every `plk_` that one or more base64url characters follow replaced by
/// `plk_[redacted]`, wherever it sits in a word.
pub fn redact_token(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(TOKEN_PREFIX) {
        let after = &rest[at + TOKEN_PREFIX.len()..];
        let len = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(after.len());
        out.push_str(&rest[..at]);
        out.push_str(if len == 0 { TOKEN_PREFIX } else { REDACTED });
        rest = &after[len..];
    }
    out.push_str(rest);
    out
}
