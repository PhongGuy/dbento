//! `dbento self-update`: checks the signed release feed and, when this copy is a standalone
//! download, replaces it in place. Every other install is pointed at the download page.

use std::path::PathBuf;

use dbento_core::update::{
    self, Available, CheckError, InstallKind, Os, Product, Reason, UpdateConfig,
};

use crate::Exit;
use crate::confirm::confirm;
use crate::interrupt::Interrupt;

/// The `self-update` arguments.
#[derive(clap::Args)]
pub(crate) struct SelfUpdateArgs {
    /// Only report whether a newer release exists; exit 2 when one does.
    #[arg(long)]
    check: bool,
    /// Install without asking.
    #[arg(long)]
    yes: bool,
}

/// Checks for a newer release and installs it when this copy owns its install. Every failure
/// is reported here and maps to [`Exit::Failed`]; "available" under `--check` is [`Exit::Refused`].
pub(crate) async fn run(args: &SelfUpdateArgs) -> Exit {
    let current = env!("CARGO_PKG_VERSION");
    println!("dbento {current}");
    let checked = match update::build_config(Product::Cli) {
        Ok(cfg) => update::check(&cfg).await.map(|a| (cfg, a)),
        Err(e) => Err(e),
    };
    // A release published without a manifest has nothing to offer yet.
    let (cfg, available) = match checked {
        Ok((cfg, Some(a))) => (cfg, a),
        Ok((_, None)) | Err(CheckError::Http(404)) => {
            println!("dbento {current} is up to date");
            return Exit::Ok;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return Exit::Failed;
        }
    };
    let version = available.version();
    println!("dbento {version} is available");
    println!("Release notes: {}", update::release_notes_url(version));
    if args.check {
        return Exit::Refused;
    }
    match local_kind() {
        InstallKind::NotOwned(reason) => {
            println!("This dbento is not updated in place: {reason}.");
            println!("Download page: {}", cfg.download_page());
            Exit::Ok
        }
        kind => install(&cfg, &available, &kind, args.yes).await,
    }
}

// Downloads beside the binary and swaps it in, after the user agrees (or `--yes`).
async fn install(cfg: &UpdateConfig, available: &Available, kind: &InstallKind, yes: bool) -> Exit {
    let version = available.version();
    if !kind.accepts(available.format()) {
        eprintln!("error: the release does not ship a standalone dbento for this install");
        println!("Download page: {}", cfg.download_page());
        return Exit::Failed;
    }
    let interrupt = match Interrupt::install() {
        Ok(interrupt) => interrupt,
        Err(e) => {
            eprintln!("error: cannot handle Ctrl-C: {e}");
            return Exit::Failed;
        }
    };
    if !confirm(&format!("Install dbento {version}?"), yes, &interrupt).await {
        println!("Nothing changed.");
        return Exit::Ok;
    }
    let result = match update::staging_dir(kind) {
        Ok(dir) => match update::download(available, &dir, |_, _| {}).await {
            Ok(staged) => update::install(kind, staged).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        },
        Err(e) => Err(e.to_string()),
    };
    match result {
        Ok(_) => {
            println!("Installed dbento {version}.");
            Exit::Ok
        }
        Err(e) => {
            eprintln!("error: {e}");
            println!("Download page: {}", cfg.download_page());
            Exit::Failed
        }
    }
}

// The install this binary runs from, classified over its canonical path; a debug build is
// always a source build, so a test binary never replaces itself.
fn local_kind() -> InstallKind {
    let exe = std::env::current_exe().and_then(std::fs::canonicalize);
    let (Some(os), Ok(exe)) = (Os::current(), exe) else {
        return InstallKind::NotOwned(Reason::Unrecognised);
    };
    let appimage = std::env::var_os("APPIMAGE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let app = if cfg!(windows) {
        "dbento-app.exe"
    } else {
        "dbento-app"
    };
    let sibling = exe.with_file_name(app).exists();
    update::install_kind(
        Product::Cli,
        os,
        update::RELEASE_BUILD,
        &exe,
        appimage.as_deref(),
        sibling,
    )
}
