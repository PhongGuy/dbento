//! `cargo xtask packager-config`: the complete cargo-packager config for one build.
//!
//! cargo-packager's `--config` replaces the `Cargo.toml` section instead of merging it, so this
//! fills what cargo-packager would derive from the workspace and adds signing only from the
//! environment, keeping secrets out of the committed metadata.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, bail};
use serde_json::{Map, Value};

/// The package whose `[package.metadata.packager]` section is packaged.
pub const PACKAGE: &str = "dbento-app";
/// Environment variable holding the macOS code-signing identity.
pub const MACOS_IDENTITY_VAR: &str = "DBENTO_MACOS_SIGNING_IDENTITY";
/// Environment variable holding the Windows sign command.
pub const WINDOWS_SIGN_COMMAND_VAR: &str = "DBENTO_WINDOWS_SIGN_COMMAND";

/// Every spelling cargo-packager accepts for a signing setting, by section.
const SECRET_KEYS: &[(&str, &[&str])] = &[
    (
        "macos",
        &["signingIdentity", "signing-identity", "signing_identity"],
    ),
    (
        "windows",
        &[
            "signCommand",
            "sign-command",
            "sign_command",
            "certificateThumbprint",
            "certificate-thumbprint",
            "certificate_thumbprint",
        ],
    ),
];

/// Signing settings supplied by the environment; `None` means unsigned.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Signing {
    pub macos_identity: Option<String>,
    pub windows_sign_command: Option<String>,
}

impl Signing {
    /// Reads both variables through `get`; an unset or blank value means unsigned, since an
    /// absent CI secret expands to an empty string.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Signing {
        let present = |name: &str| get(name).filter(|value| !value.trim().is_empty());
        Signing {
            macos_identity: present(MACOS_IDENTITY_VAR),
            windows_sign_command: present(WINDOWS_SIGN_COMMAND_VAR),
        }
    }
}

/// Builds the config from `cargo metadata --no-deps --format-version 1` output: the package's
/// packager section plus `name`, `version`, `binariesDir`, `outDir`, absolute icon, desktop
/// template and resource paths, `notices` appended to the resources, and signing from `signing`.
/// Errors when the package or section is missing or the section already holds a signing setting.
pub fn packager_config(
    metadata: &Value,
    target: Option<&str>,
    out_dir: &Path,
    notices: &Path,
    signing: &Signing,
) -> anyhow::Result<Value> {
    let package = metadata["packages"]
        .as_array()
        .and_then(|packages| packages.iter().find(|p| p["name"] == PACKAGE))
        .with_context(|| format!("package {PACKAGE} not found in cargo metadata"))?;
    let Some(Value::Object(section)) = package["metadata"].get("packager") else {
        bail!("{PACKAGE} has no [package.metadata.packager] section");
    };
    let mut config = section.clone();

    for (name, keys) in SECRET_KEYS {
        if let Some(found) = keys
            .iter()
            .find(|key| config.get(*name).and_then(|s| s.get(**key)).is_some())
        {
            bail!(
                "{name}.{found} must not be committed in [package.metadata.packager]; \
                 set it through the environment"
            );
        }
    }

    let field = |value: &Value, name: &str| {
        value[name]
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("cargo metadata lacks {name}"))
    };
    let manifest = field(package, "manifest_path")?;
    let manifest_dir = Path::new(&manifest)
        .parent()
        .context("manifest path has no directory")?;
    let mut binaries_dir = Path::new(&field(metadata, "target_directory")?).to_path_buf();
    if let Some(target) = target {
        binaries_dir.push(target);
    }
    binaries_dir.push("release");

    config.insert("name".into(), PACKAGE.into());
    config.insert("version".into(), field(package, "version")?.into());
    config.insert("binariesDir".into(), path_value(&binaries_dir));
    config.insert("outDir".into(), path_value(out_dir));

    // A raw --config file does not move cargo-packager into the crate, so relative icon
    // globs and the desktop template are anchored to the manifest directory here.
    if let Some(Value::Array(icons)) = config.get_mut("icons") {
        for icon in icons {
            if let Some(path) = icon.as_str() {
                *icon = path_value(&manifest_dir.join(path));
            }
        }
    }
    if let Some(template) = config
        .get_mut("deb")
        .and_then(|deb| deb.get_mut("desktopTemplate"))
        && let Some(path) = template.as_str()
    {
        *template = path_value(&manifest_dir.join(path));
    }

    // Resources are anchored the same way; an object entry keeps its target and moves only
    // its source.
    if let Some(Value::Array(resources)) = config.get_mut("resources") {
        for resource in resources {
            let src = match resource {
                Value::Object(entry) => entry.get_mut("src"),
                other @ (Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::String(_)
                | Value::Array(_)) => Some(other),
            };
            if let Some(src) = src
                && let Some(path) = src.as_str()
            {
                *src = path_value(&manifest_dir.join(path));
            }
        }
    }

    // The notices are generated at package time, so they join the resources only here, after
    // anchoring, as the absolute path the caller already holds.
    let resources = config
        .entry("resources")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Value::Array(resources) = resources else {
        bail!("[package.metadata.packager] resources is not an array");
    };
    resources.push(path_value(notices));

    add_setting(
        &mut config,
        "macos",
        "signingIdentity",
        signing.macos_identity.as_deref(),
    )?;
    add_setting(
        &mut config,
        "windows",
        "signCommand",
        signing.windows_sign_command.as_deref(),
    )?;
    Ok(Value::Object(config))
}

/// Merges `key` into the `section` object, creating the object only when there is a value.
/// Errors when the committed section is not a table, so a signing value is never dropped.
fn add_setting(
    config: &mut Map<String, Value>,
    section: &str,
    key: &str,
    value: Option<&str>,
) -> anyhow::Result<()> {
    let Some(value) = value else { return Ok(()) };
    let entry = config
        .entry(section)
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(object) = entry else {
        bail!(
            "[package.metadata.packager] {section} is not a table, so {section}.{key} cannot be set"
        );
    };
    object.insert(key.into(), value.into());
    Ok(())
}

fn path_value(path: &Path) -> Value {
    path.to_string_lossy().into_owned().into()
}

/// Replaces `path` with `contents`, readable only by the owner on unix regardless of any
/// earlier file's mode or the umask.
pub fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(contents)
}

/// Runs `cargo metadata`, reads the signing variables and writes the config to `output`.
/// Errors unless `notices` is a file, so a package never ships without its notices.
pub fn run(
    out_dir: &Path,
    output: &Path,
    target: Option<&str>,
    notices: &Path,
) -> anyhow::Result<()> {
    if !notices.is_file() {
        bail!(
            "{} is not a file; run cargo xtask notices --out <file> first",
            notices.display()
        );
    }
    let out_dir = std::path::absolute(out_dir)?;
    let notices = std::path::absolute(notices)?;
    let metadata = crate::cargo_metadata()?;
    let signing = Signing::from_vars(|name| std::env::var(name).ok());
    let config = packager_config(&metadata, target, &out_dir, &notices, &signing)?;
    let mut json = serde_json::to_vec_pretty(&config)?;
    json.push(b'\n');
    write_private(output, &json).with_context(|| format!("writing {}", output.display()))
}
