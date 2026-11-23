//! The `dbento` command line: argument parsing and the mapping of outcomes to exit codes.

mod cell;
mod confirm;
mod csv;
mod exec;
mod import;
mod interrupt;
mod json;
mod mcp;
mod run;
mod secret;
mod self_update;
mod table;
#[cfg(feature = "team")]
mod team;
#[cfg(feature = "team")]
mod team_folder;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use dbento_core::{Color, ConnectionConfig, Databases, Workspace};
use serde::Serialize;

/// How a run ended; the highest code wins when several things went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Exit {
    Ok = 0,
    Failed = 1,
    Refused = 2,
    Usage = 3,
    #[cfg_attr(
        not(feature = "team"),
        allow(dead_code, reason = "only team folder commands conflict")
    )]
    Conflicted = 4,
    #[cfg_attr(
        not(feature = "team"),
        allow(dead_code, reason = "only team commands sign out")
    )]
    SignedOut = 5,
}

#[derive(Parser)]
#[command(name = "dbento", version, about)]
struct Cli {
    /// The workspace directory; without it the nearest dbento.toml above the working directory.
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create dbento.toml and queries/ in the workspace directory or the working directory.
    Init,
    /// List the workspace's connections.
    Connections {
        /// The output format.
        #[arg(long, value_enum, default_value_t = ListFormat::Table)]
        format: ListFormat,
    },
    /// Run SQL on a connection and stream the results.
    Query {
        /// The connection name from dbento.toml.
        conn: String,
        /// One or more statements. SQL may begin with a `--` comment, so hyphens are allowed.
        #[arg(allow_hyphen_values = true)]
        sql: String,
        #[command(flatten)]
        exec: exec::ExecArgs,
    },
    /// Run a saved query file; its connection is its `queries/<conn>/` folder unless `--conn` is given.
    Run {
        /// The query file (.sql, .js, .surql, .redis or .partiql (DynamoDB), matching the
        /// connection's language).
        file: PathBuf,
        /// The connection to run on.
        #[arg(long)]
        conn: Option<String>,
        #[command(flatten)]
        exec: exec::ExecArgs,
    },
    /// Import connections and saved queries from another tool.
    Import {
        #[command(subcommand)]
        source: ImportSource,
    },
    /// Manage a connection's keychain secret.
    Secret {
        #[command(subcommand)]
        action: SecretAction,
    },
    /// Serve the workspace's databases to an AI client over stdio (MCP); writes need approval in the app.
    Mcp(mcp::McpArgs),
    /// Check for a newer dbento and install it when this copy is a standalone download.
    SelfUpdate(self_update::SelfUpdateArgs),
    /// Sync a workspace with a Potluck team server.
    #[cfg(feature = "team")]
    Team {
        #[command(subcommand)]
        action: team::TeamCommand,
    },
}

#[derive(Subcommand)]
enum ImportSource {
    /// A DataGrip project folder or `.idea` folder.
    Datagrip(import::DataGripArgs),
    /// A DBeaver project folder or `.dbeaver` folder.
    Dbeaver(import::DBeaverArgs),
    /// A Beekeeper Studio data folder or its app.db.
    Beekeeper(import::BeekeeperArgs),
}

#[derive(Subcommand)]
enum SecretAction {
    /// Store the secret, read from the terminal prompt or the first line of stdin.
    Set {
        /// Store the passphrase of the connection's encrypted `tls_key` instead of its password.
        #[arg(long)]
        tls_key: bool,
        /// Store the password of the connection's SSH tunnel instead.
        #[arg(long)]
        ssh_password: bool,
        /// Store the passphrase of the SSH tunnel's `identity_file` instead.
        #[arg(long)]
        ssh_key_passphrase: bool,
        /// The connection name from dbento.toml.
        conn: String,
        /// Caught only to refuse it: a secret is never taken from the command line.
        #[arg(hide = true, trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },
    /// Remove the stored secret.
    Delete {
        /// Remove the stored `tls_key` passphrase instead of the password.
        #[arg(long)]
        tls_key: bool,
        /// Remove the stored SSH tunnel password instead.
        #[arg(long)]
        ssh_password: bool,
        /// Remove the stored SSH `identity_file` passphrase instead.
        #[arg(long)]
        ssh_key_passphrase: bool,
        /// The connection name from dbento.toml.
        conn: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ListFormat {
    Table,
    Json,
}

/// Opens exactly `flag` when given, else the nearest workspace above the working directory,
/// printing a warning for each ssh setting that will not travel to another machine.
fn open_workspace(flag: Option<&Path>) -> anyhow::Result<Workspace> {
    let ws = match flag {
        Some(dir) => Workspace::open(dir)?,
        None => Workspace::discover(std::env::current_dir()?)?,
    };
    if let Some(home) = secret::ssh_home() {
        for line in dbento_core::ssh_warnings(&ws, &home) {
            eprintln!("warning: {line}");
        }
    }
    Ok(ws)
}

/// Which of a connection's secrets `dbento secret` acts on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SecretSlot {
    Password,
    TlsKey,
    SshPassword,
    SshKeyPassphrase,
}

impl SecretSlot {
    /// The slot the flags name, whether they lead or trail the connection name. At most one
    /// flag is allowed, and any other trailing text is refused, since it may be a pasted secret.
    fn from_flags(
        tls_key: bool,
        ssh_password: bool,
        ssh_key_passphrase: bool,
        extra: &[String],
    ) -> anyhow::Result<SecretSlot> {
        let mut named = Vec::new();
        for (set, slot) in [
            (tls_key, SecretSlot::TlsKey),
            (ssh_password, SecretSlot::SshPassword),
            (ssh_key_passphrase, SecretSlot::SshKeyPassphrase),
        ] {
            if set {
                named.push(slot);
            }
        }
        for arg in extra {
            named.push(match arg.as_str() {
                "--tls-key" => SecretSlot::TlsKey,
                "--ssh-password" => SecretSlot::SshPassword,
                "--ssh-key-passphrase" => SecretSlot::SshKeyPassphrase,
                _ => anyhow::bail!(
                    "the secret is read from a prompt or stdin, never from the command line"
                ),
            });
        }
        match named.as_slice() {
            [] => Ok(SecretSlot::Password),
            [slot] => Ok(*slot),
            _ => anyhow::bail!(
                "use at most one of --tls-key, --ssh-password and --ssh-key-passphrase"
            ),
        }
    }
}

/// The SSH home, or an error when neither it nor the user name can be found.
fn require_ssh_home() -> anyhow::Result<dbento_core::SshHome> {
    secret::ssh_home()
        .context("cannot find the home directory or user name for ~/.ssh; set $HOME and $USER")
}

#[derive(Serialize)]
struct ConnectionRow {
    name: String,
    engine: &'static str,
    target: String,
    read_only: bool,
    // Both print null when unset, as the query JSON and the MCP connection list do.
    color: Option<&'static str>,
    folder: Option<String>,
}

#[derive(Serialize)]
struct ConnectionList {
    dbento: u32,
    connections: Vec<ConnectionRow>,
}

fn color_name(c: Color) -> &'static str {
    match c {
        Color::Red => "red",
        Color::Orange => "orange",
        Color::Yellow => "yellow",
        Color::Green => "green",
        Color::Teal => "teal",
        Color::Blue => "blue",
        Color::Purple => "purple",
        Color::Gray => "gray",
    }
}

fn connections(ws: &Workspace, format: ListFormat) -> anyhow::Result<()> {
    let row = |c: &ConnectionConfig| ConnectionRow {
        name: c.name().to_owned(),
        engine: c.engine().name(),
        target: c.display_target(),
        read_only: c.read_only(),
        color: c.effective_color().map(color_name),
        folder: c.folder().map(ToString::to_string),
    };
    // Each listed child follows its parent; an `all` parent's children are unknown until
    // the server is asked, so only the parent is listed.
    let mut rows = Vec::new();
    for c in ws.connections() {
        rows.push(row(c));
        if let Some(Databases::Listed(dbs)) = c.databases() {
            for db in dbs {
                if let Some(child) = ws.connection(&format!("{}/{db}", c.name())) {
                    rows.push(row(&child));
                }
            }
        }
    }
    match format {
        ListFormat::Json => println!(
            "{}",
            serde_json::to_string(&ConnectionList {
                dbento: 1,
                connections: rows
            })?
        ),
        ListFormat::Table => {
            let cells: Vec<Vec<String>> = rows
                .iter()
                .map(|r| {
                    vec![
                        r.name.clone(),
                        r.engine.to_owned(),
                        r.target.clone(),
                        r.read_only.to_string(),
                        r.color.unwrap_or_default().to_owned(),
                        r.folder.clone().unwrap_or_default(),
                    ]
                })
                .collect();
            let headers = ["name", "engine", "target", "read_only", "color", "folder"];
            println!("{}", table::render(&headers, &cells));
        }
    }
    Ok(())
}

async fn run(cli: Cli) -> anyhow::Result<Exit> {
    match cli.command {
        Command::Init => {
            let dir = match &cli.workspace {
                Some(dir) => dir.clone(),
                None => std::env::current_dir()?,
            };
            // `init` writes into an existing directory; `--workspace` may name a new one.
            std::fs::create_dir_all(&dir)?;
            Workspace::init(&dir)?;
            eprintln!("created dbento.toml and queries/ in {}", dir.display());
        }
        Command::Connections { format } => {
            connections(&open_workspace(cli.workspace.as_deref())?, format)?;
        }
        Command::Query { conn, sql, exec } => {
            let ws = open_workspace(cli.workspace.as_deref())?;
            let config = ws.resolve(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
            return exec::execute(&ws, &config, &sql, &exec).await;
        }
        Command::Run { file, conn, exec } => {
            let ws = open_workspace(cli.workspace.as_deref())?;
            let sql = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", file.display()))?;
            let name = match conn {
                Some(name) => name,
                None => run::bound_connection(&ws, &file)?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} is not in a connection's queries folder; pass --conn",
                        file.display()
                    )
                })?,
            };
            let config = ws.resolve(&name).map_err(|e| anyhow::anyhow!("{e}"))?;
            // A file of another language would only reach the server to fail confusingly.
            if let Some(why) = run::language_mismatch(&file, config.engine()) {
                anyhow::bail!(why);
            }
            return exec::execute(&ws, &config, &sql, &exec).await;
        }
        Command::Import {
            source: ImportSource::Datagrip(args),
        } => return import::datagrip(cli.workspace.as_deref(), &args),
        Command::Import {
            source: ImportSource::Dbeaver(args),
        } => return import::dbeaver(cli.workspace.as_deref(), &args),
        Command::Import {
            source: ImportSource::Beekeeper(args),
        } => return import::beekeeper(cli.workspace.as_deref(), &args),
        Command::Mcp(args) => return mcp::serve(cli.workspace.as_deref(), &args).await,
        Command::SelfUpdate(args) => return Ok(self_update::run(&args).await),
        #[cfg(feature = "team")]
        Command::Team { action } => return Ok(team::run(cli.workspace.as_deref(), action)),
        Command::Secret { action } => {
            let ws = open_workspace(cli.workspace.as_deref())?;
            match action {
                SecretAction::Set {
                    tls_key,
                    ssh_password,
                    ssh_key_passphrase,
                    conn,
                    extra,
                } => {
                    let slot =
                        SecretSlot::from_flags(tls_key, ssh_password, ssh_key_passphrase, &extra)?;
                    // Refuse before prompting, so a bad connection never asks for a secret.
                    let (what, prompt) = match slot {
                        SecretSlot::Password => {
                            secret::keychain_connection(&ws, &conn)?;
                            ("secret", format!("Secret for '{conn}': "))
                        }
                        SecretSlot::TlsKey => {
                            secret::key_connection(&ws, &conn)?;
                            ("key passphrase", format!("Key passphrase for '{conn}': "))
                        }
                        SecretSlot::SshPassword => {
                            secret::ssh_connection(&ws, &conn)?;
                            let login = secret::ssh_login(&ws, &conn, &require_ssh_home()?)?;
                            (
                                "SSH password",
                                format!("SSH password for '{conn}' ({login}): "),
                            )
                        }
                        SecretSlot::SshKeyPassphrase => {
                            secret::ssh_connection(&ws, &conn)?;
                            (
                                "SSH key passphrase",
                                format!("SSH key passphrase for '{conn}': "),
                            )
                        }
                    };
                    let secret = if std::io::stdin().is_terminal() {
                        let typed = rpassword::prompt_password(prompt)?;
                        secret::read_secret(&mut typed.as_bytes())?
                    } else {
                        secret::read_secret(&mut std::io::stdin().lock())?
                    };
                    let store = secret::store();
                    match slot {
                        SecretSlot::Password => secret::set(&ws, &conn, &store, &secret)?,
                        SecretSlot::TlsKey => {
                            secret::set_key_passphrase(&ws, &conn, &store, &secret)?;
                        }
                        SecretSlot::SshPassword => {
                            let home = require_ssh_home()?;
                            secret::set_ssh_password(&ws, &conn, &home, &store, &secret)?;
                        }
                        SecretSlot::SshKeyPassphrase => {
                            secret::set_ssh_key_passphrase(&ws, &conn, &store, &secret)?;
                        }
                    }
                    eprintln!("stored the {what} for '{conn}'");
                }
                SecretAction::Delete {
                    tls_key,
                    ssh_password,
                    ssh_key_passphrase,
                    conn,
                } => {
                    let slot =
                        SecretSlot::from_flags(tls_key, ssh_password, ssh_key_passphrase, &[])?;
                    let store = secret::store();
                    let what = match slot {
                        SecretSlot::Password => {
                            secret::delete(&ws, &conn, &store)?;
                            "secret"
                        }
                        SecretSlot::TlsKey => {
                            secret::delete_key_passphrase(&ws, &conn, &store)?;
                            "key passphrase"
                        }
                        SecretSlot::SshPassword => {
                            // The ssh table is checked first, so a direct connection's refusal
                            // does not depend on finding a home.
                            secret::ssh_connection(&ws, &conn)?;
                            let home = require_ssh_home()?;
                            secret::delete_ssh_password(&ws, &conn, &home, &store)?;
                            "SSH password"
                        }
                        SecretSlot::SshKeyPassphrase => {
                            secret::delete_ssh_key_passphrase(&ws, &conn, &store)?;
                            "SSH key passphrase"
                        }
                    };
                    eprintln!("deleted the {what} for '{conn}'");
                }
            }
        }
    }
    Ok(Exit::Ok)
}

/// The first token when it is `secret` or `team`, whose arguments may hold a pasted secret,
/// after only `--workspace <dir>` or `--workspace=<dir>`. Any other flag first means neither.
fn secret_bearing_subcommand(args: impl Iterator<Item = String>) -> Option<&'static str> {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workspace" => {
                args.next();
            }
            a if a.starts_with("--workspace=") => {}
            "secret" => return Some("secret"),
            "team" if cfg!(feature = "team") => return Some("team"),
            _ => return None,
        }
    }
    None
}

fn main() -> ExitCode {
    // clap exits 2 on a usage error; that code is reserved here, so every parse error is 3.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // A clap message quotes the offending argument, which could be a secret.
            if e.use_stderr()
                && let Some(name) = secret_bearing_subcommand(std::env::args().skip(1))
            {
                eprintln!(
                    "error: invalid arguments for `dbento {name}`; see `dbento {name} --help`"
                );
                return ExitCode::from(Exit::Usage as u8);
            }
            let _ = e.print();
            return ExitCode::from(if e.use_stderr() { Exit::Usage as u8 } else { 0 });
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(Exit::Usage as u8);
        }
    };
    let result = runtime.block_on(run(cli));
    // Dropping the runtime would wait for a prompt read that never returns after a Ctrl-C.
    runtime.shutdown_background();
    match result {
        Ok(exit) => ExitCode::from(exit as u8),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(Exit::Usage as u8)
        }
    }
}
