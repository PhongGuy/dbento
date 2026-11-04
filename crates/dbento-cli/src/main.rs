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
mod table;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use dbento_core::{Color, Workspace};
use serde::Serialize;

/// How a run ended; the highest code wins when several things went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Exit {
    Ok = 0,
    Failed = 1,
    Refused = 2,
    Usage = 3,
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
    /// Run a SQL file; its connection is its `queries/<conn>/` folder unless `--conn` is given.
    Run {
        /// The SQL file.
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
        /// The connection name from dbento.toml.
        conn: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ListFormat {
    Table,
    Json,
}

/// Opens exactly `flag` when given, else the nearest workspace above the working directory.
fn open_workspace(flag: Option<&Path>) -> anyhow::Result<Workspace> {
    Ok(match flag {
        Some(dir) => Workspace::open(dir)?,
        None => Workspace::discover(std::env::current_dir()?)?,
    })
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
        _ => "unknown",
    }
}

fn connections(ws: &Workspace, format: ListFormat) -> anyhow::Result<()> {
    let rows: Vec<ConnectionRow> = ws
        .connections()
        .iter()
        .map(|c| ConnectionRow {
            name: c.name().to_owned(),
            engine: c.engine().name(),
            target: c.display_target(),
            read_only: c.read_only(),
            color: c.effective_color().map(color_name),
            folder: c.folder().map(ToString::to_string),
        })
        .collect();
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
            let config = ws
                .connection(&conn)
                .ok_or_else(|| anyhow::anyhow!("no connection named {conn:?} in dbento.toml"))?;
            return exec::execute(&ws, config, &sql, &exec).await;
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
            let config = ws
                .connection(&name)
                .ok_or_else(|| anyhow::anyhow!("no connection named {name:?} in dbento.toml"))?;
            return exec::execute(&ws, config, &sql, &exec).await;
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
        Command::Secret { action } => {
            let ws = open_workspace(cli.workspace.as_deref())?;
            match action {
                SecretAction::Set {
                    tls_key,
                    conn,
                    extra,
                } => {
                    // The flag may also follow the name, where the trailing catch-all takes it.
                    let tls_key = tls_key || extra == ["--tls-key"];
                    if !extra.is_empty() && extra != ["--tls-key"] {
                        anyhow::bail!(
                            "the secret is read from a prompt or stdin, never from the command line"
                        );
                    }
                    // Refuse before prompting, so a bad connection never asks for a secret.
                    let (what, label) = if tls_key {
                        secret::key_connection(&ws, &conn)?;
                        ("key passphrase", "Key passphrase")
                    } else {
                        secret::keychain_connection(&ws, &conn)?;
                        ("secret", "Secret")
                    };
                    let secret = if std::io::stdin().is_terminal() {
                        let prompt = format!("{label} for '{conn}': ");
                        let typed = rpassword::prompt_password(prompt)?;
                        secret::read_secret(&mut typed.as_bytes())?
                    } else {
                        secret::read_secret(&mut std::io::stdin().lock())?
                    };
                    if tls_key {
                        secret::set_key_passphrase(&ws, &conn, &secret::store(), &secret)?;
                    } else {
                        secret::set(&ws, &conn, &secret::store(), &secret)?;
                    }
                    eprintln!("stored the {what} for '{conn}'");
                }
                SecretAction::Delete { tls_key, conn } => {
                    if tls_key {
                        secret::delete_key_passphrase(&ws, &conn, &secret::store())?;
                        eprintln!("deleted the key passphrase for '{conn}'");
                    } else {
                        secret::delete(&ws, &conn, &secret::store())?;
                        eprintln!("deleted the secret for '{conn}'");
                    }
                }
            }
        }
    }
    Ok(Exit::Ok)
}

/// Whether the first token is `secret`, after only `--workspace <dir>` or `--workspace=<dir>`.
/// Any other flag first means the invocation is not clearly a secret one.
fn subcommand_is_secret(args: impl Iterator<Item = String>) -> bool {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workspace" => {
                args.next();
            }
            a if a.starts_with("--workspace=") => {}
            a if a.starts_with('-') => return false,
            a => return a == "secret",
        }
    }
    false
}

fn main() -> ExitCode {
    // clap exits 2 on a usage error; that code is reserved here, so every parse error is 3.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // A clap message quotes the offending argument, which could be a secret.
            if e.use_stderr() && subcommand_is_secret(std::env::args().skip(1)) {
                eprintln!(
                    "error: invalid arguments for `dbento secret`; see `dbento secret --help`"
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
