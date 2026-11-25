//! Release notes: the `notes.json` document published with a release, parsed into sanitised and
//! capped entries, and the part of it an install has not seen yet.

use std::cmp::Ordering;

use semver::Version;
use serde::Deserialize;

use super::NotesError;

// The contract caps every text so a hostile or broken document cannot flood a dialog.
const TEXT_CAP: usize = 500;

#[derive(Deserialize)]
struct RawDoc {
    releases: Vec<RawEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    version: String,
    #[serde(default)]
    previous: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    intro: Option<String>,
    #[serde(default)]
    groups: Vec<RawGroup>,
}

#[derive(Deserialize)]
struct RawGroup {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    items: Vec<RawItem>,
}

#[derive(Deserialize)]
struct RawItem {
    #[serde(default)]
    text: String,
    #[serde(default)]
    pr: Option<u64>,
}

/// The release notes document: every release it lists, sanitised.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseNotes {
    releases: Vec<ReleaseEntry>,
}

/// One release in the notes.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseEntry {
    version: String,
    parsed: Version,
    previous: Option<String>,
    parsed_previous: Option<Version>,
    date: Option<String>,
    intro: Option<String>,
    groups: Vec<Group>,
}

/// A titled list of changes of one kind.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    kind: NotesKind,
    title: String,
    items: Vec<Item>,
}

/// One change.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    text: String,
    pr: Option<u64>,
}

/// What kind of change a group lists.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotesKind {
    /// New features.
    Features,
    /// Bug fixes.
    Fixes,
    /// Usability and accessibility.
    Usability,
    /// Performance.
    Performance,
    /// Anything else, including a kind this version does not know.
    Other,
}

/// The releases newer than an install, and whether the list reaches back to it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Between {
    releases: Vec<ReleaseEntry>,
    older_on_release_page: bool,
}

impl ReleaseNotes {
    /// Parses notes.json bytes: schema 1 only, every text sanitised and capped, empty items and
    /// groups dropped. A schema other than 1 is [`NotesError::UnsupportedSchema`]; bad JSON, a
    /// wrong shape or a version that is not semver is [`NotesError::Invalid`].
    pub fn parse(bytes: &[u8]) -> Result<ReleaseNotes, NotesError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| NotesError::Invalid(e.to_string()))?;
        let schema = value
            .get("schema")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| NotesError::Invalid("no integer schema".to_owned()))?;
        if schema != 1 {
            return Err(NotesError::UnsupportedSchema(schema));
        }
        let raw: RawDoc =
            serde_json::from_value(value).map_err(|e| NotesError::Invalid(e.to_string()))?;
        let releases = raw
            .releases
            .into_iter()
            .map(ReleaseEntry::from_raw)
            .collect::<Result<_, _>>()?;
        Ok(ReleaseNotes { releases })
    }

    /// Every release, in the order the file lists them.
    pub fn releases(&self) -> &[ReleaseEntry] {
        &self.releases
    }

    /// The releases strictly newer than `installed`, newest first. An `installed` that is not
    /// semver shows every release and points at the release page for anything older.
    pub fn between(&self, installed: &str) -> Between {
        let installed = Version::parse(installed).ok();
        let newer = |v: &Version| {
            installed
                .as_ref()
                .is_none_or(|i| v.cmp_precedence(i) == Ordering::Greater)
        };
        let mut releases: Vec<ReleaseEntry> = self
            .releases
            .iter()
            .filter(|e| newer(&e.parsed))
            .cloned()
            .collect();
        releases.sort_by(|a, b| b.parsed.cmp_precedence(&a.parsed));
        // The list reaches the install only when the oldest shown release names a predecessor
        // that is not newer than it; no predecessor means that release was the first.
        let older_on_release_page = installed.is_none()
            || releases
                .last()
                .is_none_or(|oldest| oldest.parsed_previous.as_ref().is_some_and(newer));
        Between {
            releases,
            older_on_release_page,
        }
    }
}

impl ReleaseEntry {
    fn from_raw(raw: RawEntry) -> Result<ReleaseEntry, NotesError> {
        let version = clean(&raw.version);
        let parsed = semver(&version)?;
        let previous = raw.previous.as_deref().map(clean);
        let parsed_previous = previous.as_deref().map(semver).transpose()?;
        let groups = raw.groups.into_iter().filter_map(Group::from_raw).collect();
        Ok(ReleaseEntry {
            version,
            parsed,
            previous,
            parsed_previous,
            date: non_empty(raw.date.as_deref()),
            intro: non_empty(raw.intro.as_deref()),
            groups,
        })
    }

    /// The release version.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The release before this one, or `None` for the first release.
    pub fn previous(&self) -> Option<&str> {
        self.previous.as_deref()
    }

    /// The release date, as the file states it.
    pub fn date(&self) -> Option<&str> {
        self.date.as_deref()
    }

    /// A sentence introducing the release.
    pub fn intro(&self) -> Option<&str> {
        self.intro.as_deref()
    }

    /// The groups of changes, none of them empty.
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }
}

impl Group {
    // A group whose items all sanitise to nothing is dropped, as is an empty item.
    fn from_raw(raw: RawGroup) -> Option<Group> {
        let items: Vec<Item> = raw
            .items
            .into_iter()
            .filter_map(|i| {
                let text = clean(&i.text);
                (!text.is_empty()).then_some(Item { text, pr: i.pr })
            })
            .collect();
        if items.is_empty() {
            return None;
        }
        let kind = match raw.kind.as_deref() {
            Some("features") => NotesKind::Features,
            Some("fixes") => NotesKind::Fixes,
            Some("usability") => NotesKind::Usability,
            Some("performance") => NotesKind::Performance,
            _ => NotesKind::Other,
        };
        let title = non_empty(raw.title.as_deref()).unwrap_or_else(|| kind.title().to_owned());
        Some(Group { kind, title, items })
    }

    /// What kind of change the group lists.
    pub fn kind(&self) -> NotesKind {
        self.kind
    }

    /// The group's English title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The changes, none of them empty.
    pub fn items(&self) -> &[Item] {
        &self.items
    }
}

impl Item {
    /// The change, as plain text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The pull request number, never meant to be rendered as a link.
    pub fn pr(&self) -> Option<u64> {
        self.pr
    }
}

impl NotesKind {
    // The English title a group of this kind falls back to.
    fn title(self) -> &'static str {
        match self {
            NotesKind::Features => "New features",
            NotesKind::Fixes => "Fixes",
            NotesKind::Usability => "Usability and accessibility",
            NotesKind::Performance => "Performance",
            NotesKind::Other => "Other changes",
        }
    }
}

impl Between {
    /// The releases newer than the install, newest first.
    pub fn releases(&self) -> &[ReleaseEntry] {
        &self.releases
    }

    /// True when releases between the install and the oldest one shown are missing from the
    /// notes, so the user should look at the release page for them.
    pub fn older_on_release_page(&self) -> bool {
        self.older_on_release_page
    }
}

/// Removes C0 and C1 controls, DEL and the bidi controls U+202A-U+202E and U+2066-U+2069; tab,
/// CR and newline become a space; trims.
pub fn plain_text(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\t' | '\r' | '\n' => Some(' '),
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => None,
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn clean(s: &str) -> String {
    plain_text(s).chars().take(TEXT_CAP).collect()
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(clean).filter(|s| !s.is_empty())
}

fn semver(s: &str) -> Result<Version, NotesError> {
    Version::parse(s).map_err(|e| NotesError::Invalid(format!("version {s:?}: {e}")))
}
