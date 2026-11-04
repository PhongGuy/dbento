//! What connect runs under its timeout: the version floor, signin, and `use`.

use super::decode::Wire;
use super::rpc::{Param, Rpc, RpcError};
use crate::secrets::Secret;
use crate::session::{ConnectError, ReadOnlyEnforcement};
use crate::workspace::{AuthLevel, ConnectionConfig};

/// The first version with `query_stream`.
const FLOOR: Version = (3, 3, 0);
/// The highest major this client was built against.
const KNOWN_MAJOR: u16 = 3;

/// Major, minor and patch.
pub(crate) type Version = (u16, u16, u16);

/// Major, minor and patch of `surrealdb-3.3.5`, with any `+build` or `-beta.1` after them.
pub(crate) fn server_version(text: &str) -> Option<Version> {
    let rest = text.strip_prefix("surrealdb-")?;
    let core = rest.split(['+', '-']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u16>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// Refuses a server below the floor; a higher major or a prerelease connects with the warning
/// returned.
pub(crate) fn version_check(
    text: &str,
    target: &str,
) -> Result<(Version, Option<String>), ConnectError> {
    let Some(version) = server_version(text) else {
        return Err(ConnectError::Open {
            message: format!("{target}: cannot read the server version {text:?}"),
        });
    };
    if version < FLOOR {
        return Err(ConnectError::Open {
            message: format!(
                "{target}: SurrealDB {}.{}.{} is below the minimum {}.{}.{}, where streaming \
                 starts",
                version.0, version.1, version.2, FLOOR.0, FLOOR.1, FLOOR.2
            ),
        });
    }
    let name = text.strip_prefix("surrealdb-").unwrap_or(text);
    let warning = (version.0 > KNOWN_MAJOR || name.contains('-')).then(|| {
        format!(
            "SurrealDB {name} is newer than the versions Dbento has been tested against; its \
             behaviour may differ"
        )
    });
    Ok((version, warning))
}

fn wire_message(e: RpcError) -> String {
    match e {
        RpcError::Server(e) => e.message,
        RpcError::Closed(why) => why,
    }
}

/// The level the session signed in at, and the namespace and database it holds.
#[derive(Debug, Clone)]
pub(crate) struct Place {
    pub(crate) level: Option<AuthLevel>,
    pub(crate) namespace: String,
    pub(crate) database: String,
}

/// A logged-in socket.
#[derive(Debug)]
pub(crate) struct Login {
    pub(crate) version: Version,
    pub(crate) warning: Option<String>,
    pub(crate) read_only: Option<ReadOnlyEnforcement>,
    pub(crate) viewer: bool,
    pub(crate) place: Place,
}

/// `version`, `signin` unless the connection has no user, then `use`.
pub(crate) async fn login(
    rpc: &Rpc,
    conn: &ConnectionConfig,
    secret: Option<&Secret>,
    requested: bool,
) -> Result<Login, ConnectError> {
    let target = conn.display_target();
    let open = |message: String| ConnectError::Open {
        message: format!("{target}: {message}"),
    };
    let reply = rpc
        .call("version", &[], None)
        .await
        .map_err(|e| open(wire_message(e)))?;
    let Wire::Text(version) = reply else {
        return Err(open("the version reply is not text".to_owned()));
    };
    let (version, warning) = version_check(&version, &target)?;
    let namespace = conn.namespace().unwrap_or_default().to_owned();
    let database = conn.database().unwrap_or_default().to_owned();
    let level = match conn.user() {
        Some(user) => Some(signin(rpc, conn, user, secret, &namespace, &database).await?),
        None => None,
    };
    rpc.call(
        "use",
        &[Param::Text(&namespace), Param::Text(&database)],
        None,
    )
    .await
    .map_err(|e| open(wire_message(e)))?;
    Ok(Login {
        version,
        warning,
        read_only: requested.then_some(ReadOnlyEnforcement::BestEffort),
        viewer: false,
        place: Place {
            level,
            namespace,
            database,
        },
    })
}

// A signin carries `NS` below root and `DB` at database level. Its failure names what to
// check, never the secret.
async fn signin(
    rpc: &Rpc,
    conn: &ConnectionConfig,
    user: &str,
    secret: Option<&Secret>,
    namespace: &str,
    database: &str,
) -> Result<AuthLevel, ConnectError> {
    let level = conn.auth_level().unwrap_or(AuthLevel::Root);
    let mut fields = Vec::new();
    if level != AuthLevel::Root {
        fields.push(("NS", Param::Text(namespace)));
    }
    if level == AuthLevel::Database {
        fields.push(("DB", Param::Text(database)));
    }
    fields.push(("user", Param::Text(user)));
    fields.push((
        "pass",
        Param::Text(secret.map_or("", |secret| secret.expose())),
    ));
    let target = conn.display_target();
    rpc.call("signin", &[Param::Object(fields)], None)
        .await
        .map(|_| level)
        .map_err(|e| ConnectError::Open {
            message: format!(
                "{target}: {}; check the password, and that the user exists at auth_level {} \
                 in namespace {namespace:?}, database {database:?}",
                wire_message(e),
                level.name()
            ),
        })
}

#[cfg(test)]
mod tests;
