//! Pinned public datasets for `db up --datasets`: sha256-verified downloads into the cache.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Sent on every request; carries no personal address.
pub const USER_AGENT: &str = "dbento-research (+https://github.com)";

/// One file to download, pinned by hash.
#[derive(Debug, PartialEq, Eq)]
pub struct Download {
    pub url: &'static str,
    pub file: &'static str,
    pub sha256: &'static str,
}

/// A downloadable dataset and the database it loads into.
#[derive(Debug, PartialEq, Eq)]
pub struct Dataset {
    pub name: &'static str,
    pub database: &'static str,
    pub env_label: &'static str,
    pub license: &'static str,
    pub downloads: &'static [Download],
}

pub const PAGILA: Dataset = Dataset {
    name: "pagila",
    database: "pagila",
    env_label: "PAGILA",
    license: "PostgreSQL License per the README at pagila-v3.1.0; that tag's LICENSE.txt carries MIT text",
    downloads: &[
        Download {
            url: "https://raw.githubusercontent.com/devrimgunduz/pagila/fef9675714cfba1756df4719b5e36075a7ddf90e/pagila-schema.sql",
            file: "pagila-schema.sql",
            sha256: "8ce358e4c8014087b85296694a0893887bd7a4190e3ce407f2721b86b98e5707",
        },
        Download {
            url: "https://raw.githubusercontent.com/devrimgunduz/pagila/fef9675714cfba1756df4719b5e36075a7ddf90e/pagila-data.sql",
            file: "pagila-data.sql",
            sha256: "fb81bec377687c83e11d2a24916ae28656d85550bf0ada798305bf7e2af9823b",
        },
        Download {
            url: "https://raw.githubusercontent.com/devrimgunduz/pagila/fef9675714cfba1756df4719b5e36075a7ddf90e/LICENSE.txt",
            file: "LICENSE.txt",
            sha256: "516e7dac679ac1eeb62d5614b01c4e7318154e9a147377d6264954215997ff38",
        },
    ],
};

const AIRLINES_LICENSE: &str = "PostgreSQL License, per postgrespro.com/docs/postgrespro/13/demodb-bookings.html (2017-08-15 dump)";

pub const AIRLINES_SMALL: Dataset = Dataset {
    name: "airlines",
    database: "demo",
    env_label: "AIRLINES",
    license: AIRLINES_LICENSE,
    downloads: &[Download {
        url: "https://edu.postgrespro.com/demo-small-en.zip",
        file: "demo-small-en.zip",
        sha256: "ea70bc6382a91345ac3b91de3ed92934853495653374ebf60589a19453030c00",
    }],
};

pub const AIRLINES_MEDIUM: Dataset = Dataset {
    name: "airlines-medium",
    database: "demo",
    env_label: "AIRLINES",
    license: AIRLINES_LICENSE,
    downloads: &[Download {
        url: "https://edu.postgrespro.com/demo-medium-en.zip",
        file: "demo-medium-en.zip",
        sha256: "e14642631634362c9a267782d0b399e8ee2d05680b6d5f59540ffb1684374674",
    }],
};

pub const AIRLINES_BIG: Dataset = Dataset {
    name: "airlines-big",
    database: "demo",
    env_label: "AIRLINES",
    license: AIRLINES_LICENSE,
    downloads: &[Download {
        url: "https://edu.postgrespro.com/demo-big-en.zip",
        file: "demo-big-en.zip",
        sha256: "1db6b36d18b8eb68cf3bb81b720637a1bc3897635e73c02cdbf6374d56e91866",
    }],
};

/// One `--datasets` entry: a download, or the generated stress tables.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Choice {
    Download(&'static Dataset),
    Stress,
}

/// Parses a comma list, keeping order and dropping duplicates. An empty string is no datasets;
/// an empty entry, an unknown name or two airlines sizes is an error.
pub fn parse_datasets(s: &str) -> Result<Vec<Choice>> {
    let mut out: Vec<Choice> = Vec::new();
    if s.trim().is_empty() {
        return Ok(out);
    }
    for raw in s.split(',') {
        let name = raw.trim();
        let choice = match name {
            "" => bail!("empty dataset name in {s:?}"),
            "stress" => Choice::Stress,
            "pagila" => Choice::Download(&PAGILA),
            "airlines" => Choice::Download(&AIRLINES_SMALL),
            "airlines-medium" => Choice::Download(&AIRLINES_MEDIUM),
            "airlines-big" => Choice::Download(&AIRLINES_BIG),
            "imdb" => bail!("dbento never downloads IMDb: its licence forbids redistribution"),
            other => bail!(
                "unknown dataset {other:?}; choose from pagila, airlines, airlines-medium, airlines-big, stress"
            ),
        };
        if out.contains(&choice) {
            continue;
        }
        // All airlines sizes load into the same `demo` database.
        if let Choice::Download(d) = &choice
            && d.database == "demo"
            && out
                .iter()
                .any(|c| matches!(c, Choice::Download(o) if o.database == "demo"))
        {
            bail!("choose only one airlines size, not several");
        }
        out.push(choice);
    }
    Ok(out)
}

/// The curl invocation for one download: HTTPS only, redirects followed, fails on HTTP errors.
pub fn curl_args(url: &str, out: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "-f", "-L", "--retry", "3", "--proto", "=https", "-A", USER_AGENT, "-o",
    ]
    .map(String::from)
    .into();
    args.push(out.display().to_string());
    args.push(url.to_string());
    args
}

/// Removes a leftover `.part` (or a symlink planted there) so curl never writes through it.
pub fn clear_part(part: &Path) -> Result<()> {
    match std::fs::remove_file(part) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("remove stale {}", part.display()))
        }
        _ => Ok(()),
    }
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    // Chunked so the 243 MB airlines zip never sits in memory.
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(crate::db::hex(hasher.finalize()))
}

/// Checks the file's sha256; the error names both the expected and the actual hash.
pub fn verify_file(path: &Path, sha256: &str) -> Result<()> {
    let actual = hash_file(path)?;
    if actual != sha256 {
        bail!(
            "{}: sha256 mismatch, expected {sha256}, got {actual}",
            path.display()
        );
    }
    Ok(())
}

/// Returns `dir/file`, downloading it when absent or no longer matching its pin. The download
/// lands in a `.part` file that is renamed into place only after it verifies.
pub fn fetch(d: &Download, dir: &Path) -> Result<PathBuf> {
    let dest = dir.join(d.file);
    if dest.exists() {
        if verify_file(&dest, d.sha256).is_ok() {
            return Ok(dest);
        }
        std::fs::remove_file(&dest).with_context(|| format!("remove stale {}", dest.display()))?;
    }
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let part = dir.join(format!("{}.part", d.file));
    let result = clear_part(&part)
        .and_then(|()| download_to(d, &part))
        .and_then(|()| {
            std::fs::rename(&part, &dest).with_context(|| format!("rename into {}", dest.display()))
        });
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result.map(|()| dest)
}

fn download_to(d: &Download, part: &Path) -> Result<()> {
    let status = Command::new("curl")
        .args(curl_args(d.url, part))
        .status()
        .context("run curl (a host prerequisite for --datasets; install curl)")?;
    if !status.success() {
        bail!("curl failed for {} ({status})", d.url);
    }
    verify_file(part, d.sha256)
}

/// The text written to `SOURCE.txt` beside a dataset's files.
pub fn source_note(d: &Dataset) -> String {
    let mut note = format!("Dataset: {}\nLicense: {}\n\nFiles:\n", d.name, d.license);
    for f in d.downloads {
        note.push_str(&format!("{}\n  sha256 {}\n", f.url, f.sha256));
    }
    note
}
