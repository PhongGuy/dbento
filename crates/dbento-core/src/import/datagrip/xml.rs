//! Reads the XML metadata files of a DataGrip project into raw, unmapped records.

use super::DataGripError;
use super::forest::{self, ForestForm};
use roxmltree::{Document, Node};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Everything the metadata files say, before any mapping to Dbento connections.
#[derive(Debug)]
pub(crate) struct Project {
    pub(crate) root: PathBuf,
    pub(crate) idea: PathBuf,
    pub(crate) sources: Vec<RawSource>,
    pub(crate) local: HashMap<String, RawLocal>,
    pub(crate) mappings: Vec<RawMapping>,
    /// Folder segments per data-source uuid from `db-forest-config.xml`; empty when unusable.
    pub(crate) forest: HashMap<String, Arc<[String]>>,
    /// Raw colour value per data-source uuid from `databaseColors.xml`.
    pub(crate) colors: HashMap<String, String>,
    /// The colours file holds colours Dbento cannot carry (object colours), or is unreadable.
    pub(crate) has_object_colors: bool,
}

#[derive(Default)]
pub(crate) struct RawSource {
    pub(crate) uuid: String,
    pub(crate) name: String,
    pub(crate) driver_ref: Option<String>,
    pub(crate) jdbc_driver: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) working_dir: Option<String>,
    pub(crate) read_only: bool,
    pub(crate) group: Option<String>,
    pub(crate) properties: Vec<(String, String)>,
}

// The URL and property values can carry a password, so Debug never prints them.
impl fmt::Debug for RawSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.properties.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("RawSource")
            .field("uuid", &self.uuid)
            .field("name", &self.name)
            .field("driver_ref", &self.driver_ref)
            .field("jdbc_driver", &self.jdbc_driver)
            .field("url", &self.url.as_ref().map(|_| "<redacted>"))
            .field("working_dir", &self.working_dir)
            .field("read_only", &self.read_only)
            .field("group", &self.group)
            .field("properties", &names)
            .field("property_values", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Default)]
pub(crate) struct RawLocal {
    pub(crate) user_name: Option<String>,
    pub(crate) auth_provider: Option<String>,
    pub(crate) ssh_enabled: bool,
    pub(crate) ssl: Option<RawSsl>,
}

#[derive(Debug, Default)]
pub(crate) struct RawSsl {
    pub(crate) enabled: Option<bool>,
    pub(crate) mode: Option<String>,
    pub(crate) ca_cert: Option<String>,
    pub(crate) client_cert: Option<String>,
    pub(crate) client_key: Option<String>,
}

#[derive(Debug)]
pub(crate) struct RawMapping {
    pub(crate) url: String,
    pub(crate) uuid: String,
}

/// Reads a DataGrip project from its folder or from its `.idea` folder.
pub(crate) fn read_project(folder: &Path) -> Result<Project, DataGripError> {
    let (root, idea) = locate(folder)?;
    let mut project = Project {
        root,
        idea,
        sources: Vec::new(),
        local: HashMap::new(),
        mappings: Vec::new(),
        forest: HashMap::new(),
        colors: HashMap::new(),
        has_object_colors: false,
    };
    // Components are found by name in whichever file holds them, so shared-only and
    // multi-file layouts read the same way.
    for (file, required) in [
        ("dataSources.xml", true),
        ("dataSources.local.xml", false),
        ("data_source_mapping.xml", false),
    ] {
        let path = project.idea.join(file);
        let Some(text) = read_text(&path, required)? else {
            continue;
        };
        let doc = Document::parse(&text).map_err(|e| DataGripError::Xml {
            path: path.clone(),
            line: e.pos().row,
            // roxmltree quotes document text after the first quote; keep only the cause.
            message: match e.to_string().split('\'').next().unwrap_or_default().trim() {
                "" => "malformed XML".to_owned(),
                cause => cause.to_owned(),
            },
        })?;
        collect(&doc, &mut project);
    }
    let uuids: HashSet<&str> = project.sources.iter().map(|s| s.uuid.as_str()).collect();
    project.forest =
        read_forest(&project.idea.join("db-forest-config.xml"), &uuids).unwrap_or_default();
    let colors_path = project.idea.join("databaseColors.xml");
    match read_colors(&colors_path, &uuids) {
        Some((colors, has_object_colors)) => {
            project.colors = colors;
            project.has_object_colors = has_object_colors;
        }
        None => project.has_object_colors = colors_path.exists(),
    }
    Ok(project)
}

/// The data-source colours keyed by uuid, and whether any entry is keyed by something else.
/// `None` when the file is absent or unusable: colours are a convenience, like the forest.
fn read_colors(path: &Path, uuids: &HashSet<&str>) -> Option<(HashMap<String, String>, bool)> {
    let text = read_text(path, false).ok()??;
    let doc = Document::parse(&text).ok()?;
    let (mut colors, mut others) = (HashMap::new(), false);
    let settings = doc.descendants().filter(|n| {
        n.has_tag_name("component") && n.attribute("name") == Some("DatabaseColorSettings")
    });
    for entry in settings
        .flat_map(|c| children(c, "colors"))
        .flat_map(|c| children(c, "entry"))
    {
        let key = attr(entry, "key");
        if uuids.contains(key.as_str()) {
            colors.insert(key, attr(entry, "value"));
        } else {
            others = true;
        }
    }
    Some((colors, others))
}

/// The folder forest, or `None` when the file is absent, unreadable, not UTF-8 or not XML:
/// folders are a convenience, so a bad forest file never fails the import.
fn read_forest(path: &Path, uuids: &HashSet<&str>) -> Option<HashMap<String, Arc<[String]>>> {
    let keep = |uuid: &str| uuids.contains(uuid);
    let text = read_text(path, false).ok()??;
    let doc = Document::parse(&text).ok()?;
    let components = doc.descendants().filter(|n| n.has_tag_name("component"));
    for component in components {
        match component.attribute("name") {
            Some("db-forest-configuration") => {
                let data = child(component, "data")?.text()?;
                return Some(forest::parse_forest(data, ForestForm::V2, keep));
            }
            Some("db-tree-configuration") => {
                let data = children(component, "option")
                    .find(|o| o.attribute("name") == Some("data"))?
                    .attribute("value")?;
                return Some(forest::parse_forest(data, ForestForm::Legacy, keep));
            }
            _ => {}
        }
    }
    None
}

/// Finds the project root and `.idea` folder, or says the folder is not a project.
fn locate(folder: &Path) -> Result<(PathBuf, PathBuf), DataGripError> {
    let idea = folder.join(".idea");
    if idea.join("dataSources.xml").exists() {
        return Ok((folder.to_path_buf(), idea));
    }
    if folder.join("dataSources.xml").exists() {
        let is_idea = folder.file_name().is_some_and(|n| n == ".idea");
        let root = match folder.parent() {
            Some(parent) if is_idea => parent,
            _ => folder,
        };
        return Ok((root.to_path_buf(), folder.to_path_buf()));
    }
    Err(DataGripError::NotAProject {
        folder: folder.to_path_buf(),
    })
}

/// Metadata files beyond this size are refused; real ones are a few kilobytes.
pub(super) const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;

/// Reads one file as UTF-8; an optional file that is absent yields `None`.
/// Only a regular file within `MAX_METADATA_BYTES` is read, so a symlink to a
/// device or FIFO, or a huge file, is refused before any byte is loaded.
fn read_text(path: &Path, required: bool) -> Result<Option<String>, DataGripError> {
    let io = |message: String| DataGripError::Io {
        path: path.to_path_buf(),
        message,
    };
    let file = match open_regular(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::Other => return Err(io(e.to_string())),
        Err(e) => return Err(io(e.kind().to_string())),
    };
    // The size can change after the check, so the read itself is bounded too.
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io(e.kind().to_string()))?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(io("file too large".to_owned()));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| DataGripError::NotUtf8 {
            path: path.to_path_buf(),
        })
}

// Opens `path` and checks the opened handle, so nothing can be swapped in after the check.
// O_NONBLOCK keeps a FIFO from blocking the open before its type is seen.
fn open_regular(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    if meta.len() > MAX_METADATA_BYTES {
        return Err(std::io::Error::other("file too large"));
    }
    Ok(file)
}

/// Copies the records of every known component in one parsed document into the project.
fn collect(doc: &Document<'_>, project: &mut Project) {
    for component in doc.descendants().filter(|n| n.has_tag_name("component")) {
        match component.attribute("name") {
            Some("DataSourceManagerImpl") => {
                for ds in children(component, "data-source") {
                    project.sources.push(raw_source(ds));
                }
            }
            Some("dataSourceStorageLocal") => {
                for ds in children(component, "data-source") {
                    let uuid = attr(ds, "uuid");
                    project.local.insert(uuid, raw_local(ds));
                }
            }
            Some("DataSourcePerFileMappings") => {
                for file in children(component, "file") {
                    project.mappings.push(RawMapping {
                        url: attr(file, "url"),
                        uuid: attr(file, "value"),
                    });
                }
            }
            _ => {}
        }
    }
}

fn raw_source(ds: Node<'_, '_>) -> RawSource {
    let properties = child(ds, "driver-properties")
        .map(|props| {
            children(props, "property")
                .map(|p| (attr(p, "name"), attr(p, "value")))
                .collect()
        })
        .unwrap_or_default();
    RawSource {
        uuid: attr(ds, "uuid"),
        name: attr(ds, "name"),
        driver_ref: text(ds, "driver-ref"),
        jdbc_driver: text(ds, "jdbc-driver"),
        url: text(ds, "jdbc-url"),
        working_dir: text(ds, "working-dir"),
        read_only: ds.attribute("read-only") == Some("true"),
        group: ds.attribute("group").map(str::to_owned),
        properties,
    }
}

fn raw_local(ds: Node<'_, '_>) -> RawLocal {
    let ssh_enabled = child(ds, "ssh-properties")
        .and_then(|ssh| text(ssh, "enabled"))
        .is_some_and(|v| v == "true");
    let ssl = child(ds, "ssl-config").map(|ssl| RawSsl {
        enabled: text(ssl, "enabled").map(|v| v == "true"),
        mode: text(ssl, "mode"),
        ca_cert: text(ssl, "ca-cert"),
        client_cert: text(ssl, "client-cert"),
        client_key: text(ssl, "client-key"),
    });
    RawLocal {
        user_name: text(ds, "user-name"),
        auth_provider: text(ds, "auth-provider"),
        ssh_enabled,
        ssl,
    }
}

fn children<'a, 'i: 'a>(
    node: Node<'a, 'i>,
    tag: &'static str,
) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children().filter(move |n| n.has_tag_name(tag))
}

fn child<'a, 'i>(node: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|n| n.has_tag_name(tag))
}

/// Trimmed text of a named child; absent or blank gives `None`.
fn text(node: Node<'_, '_>, tag: &str) -> Option<String> {
    let t = child(node, tag)?.text()?.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

fn attr(node: Node<'_, '_>, name: &str) -> String {
    node.attribute(name).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests;
