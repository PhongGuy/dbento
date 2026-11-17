//! The self-updater: the public release feed, a build's update configuration and the URL
//! rules every update request follows. Compiled only with the `update` feature.

mod error;

use std::time::Duration;

use url::{Host, Url};

pub use error::CheckError;

/// The signed release manifest; its signature is at the same URL plus `.sig`.
pub const MANIFEST_URL: &str =
    "https://github.com/PhongGuy/dbento/releases/latest/download/latest.json";

/// The page a user downloads releases from by hand.
pub const DOWNLOAD_PAGE: &str = "https://github.com/PhongGuy/dbento/releases/latest";

/// Which program is asking about updates.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Product {
    /// The desktop app.
    App,
    /// The `dbento` command-line tool.
    Cli,
}

/// An operating system a release can target.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// Linux.
    Linux,
    /// macOS.
    Macos,
    /// Windows.
    Windows,
}

impl Os {
    /// The system this build runs on, or `None` for one no release targets.
    pub fn current() -> Option<Os> {
        match std::env::consts::OS {
            "linux" => Some(Os::Linux),
            "macos" => Some(Os::Macos),
            "windows" => Some(Os::Windows),
            _ => None,
        }
    }
}

/// How a release artifact is packaged.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A Linux AppImage (`appimage`).
    AppImage,
    /// A macOS app bundle, shipped as `.app.tar.gz` (`app`).
    App,
    /// A Windows NSIS installer (`nsis`).
    Nsis,
    /// A bare executable (`binary`).
    Binary,
}

/// Network time limits for the update check and the artifact download.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// Establishing a connection.
    pub connect: Duration,
    /// Fetching the manifest and its signature, each.
    pub manifest: Duration,
    /// The whole artifact download.
    pub artifact: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: Duration::from_secs(10),
            manifest: Duration::from_secs(30),
            artifact: Duration::from_secs(15 * 60),
        }
    }
}

/// Everything a check needs: where the manifest lives, which keys may sign it, and what this
/// build is.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct UpdateConfig {
    manifest_url: String,
    download_page: String,
    keys: Vec<String>,
    current_version: String,
    target_id: String,
    timeouts: Timeouts,
}

impl UpdateConfig {
    /// The URL of `latest.json`.
    pub fn manifest_url(&self) -> &str {
        &self.manifest_url
    }

    /// The page a user downloads releases from by hand.
    pub fn download_page(&self) -> &str {
        &self.download_page
    }

    /// Trusted minisign public keys, one base64 `.pub` line each.
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    /// This build's version.
    pub fn current_version(&self) -> &str {
        &self.current_version
    }

    /// The manifest target this build installs from, as [`target_id`] gives it.
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    /// The network time limits.
    pub fn timeouts(&self) -> Timeouts {
        self.timeouts
    }

    /// This config with other network time limits.
    #[must_use]
    pub fn with_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }
}

/// The release notes page for `version`.
pub fn release_notes_url(version: &str) -> String {
    format!("https://github.com/PhongGuy/dbento/releases/tag/v{version}")
}

/// The manifest target id for `product` on this host, `<app|cli>-<os>-<arch>`, or `None` on a
/// system or architecture no release ships for.
pub fn target_id(product: Product) -> Option<String> {
    let product = match product {
        Product::App => "app",
        Product::Cli => "cli",
    };
    let os = match Os::current()? {
        Os::Linux => "linux",
        Os::Macos => "macos",
        Os::Windows => "windows",
    };
    let arch = match std::env::consts::ARCH {
        arch @ ("x86_64" | "aarch64") => arch,
        _ => return None,
    };
    Some(format!("{product}-{os}-{arch}"))
}

/// The configuration for `product` from explicit inputs. `keys_text` holds one key per line;
/// blank lines and `#` comments are ignored. A missing or empty URL, no key, or a host no
/// release targets is [`CheckError::Disabled`]; a URL outside the URL rule is
/// [`CheckError::ManifestInvalid`].
pub fn build_config_with(
    product: Product,
    manifest_url: Option<&str>,
    download_page: Option<&str>,
    keys_text: &str,
    current_version: &str,
) -> Result<UpdateConfig, CheckError> {
    let keys: Vec<String> = keys_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();
    let (Some(manifest_url), Some(download_page), Some(target_id)) = (
        manifest_url.filter(|u| !u.is_empty()),
        download_page.filter(|p| !p.is_empty()),
        target_id(product),
    ) else {
        return Err(CheckError::Disabled);
    };
    if keys.is_empty() {
        return Err(CheckError::Disabled);
    }
    if !Url::parse(manifest_url).is_ok_and(|u| allowed(&u)) {
        return Err(CheckError::ManifestInvalid(format!(
            "the manifest URL must be https: {manifest_url}"
        )));
    }
    // A loopback download page serves local runs only, so release builds insist on https.
    let page_ok = Url::parse(download_page)
        .is_ok_and(|u| u.scheme() == "https" || (cfg!(debug_assertions) && allowed(&u)));
    if !page_ok {
        return Err(CheckError::ManifestInvalid(format!(
            "the download page must be https: {download_page}"
        )));
    }
    Ok(UpdateConfig {
        manifest_url: manifest_url.to_owned(),
        download_page: download_page.to_owned(),
        keys,
        current_version: current_version.to_owned(),
        target_id,
        timeouts: Timeouts::default(),
    })
}

/// The configuration this build was released with: the public feed, unless
/// `DBENTO_UPDATE_MANIFEST_URL` overrides it, and the keys from `assets/update/keys.txt`.
/// A debug build without the override is [`CheckError::Disabled`].
pub fn build_config(product: Product) -> Result<UpdateConfig, CheckError> {
    let manifest_url = feed(
        std::env::var("DBENTO_UPDATE_MANIFEST_URL").ok(),
        cfg!(debug_assertions),
    );
    build_config_with(
        product,
        manifest_url.as_deref(),
        Some(DOWNLOAD_PAGE),
        include_str!("../../../../assets/update/keys.txt"),
        env!("CARGO_PKG_VERSION"),
    )
}

// A non-empty override always wins. Without one, only release builds use the compiled feed,
// so a source build never offers to replace itself with a published release.
fn feed(override_url: Option<String>, debug: bool) -> Option<String> {
    match override_url.filter(|u| !u.is_empty()) {
        Some(url) => Some(url),
        None if debug => None,
        None => Some(MANIFEST_URL.to_owned()),
    }
}

/// The URL rule for update traffic: `https`, or plain `http` to a loopback host.
pub(crate) fn allowed(url: &Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => match url.host() {
            Some(Host::Domain(name)) => name == "localhost",
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{MANIFEST_URL, feed};

    #[test]
    fn debug_builds_need_the_override_and_release_builds_default_to_the_feed() {
        assert_eq!(feed(None, true), None);
        assert_eq!(feed(Some(String::new()), true), None);
        let local = "http://127.0.0.1:9/latest.json";
        assert_eq!(feed(Some(local.into()), true).as_deref(), Some(local));
        assert_eq!(feed(None, false).as_deref(), Some(MANIFEST_URL));
        assert_eq!(
            feed(Some(String::new()), false).as_deref(),
            Some(MANIFEST_URL)
        );
        assert_eq!(feed(Some(local.into()), false).as_deref(), Some(local));
    }
}
