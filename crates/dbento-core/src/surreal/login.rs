//! What connect runs under its timeout: the version floor, signin, who signed in, whether the
//! target exists, and `use`.

use super::decode::Wire;
use super::lex::quote_ident;
use super::rpc::{Param, Rpc, RpcError};
use super::units::UseTarget;
use super::wire::{WireError, field};
use crate::secrets::Secret;
use crate::session::{ConnectError, ReadOnlyEnforcement};
use crate::workspace::{AuthLevel, ConnectionConfig};

/// The first version with `query_stream`.
const FLOOR: Version = (3, 3, 0);
/// The highest major this client was built against.
const KNOWN_MAJOR: u16 = 3;

/// The role a login holds, weakest first so the strongest of several wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    Viewer,
    Editor,
    Owner,
}

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

/// Who `RETURN $token` says signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Identity {
    Anonymous,
    User { name: String, level: AuthLevel },
}

/// The identity in a `$token` value. An `AC` claim is a record user, which no role describes.
pub(crate) fn identity_of(token: &Wire) -> Result<Identity, ConnectError> {
    if matches!(token, Wire::None | Wire::Null) {
        return Ok(Identity::Anonymous);
    }
    if field(token, "AC").is_some_and(|claim| !matches!(claim, Wire::None | Wire::Null)) {
        return Err(ConnectError::Unsupported {
            reason: "record users are not supported: table permissions govern them, so no role \
                     says whether a write would be swallowed"
                .to_owned(),
        });
    }
    let Some(Wire::Text(name)) = field(token, "ID") else {
        return Err(ConnectError::Open {
            message: "the login's token has no ID claim".to_owned(),
        });
    };
    let level = match (field(token, "NS"), field(token, "DB")) {
        (Some(_), Some(_)) => AuthLevel::Database,
        (Some(_), None) => AuthLevel::Namespace,
        _ => AuthLevel::Root,
    };
    Ok(Identity::User {
        name: name.clone(),
        level,
    })
}

/// The strongest role in an `INFO FOR USER .. STRUCTURE` reply, compared case-insensitively.
pub(crate) fn role_of(user: &Wire) -> Option<Role> {
    let Some(Wire::Array(roles)) = field(user, "roles") else {
        return None;
    };
    roles
        .iter()
        .filter_map(|role| match role {
            Wire::Text(role) if role.eq_ignore_ascii_case("owner") => Some(Role::Owner),
            Wire::Text(role) if role.eq_ignore_ascii_case("editor") => Some(Role::Editor),
            Wire::Text(role) if role.eq_ignore_ascii_case("viewer") => Some(Role::Viewer),
            _ => None,
        })
        .max()
}

/// The session's read-only level from its role and the connect argument. An unread role fails a
/// writable session, since the role is what stops a swallowed write being called success.
pub(crate) fn read_only_level(
    role: Result<Role, String>,
    requested: bool,
    target: &str,
) -> Result<Option<ReadOnlyEnforcement>, ConnectError> {
    match role {
        Ok(Role::Viewer) => Ok(Some(ReadOnlyEnforcement::Database)),
        Ok(_) | Err(_) if requested => Ok(Some(ReadOnlyEnforcement::BestEffort)),
        Ok(_) => Ok(None),
        Err(why) => Err(ConnectError::Open {
            message: format!(
                "{target}: cannot read this login's role ({why}), and without it a swallowed \
                 write would look like success; set read_only = true to connect anyway"
            ),
        }),
    }
}

fn wire_message(e: RpcError) -> String {
    match e {
        RpcError::Server(e) => e.message,
        RpcError::Unreadable(why) | RpcError::Closed(why) => why,
    }
}

/// The result of one statement of a `query` reply, or the server's words for its failure.
fn statement(reply: &Wire, at: usize) -> Result<Wire, String> {
    let Wire::Array(statements) = reply else {
        return Err("the reply is not a list of statements".to_owned());
    };
    let Some(one) = statements.get(at) else {
        return Err("the reply has too few statements".to_owned());
    };
    let result = field(one, "result").cloned().unwrap_or(Wire::None);
    match field(one, "status") {
        Some(Wire::Text(status)) if status == "OK" => Ok(result),
        _ => Err(match result {
            Wire::Text(message) => message,
            other => super::cell::surql_text(&other),
        }),
    }
}

async fn query(rpc: &Rpc, text: &str, vars: Option<Param<'_>>) -> Result<Wire, RpcError> {
    let mut params = vec![Param::Text(text)];
    params.extend(vars);
    rpc.call("query", &params, None).await
}

/// Whether the `key` map of an `INFO` reply names `name`.
fn holds(info: &Wire, key: &str, name: &str) -> bool {
    field(info, key).is_some_and(|names| field(names, name).is_some())
}

/// The level the session signed in at, and the namespace and database it holds.
#[derive(Debug, Clone)]
pub(crate) struct Place {
    pub(crate) level: Option<AuthLevel>,
    pub(crate) namespace: String,
    pub(crate) database: String,
}

/// Why a target is not there or cannot be checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Missing {
    Target(String),
    Server(WireError),
    Closed(String),
}

impl From<RpcError> for Missing {
    fn from(e: RpcError) -> Self {
        match e {
            RpcError::Server(e) => Missing::Server(e),
            RpcError::Unreadable(why) => {
                Missing::Target(format!("Dbento cannot read the server's reply: {why}"))
            }
            RpcError::Closed(why) => Missing::Closed(why),
        }
    }
}

/// Checks that `target` exists, since RPC `use` would create it. Below root the namespace
/// check is skipped: `INFO FOR ROOT` is refused there, and `use` refuses another namespace.
/// The text `USE NS` of a root check lasts one call, so the session stays put.
pub(crate) async fn pre_check(rpc: &Rpc, place: &Place, target: &UseTarget) -> Result<(), Missing> {
    let whole_server = matches!(place.level, None | Some(AuthLevel::Root));
    if let Some(namespace) = target.namespace.as_deref().filter(|_| whole_server) {
        let reply = query(rpc, "INFO FOR ROOT", None).await?;
        let info = statement(&reply, 0).map_err(Missing::Target)?;
        if !holds(&info, "namespaces", namespace) {
            return Err(Missing::Target(format!(
                "the namespace {namespace:?} does not exist"
            )));
        }
    }
    let Some(database) = target.database.as_deref() else {
        return Ok(());
    };
    let at = target.namespace.as_deref().unwrap_or(&place.namespace);
    let found = match place.level {
        Some(AuthLevel::Database) => return Ok(()),
        Some(AuthLevel::Namespace) => {
            let reply = query(rpc, "INFO FOR NS", None).await?;
            statement(&reply, 0).map_err(Missing::Target)?
        }
        Some(AuthLevel::Root) | None => {
            let text = format!("USE NS {}; INFO FOR NS", quote_ident(at));
            let reply = query(rpc, &text, None).await?;
            statement(&reply, 1).map_err(Missing::Target)?
        }
    };
    if !holds(&found, "databases", database) {
        return Err(Missing::Target(format!(
            "the database {database:?} does not exist in the namespace {at:?}"
        )));
    }
    Ok(())
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

/// `version`, `signin` unless the connection has no user, identity, role, pre-check, `use`.
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
    if let Some(user) = conn.user() {
        signin(rpc, conn, user, secret, &namespace, &database).await?;
    }
    // The token is read whole: a field of it fails before `use` has picked a namespace.
    let reply = query(rpc, "RETURN $token", None)
        .await
        .map_err(|e| open(wire_message(e)))?;
    let token = statement(&reply, 0).map_err(open)?;
    let identity = identity_of(&token)?;
    let role = match &identity {
        Identity::Anonymous => anonymous(rpc).await?,
        Identity::User { name, level } => user_role(rpc, name, *level).await,
    };
    let viewer = matches!(role, Ok(Role::Viewer));
    let read_only = read_only_level(role, requested, &target)?;
    let place = Place {
        level: match identity {
            Identity::Anonymous => None,
            Identity::User { level, .. } => Some(level),
        },
        namespace,
        database,
    };
    let wanted = UseTarget {
        namespace: Some(place.namespace.clone()),
        database: Some(place.database.clone()),
    };
    pre_check(rpc, &place, &wanted).await.map_err(|e| match e {
        Missing::Target(message) | Missing::Closed(message) => open(message),
        Missing::Server(e) => open(e.message),
    })?;
    rpc.call(
        "use",
        &[Param::Text(&place.namespace), Param::Text(&place.database)],
        None,
    )
    .await
    .map_err(|e| open(wire_message(e)))?;
    Ok(Login {
        version,
        warning,
        read_only,
        viewer,
        place,
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
) -> Result<(), ConnectError> {
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
        .map(drop)
        .map_err(|e| ConnectError::Open {
            message: format!(
                "{target}: {}; check the password, and that the user exists at auth_level {} \
                 in namespace {namespace:?}, database {database:?}",
                wire_message(e),
                level.name()
            ),
        })
}

// `INFO FOR ROOT` answers on an unauthenticated server, which counts as OWNER, and is refused
// to a guest, whom table permissions govern.
async fn anonymous(rpc: &Rpc) -> Result<Result<Role, String>, ConnectError> {
    let reply = query(rpc, "INFO FOR ROOT", None).await;
    match reply.map_err(wire_message).and_then(|r| statement(&r, 0)) {
        Ok(_) => Ok(Ok(Role::Owner)),
        Err(_) => Err(ConnectError::Unsupported {
            reason: "guest access is not supported: table permissions govern a guest, so no \
                     role says whether a write would be swallowed"
                .to_owned(),
        }),
    }
}

async fn user_role(rpc: &Rpc, name: &str, level: AuthLevel) -> Result<Role, String> {
    let on = match level {
        AuthLevel::Root => "ROOT",
        AuthLevel::Namespace => "NAMESPACE",
        AuthLevel::Database => "DATABASE",
    };
    let text = format!("INFO FOR USER $x ON {on} STRUCTURE");
    let vars = Param::Object(vec![("x", Param::Text(name))]);
    let reply = query(rpc, &text, Some(vars)).await.map_err(wire_message)?;
    let user = statement(&reply, 0)?;
    role_of(&user).ok_or_else(|| "the reply names no known role".to_owned())
}

#[cfg(test)]
mod tests;
