//! `dbento import datagrip`, `dbento import dbeaver` and `dbento import beekeeper`: read a
//! source, plan against the workspace, apply, store any saved secrets, and report.

use std::path::{Path, PathBuf};

use clap::ValueEnum;
use dbento_core::import::beekeeper::{BeekeeperOptions, read_beekeeper};
use dbento_core::import::datagrip::{DataGripOptions, jetbrains_config_dirs, read_datagrip};
use dbento_core::import::dbeaver::{DBeaverOptions, read_dbeaver};
use dbento_core::import::{
    Action, ApplyError, ImportSecrets, ImportSet, OnConflict, SecretSetting, Warning, plan,
    store_secrets,
};
use serde::Serialize;

use crate::{Exit, secret, table};

#[derive(Clone, Copy, ValueEnum)]
enum ConflictArg {
    Skip,
    Rename,
}

impl From<ConflictArg> for OnConflict {
    fn from(arg: ConflictArg) -> Self {
        match arg {
            ConflictArg::Skip => OnConflict::Skip,
            ConflictArg::Rename => OnConflict::Rename,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ReportFormat {
    Table,
    Json,
}

#[derive(clap::Args)]
pub(crate) struct DataGripArgs {
    /// The project folder or its `.idea` folder.
    folder: PathBuf,
    /// Report what would happen and write nothing.
    #[arg(long)]
    dry_run: bool,
    /// What to do when a connection name is taken by a different connection.
    #[arg(long, value_enum, default_value_t = ConflictArg::Skip)]
    on_conflict: ConflictArg,
    /// The report format.
    #[arg(long, value_enum, default_value_t = ReportFormat::Table)]
    format: ReportFormat,
    /// A JetBrains config root to read consoles from, such as `~/.config/JetBrains`.
    #[arg(long = "jetbrains-config")]
    jetbrains_config: Vec<PathBuf>,
    /// Do not import JetBrains consoles.
    #[arg(long)]
    no_consoles: bool,
    /// The user for a Postgres or MySQL source that names none.
    #[arg(long)]
    default_user: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct DBeaverArgs {
    /// The project folder or its `.dbeaver` folder.
    folder: PathBuf,
    /// Report what would happen and write nothing.
    #[arg(long)]
    dry_run: bool,
    /// What to do when a connection name is taken by a different connection.
    #[arg(long, value_enum, default_value_t = ConflictArg::Skip)]
    on_conflict: ConflictArg,
    /// The report format.
    #[arg(long, value_enum, default_value_t = ReportFormat::Table)]
    format: ReportFormat,
    /// The user for a Postgres or MySQL connection that names none.
    #[arg(long)]
    default_user: Option<String>,
    /// Do not store saved passwords in the keychain; user names are still imported.
    #[arg(long)]
    no_secrets: bool,
}

#[derive(clap::Args)]
pub(crate) struct BeekeeperArgs {
    /// The Beekeeper Studio data folder holding app.db, or app.db itself.
    path: PathBuf,
    /// Report what would happen and write nothing.
    #[arg(long)]
    dry_run: bool,
    /// What to do when a connection name is taken by a different connection.
    #[arg(long, value_enum, default_value_t = ConflictArg::Skip)]
    on_conflict: ConflictArg,
    /// The report format.
    #[arg(long, value_enum, default_value_t = ReportFormat::Table)]
    format: ReportFormat,
    /// The user for a Postgres or MySQL connection that names none.
    #[arg(long)]
    default_user: Option<String>,
    /// Do not read saved passwords or the key file; user names are still imported.
    #[arg(long)]
    no_secrets: bool,
}

/// The flags shared by the importers that carry saved secrets.
#[derive(Clone, Copy)]
struct SecretImportFlags {
    dry_run: bool,
    on_conflict: ConflictArg,
    format: ReportFormat,
    no_secrets: bool,
}

#[derive(Serialize)]
struct ConnectionEntry {
    name: String,
    from: String,
    action: &'static str,
}

#[derive(Serialize)]
struct QueryEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    from: String,
    action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

#[derive(Serialize)]
struct SkippedEntry {
    kind: &'static str,
    from: String,
    reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize)]
struct WarningEntry {
    from: String,
    reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize)]
struct Report {
    dbento: u32,
    source: String,
    workspace: String,
    dry_run: bool,
    connections: Vec<ConnectionEntry>,
    queries: Vec<QueryEntry>,
    skipped: Vec<SkippedEntry>,
    warnings: Vec<WarningEntry>,
    next_steps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    secrets: Option<SecretsEntry>,
}

/// The connection names whose secret was stored, already there, or could not be stored.
#[derive(Serialize, Default)]
struct SecretsEntry {
    stored: Vec<String>,
    present: Vec<String>,
    failed: Vec<String>,
}

/// The workspace folder: the flag as given, else the nearest ancestor holding `dbento.toml`,
/// else the working directory (where a fresh workspace is created).
fn workspace_dir(flag: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(dir) = flag {
        return Ok(dir.to_path_buf());
    }
    let cwd = std::env::current_dir()?;
    let nearest = cwd
        .ancestors()
        .find(|a| std::fs::symlink_metadata(a.join("dbento.toml")).is_ok());
    Ok(nearest.unwrap_or(&cwd).to_path_buf())
}

/// The JetBrains product folders to read consoles from. Only an explicit root, or the
/// platform default when none is given, is ever scanned.
fn console_dirs(args: &DataGripArgs, home: Option<&Path>) -> Vec<PathBuf> {
    if args.no_consoles {
        return Vec::new();
    }
    let roots: Vec<PathBuf> = if args.jetbrains_config.is_empty() {
        platform_root(home).into_iter().collect()
    } else {
        args.jetbrains_config.clone()
    };
    roots
        .iter()
        .flat_map(|r| jetbrains_config_dirs(r))
        .collect()
}

fn platform_root(home: Option<&Path>) -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| Path::new(&a).join("JetBrains"))
    } else if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support/JetBrains"))
    } else {
        home.map(|h| h.join(".config/JetBrains"))
    }
}

pub(crate) fn datagrip(workspace: Option<&Path>, args: &DataGripArgs) -> anyhow::Result<Exit> {
    let ws_dir = workspace_dir(workspace)?;
    let home = std::env::home_dir();
    let opts = DataGripOptions::new()
        .config_dirs(console_dirs(args, home.as_deref()))
        .home(home)
        .default_user(args.default_user.clone());
    let set = read_datagrip(&args.folder, &opts)?;
    let import = plan(&ws_dir, &set, args.on_conflict.into())?;
    let mut report = build_report(
        &args.folder,
        args.dry_run,
        &ws_dir,
        &set,
        import.actions(),
        import.warnings(),
    );
    if !args.dry_run
        && let Err(e) = import.apply()
    {
        return report_apply_error(&mut report, &ws_dir, &e, args.format);
    }
    print_report(&report, args.format)?;
    Ok(Exit::Ok)
}

pub(crate) fn dbeaver(workspace: Option<&Path>, args: &DBeaverArgs) -> anyhow::Result<Exit> {
    let ws_dir = workspace_dir(workspace)?;
    let opts = DBeaverOptions::new()
        .default_user(args.default_user.clone())
        .secrets(!args.no_secrets);
    let (set, secrets) = read_dbeaver(&args.folder, &opts)?.into_parts();
    let flags = SecretImportFlags {
        dry_run: args.dry_run,
        on_conflict: args.on_conflict,
        format: args.format,
        no_secrets: args.no_secrets,
    };
    apply_with_secrets(&ws_dir, &args.folder, &set, &secrets, flags)
}

pub(crate) fn beekeeper(workspace: Option<&Path>, args: &BeekeeperArgs) -> anyhow::Result<Exit> {
    let ws_dir = workspace_dir(workspace)?;
    let opts = BeekeeperOptions::new()
        .default_user(args.default_user.clone())
        .secrets(!args.no_secrets);
    let (set, secrets) = read_beekeeper(&args.path, &opts)?.into_parts();
    let flags = SecretImportFlags {
        dry_run: args.dry_run,
        on_conflict: args.on_conflict,
        format: args.format,
        no_secrets: args.no_secrets,
    };
    apply_with_secrets(&ws_dir, &args.path, &set, &secrets, flags)
}

/// Plans `set` against the workspace, applies it unless a dry run, stores `secrets` in the
/// keychain unless `no_secrets`, and prints the report with its `secrets` section.
fn apply_with_secrets(
    ws_dir: &Path,
    source: &Path,
    set: &ImportSet,
    secrets: &ImportSecrets,
    flags: SecretImportFlags,
) -> anyhow::Result<Exit> {
    let import = plan(ws_dir, set, flags.on_conflict.into())?;
    let mut report = build_report(
        source,
        flags.dry_run,
        ws_dir,
        set,
        import.actions(),
        import.warnings(),
    );
    report.secrets = Some(SecretsEntry::default());
    if flags.dry_run {
        print_report(&report, flags.format)?;
        return Ok(Exit::Ok);
    }
    let imported = import.imported().to_vec();
    if let Err(e) = import.apply() {
        return report_apply_error(&mut report, ws_dir, &e, flags.format);
    }
    // Secrets go to the keychain only once dbento.toml holds the connections they belong to.
    if !flags.no_secrets {
        let env = |v: &str| std::env::var(v).ok();
        let outcome = store_secrets(ws_dir, &imported, secrets, &secret::store(), &env);
        for (name, error) in outcome.failed() {
            eprintln!("warning: the secret for '{name}' was not stored: {error}");
        }
        let names = |v: Vec<&str>| v.into_iter().map(str::to_owned).collect::<Vec<_>>();
        report.next_steps = outcome
            .needs_secret()
            .into_iter()
            .map(|n| format!("dbento secret set {n}"))
            .collect();
        report.secrets = Some(SecretsEntry {
            stored: names(outcome.stored()),
            present: names(outcome.present()),
            failed: names(outcome.failed().into_iter().map(|(n, _)| n).collect()),
        });
    }
    print_report(&report, flags.format)?;
    Ok(Exit::Ok)
}

/// Prints the report of a failed apply, with what did not land marked, then the error and
/// every file that was written.
fn report_apply_error(
    report: &mut Report,
    ws_dir: &Path,
    e: &ApplyError,
    format: ReportFormat,
) -> anyhow::Result<Exit> {
    let dir = std::fs::canonicalize(ws_dir).ok();
    mark_unwritten(report, dir.as_deref(), e.written());
    print_report(report, format)?;
    eprintln!("error: {e}");
    for path in e.written() {
        eprintln!("written: {}", path.display());
    }
    Ok(exit_for(e))
}

/// Exit 1 when some files were already written, else 3 as for any failure before a write.
pub(crate) fn exit_for(e: &ApplyError) -> Exit {
    if e.written().is_empty() {
        Exit::Usage
    } else {
        Exit::Failed
    }
}

fn build_report(
    source: &Path,
    dry_run: bool,
    ws_dir: &Path,
    set: &ImportSet,
    actions: &[Action],
    plan_warnings: &[Warning],
) -> Report {
    // Connection actions come first, one per set entry in set order; queries follow.
    let (conn_actions, query_actions) = actions.split_at(set.connections().len());
    let mut connections = Vec::new();
    let mut next_steps = Vec::new();
    for (c, a) in set.connections().iter().zip(conn_actions) {
        let (Action::AddConnection { name, source_name }
        | Action::ConnectionUnchanged { name, source_name }
        | Action::ConnectionConflict { name, source_name }) = a
        else {
            continue;
        };
        if matches!(a, Action::AddConnection { .. })
            && c.draft().engine().capabilities().password
            && c.draft().secret() == Some(SecretSetting::Keychain)
        {
            next_steps.push(format!("dbento secret set {name}"));
        }
        connections.push(ConnectionEntry {
            name: name.clone(),
            from: source_name.clone(),
            action: a.code(),
        });
    }
    let queries = query_actions
        .iter()
        .filter_map(|a| match a {
            Action::CreateQuery { path, origin } | Action::QueryUnchanged { path, origin } => {
                Some(QueryEntry {
                    path: Some(path.display().to_string()),
                    from: origin.display().to_string(),
                    action: a.code(),
                    reason: None,
                })
            }
            Action::SkipQuery { origin, reason } => Some(QueryEntry {
                path: None,
                from: origin.display().to_string(),
                action: a.code(),
                reason: Some(reason.code()),
            }),
            _ => None,
        })
        .collect();
    let skipped = set
        .skipped()
        .iter()
        .map(|s| SkippedEntry {
            kind: s.kind().code(),
            from: s.subject().to_owned(),
            reason: s.reason().code(),
            detail: s.reason().detail().map(str::to_owned),
        })
        .collect();
    let warnings = set
        .warnings()
        .iter()
        .chain(plan_warnings)
        .map(|w| WarningEntry {
            from: w.subject().to_owned(),
            reason: w.kind().code(),
            detail: w.kind().detail().map(str::to_owned),
        })
        .collect();
    Report {
        dbento: 1,
        source: source.display().to_string(),
        workspace: ws_dir.display().to_string(),
        dry_run,
        connections,
        queries,
        skipped,
        warnings,
        next_steps,
        secrets: None,
    }
}

fn print_report(report: &Report, format: ReportFormat) -> anyhow::Result<()> {
    match format {
        ReportFormat::Json => println!("{}", serde_json::to_string(report)?),
        ReportFormat::Table => {
            let text = render_table(report);
            if !text.is_empty() {
                println!("{text}");
            }
        }
    }
    Ok(())
}

/// The table report: one block per non-empty section, separated by a blank line.
fn render_table(report: &Report) -> String {
    let mut blocks = Vec::new();
    let mut block = |headers: &[&str], rows: Vec<Vec<String>>| {
        if !rows.is_empty() {
            blocks.push(table::render(headers, &rows));
        }
    };
    block(
        &["name", "from", "action"],
        report
            .connections
            .iter()
            .map(|c| vec![c.name.clone(), c.from.clone(), c.action.to_owned()])
            .collect(),
    );
    block(
        &["path", "from", "action"],
        report
            .queries
            .iter()
            .map(|q| {
                vec![
                    q.path.clone().unwrap_or_default(),
                    q.from.clone(),
                    q.action.to_owned(),
                ]
            })
            .collect(),
    );
    block(
        &["kind", "from", "reason", "detail"],
        report
            .skipped
            .iter()
            .map(|s| {
                vec![
                    s.kind.to_owned(),
                    s.from.clone(),
                    s.reason.to_owned(),
                    s.detail.clone().unwrap_or_default(),
                ]
            })
            .collect(),
    );
    block(
        &["from", "reason", "detail"],
        report
            .warnings
            .iter()
            .map(|w| {
                vec![
                    w.from.clone(),
                    w.reason.to_owned(),
                    w.detail.clone().unwrap_or_default(),
                ]
            })
            .collect(),
    );
    if let Some(s) = report.secrets.as_ref().filter(|s| !s.stored.is_empty()) {
        blocks.push(format!("stored secrets: {}", s.stored.join(", ")));
    }
    if !report.next_steps.is_empty() {
        let mut steps = vec!["next steps:".to_owned()];
        steps.extend(report.next_steps.iter().cloned());
        blocks.push(steps.join("\n"));
    }
    blocks.join("\n\n")
}

/// Marks every planned write missing from `written` as `not_written`. `dir` is the resolved
/// workspace folder, or None when it does not exist (so nothing landed).
fn mark_unwritten(report: &mut Report, dir: Option<&Path>, written: &[PathBuf]) {
    let landed = |rel: &str| dir.is_some_and(|d| written.contains(&d.join(rel)));
    for q in &mut report.queries {
        if q.action == "create" && !q.path.as_deref().is_some_and(landed) {
            q.action = NOT_WRITTEN;
        }
    }
    // Without dbento.toml the added connections, and so their secret steps, do not exist.
    if !landed("dbento.toml") {
        for c in &mut report.connections {
            if c.action == "add" {
                c.action = NOT_WRITTEN;
            }
        }
        report.next_steps.clear();
    }
}

const NOT_WRITTEN: &str = "not_written";

#[cfg(test)]
mod tests {
    use super::*;
    use dbento_core::import::ApplyError;
    use std::path::PathBuf;

    #[test]
    fn apply_failures_exit_1_only_after_a_write() {
        let none = ApplyError::Changed { written: vec![] };
        let some = ApplyError::Io {
            path: PathBuf::from("q.sql"),
            message: "disk full".into(),
            written: vec![PathBuf::from("a.sql")],
        };
        assert_eq!(exit_for(&none), Exit::Usage);
        assert_eq!(exit_for(&some), Exit::Failed);
    }

    fn empty_report() -> Report {
        Report {
            dbento: 1,
            source: "p".into(),
            workspace: "w".into(),
            dry_run: false,
            connections: vec![],
            queries: vec![],
            skipped: vec![],
            warnings: vec![],
            next_steps: vec![],
            secrets: None,
        }
    }

    #[test]
    fn an_empty_report_renders_no_table_text_at_all() {
        assert_eq!(render_table(&empty_report()), "");
    }

    #[test]
    fn the_table_shows_a_skip_or_warning_detail() {
        let mut report = empty_report();
        report.skipped.push(SkippedEntry {
            kind: "connection",
            from: "old".into(),
            reason: "unsupported_driver",
            detail: Some("oracle.thin".into()),
        });
        report.warnings.push(WarningEntry {
            from: "shop".into(),
            reason: "dropped_parameter",
            detail: Some("loggerLevel".into()),
        });
        let text = render_table(&report);
        assert!(text.contains("oracle.thin"), "{text}");
        assert!(text.contains("loggerLevel"), "{text}");
    }

    #[test]
    fn after_a_failed_apply_unwritten_files_are_marked_and_no_secret_step_is_left() {
        let mut report = empty_report();
        report.connections.push(ConnectionEntry {
            name: "shop".into(),
            from: "Shop".into(),
            action: "add",
        });
        for path in ["queries/a.sql", "queries/b.sql"] {
            report.queries.push(QueryEntry {
                path: Some(path.into()),
                from: format!("/s/{path}"),
                action: "create",
                reason: None,
            });
        }
        report.next_steps.push("dbento secret set shop".into());
        let dir = Path::new("/ws");
        mark_unwritten(&mut report, Some(dir), &[dir.join("queries/a.sql")]);
        let actions: Vec<&str> = report.queries.iter().map(|q| q.action).collect();
        assert_eq!(actions, ["create", "not_written"]);
        assert_eq!(report.connections[0].action, "not_written");
        assert!(report.next_steps.is_empty());
    }

    #[test]
    fn the_table_names_stored_secrets_before_the_next_steps() {
        let mut report = empty_report();
        report.secrets = Some(SecretsEntry {
            stored: vec!["shop".into(), "orders".into()],
            ..Default::default()
        });
        report.next_steps.push("dbento secret set stock".into());
        let text = render_table(&report);
        assert!(
            text.ends_with("stored secrets: shop, orders\n\nnext steps:\ndbento secret set stock"),
            "{text}"
        );
    }
}
