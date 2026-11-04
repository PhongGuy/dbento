//! `cargo xtask icons [--check]`: render the PNG, ICO and ICNS exports from the SVG sources.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Context;
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg;

pub const PNG_SIZES: [u32; 9] = [16, 24, 32, 48, 64, 128, 256, 512, 1024];
pub const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// The ICNS slots and the pixel size each holds, all drawn from the macOS-grid source.
const ICNS_SLOTS: [(icns::IconType, u32); 10] = [
    (icns::IconType::RGBA32_16x16, 16),
    (icns::IconType::RGBA32_16x16_2x, 32),
    (icns::IconType::RGBA32_32x32, 32),
    (icns::IconType::RGBA32_32x32_2x, 64),
    (icns::IconType::RGBA32_128x128, 128),
    (icns::IconType::RGBA32_128x128_2x, 256),
    (icns::IconType::RGBA32_256x256, 256),
    (icns::IconType::RGBA32_256x256_2x, 512),
    (icns::IconType::RGBA32_512x512, 512),
    (icns::IconType::RGBA32_512x512_2x, 1024),
];
const MACOS_SOURCE: &str = "dbento-macos.svg";

/// One generated file: its path relative to `generated/` and its exact bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

/// The directory holding the SVG sources: `<root>/assets/icon`.
pub fn icon_dir() -> PathBuf {
    crate::workspace_root().join("assets").join("icon")
}

/// The SVG drawn at `size` px: `dbento-16.svg` to 16, `dbento-32.svg` to 48, else `dbento.svg`.
pub fn source_for(size: u32) -> &'static str {
    match size {
        ..=16 => "dbento-16.svg",
        17..=48 => "dbento-32.svg",
        _ => "dbento.svg",
    }
}

/// Renders every export from the SVGs in `icon_dir`: the PNG set, then the ICO, then the ICNS.
pub fn render_exports(icon_dir: &Path) -> anyhow::Result<Vec<Export>> {
    let mut exports = Vec::new();
    for size in PNG_SIZES {
        let pixmap = render(icon_dir, source_for(size), size)?;
        exports.push(Export {
            path: PathBuf::from(format!("png/{size}x{size}.png")),
            bytes: pixmap.encode_png().context("encoding PNG")?,
        });
    }

    let mut ico = ico::IconDir::new(ico::ResourceType::Icon);
    for size in ICO_SIZES {
        let pixmap = render(icon_dir, source_for(size), size)?;
        let image = ico::IconImage::from_rgba_data(size, size, straight_rgba(&pixmap));
        ico.add_entry(ico::IconDirEntry::encode(&image).context("encoding ICO entry")?);
    }
    let mut bytes = Vec::new();
    ico.write(&mut bytes).context("writing ICO")?;
    exports.push(Export {
        path: "dbento.ico".into(),
        bytes,
    });

    let mut icns = icns::IconFamily::new();
    for (slot, size) in ICNS_SLOTS {
        let pixmap = render(icon_dir, MACOS_SOURCE, size)?;
        let image =
            icns::Image::from_data(icns::PixelFormat::RGBA, size, size, straight_rgba(&pixmap))?;
        icns.add_icon_with_type(&image, slot)
            .with_context(|| format!("adding ICNS slot {slot:?}"))?;
    }
    let mut bytes = Vec::new();
    icns.write(&mut bytes).context("writing ICNS")?;
    exports.push(Export {
        path: "dbento.icns".into(),
        bytes,
    });

    Ok(exports)
}

/// Draws `svg` scaled to fill a `size` square.
fn render(icon_dir: &Path, svg: &str, size: u32) -> anyhow::Result<Pixmap> {
    let path = icon_dir.join(svg);
    let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default())
        .with_context(|| format!("parsing {}", path.display()))?;
    let mut pixmap = Pixmap::new(size, size).context("allocating pixmap")?;
    let scale = size as f32;
    let transform =
        Transform::from_scale(scale / tree.size().width(), scale / tree.size().height());
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(pixmap)
}

/// tiny-skia stores premultiplied pixels; ICO and ICNS take straight RGBA.
fn straight_rgba(pixmap: &Pixmap) -> Vec<u8> {
    pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

/// Sorted export paths whose file under `generated` differs or is missing, plus every
/// non-export file there.
pub fn stale(generated: &Path, exports: &[Export]) -> anyhow::Result<Vec<PathBuf>> {
    let mut on_disk = BTreeSet::new();
    if generated.exists() {
        collect_files(generated, Path::new(""), &mut on_disk)?;
    }
    let mut stale = BTreeSet::new();
    for export in exports {
        let current = on_disk
            .remove(&export.path)
            .then(|| std::fs::read(generated.join(&export.path)))
            .transpose()
            .with_context(|| format!("reading {}", export.path.display()))?;
        if current.as_deref() != Some(export.bytes.as_slice()) {
            stale.insert(export.path.clone());
        }
    }
    // Whatever is left on disk is not an export at all.
    stale.extend(on_disk);
    Ok(stale.into_iter().collect())
}

fn collect_files(root: &Path, rel: &Path, out: &mut BTreeSet<PathBuf>) -> anyhow::Result<()> {
    let dir = root.join(rel);
    for entry in std::fs::read_dir(&dir).with_context(|| format!("listing {}", dir.display()))? {
        let entry = entry?;
        let rel = rel.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            collect_files(root, &rel, out)?;
        } else {
            out.insert(rel);
        }
    }
    Ok(())
}

/// Replaces `generated` with exactly `exports`, removing anything else it held.
pub fn write(generated: &Path, exports: &[Export]) -> anyhow::Result<()> {
    if generated.exists() {
        std::fs::remove_dir_all(generated)
            .with_context(|| format!("removing {}", generated.display()))?;
    }
    for export in exports {
        let path = generated.join(&export.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&path, &export.bytes)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// The `--check` failure text: one `stale: assets/icon/generated/<path>` line per path, then
/// the fix.
pub fn stale_message(stale: &[PathBuf]) -> String {
    let mut lines: Vec<String> = stale
        .iter()
        .map(|path| {
            // Forward slashes on every OS, so the message matches the repository path.
            let shown: Vec<_> = path.iter().map(|part| part.to_string_lossy()).collect();
            format!("stale: assets/icon/generated/{}", shown.join("/"))
        })
        .collect();
    lines.push("run `cargo xtask icons` and commit the result".to_owned());
    lines.join("\n")
}

/// Renders from `icon_dir`; without `check` writes `icon_dir/generated` and returns 0, with
/// `check` returns 0 when nothing is stale, else prints `stale_message` to stderr and returns 1.
pub fn run_in(icon_dir: &Path, check: bool) -> anyhow::Result<i32> {
    let generated = icon_dir.join("generated");
    let exports = render_exports(icon_dir)?;
    if !check {
        write(&generated, &exports)?;
        return Ok(0);
    }
    let stale = stale(&generated, &exports)?;
    if stale.is_empty() {
        return Ok(0);
    }
    eprintln!("{}", stale_message(&stale));
    Ok(1)
}

/// `run_in(&icon_dir(), check)`.
pub fn run(check: bool) -> anyhow::Result<i32> {
    run_in(&icon_dir(), check)
}
