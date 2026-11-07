//! The Redis catalog: the DB indexes with their key counts, and the server's `COMMAND DOCS` for
//! completion. The doc types exist in every build; reading them needs the `redis` feature.

/// One command's documentation, as the server's `COMMAND DOCS` gives it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandDoc {
    /// The lowercase name; a subcommand is `container|sub`, such as `config|get`.
    pub name: String,
    /// One line on what the command does.
    pub summary: String,
    /// The arguments in order.
    pub arguments: Vec<ArgDoc>,
    /// The subcommands, sorted by name.
    pub subcommands: Vec<CommandDoc>,
}

/// One argument of a command, or one choice or part of a compound argument.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgDoc {
    /// The argument's name, such as `key` or `condition`.
    pub name: String,
    /// The server's `type`, such as `key`, `string`, `pure-token`, `oneof` or `block`.
    pub kind: String,
    /// The word typed before the value, such as `NX` or `COUNT`.
    pub token: Option<String>,
    /// The argument may be left out.
    pub optional: bool,
    /// The argument may repeat.
    pub multiple: bool,
    /// The choices of a `oneof` or the parts of a `block`.
    pub arguments: Vec<ArgDoc>,
}

#[cfg(all(test, feature = "redis"))]
mod tests;

#[cfg(all(test, feature = "redis"))]
use self::server::{command_docs_of, fold_counts};

#[cfg(feature = "redis")]
mod server {
    use ::redis::{Pipeline, Value};

    use super::{ArgDoc, CommandDoc};
    use crate::introspect::{IntrospectError, SchemaInfo};
    use crate::redis::browse::{KeyBrowseError, server_error};
    use crate::redis::driver::{IN_MULTI, RedisSession, Shared, Unbracketable, text_of};
    use crate::session::ExecuteError;

    impl RedisSession {
        /// The server's `COMMAND DOCS`, sorted by name; refused inside `MULTI`.
        pub(crate) async fn command_docs(&self) -> Result<Vec<CommandDoc>, KeyBrowseError> {
            self.shared.command_docs().await
        }
    }

    /// What a server answers when `CONFIG GET databases` is refused or unreadable.
    const DEFAULT_DATABASES: u32 = 16;

    impl Shared {
        /// Every DB index with its key count: the count of DBs alone, then one pipeline of
        /// `SELECT i`, `DBSIZE` for each and a `SELECT` back to the session's DB.
        pub(in crate::redis) async fn schemas(&self) -> Result<Vec<SchemaInfo>, IntrospectError> {
            let back = match self.bracket_db() {
                Ok(db) => db,
                Err(Unbracketable::InMulti) => {
                    return Err(IntrospectError::Unsupported {
                        what: "the DB list inside MULTI",
                    });
                }
                Err(unknown) => {
                    return Err(IntrospectError::Execute(ExecuteError::Unsupported {
                        reason: unknown.reason().to_owned(),
                    }));
                }
            };
            let reply = self
                .one(::redis::cmd("CONFIG").arg("GET").arg("databases"))
                .await?;
            let databases = database_count(&reply).unwrap_or(DEFAULT_DATABASES);
            let mut pipe = Pipeline::with_capacity(databases as usize * 2 + 1);
            for db in 0..databases {
                pipe.cmd("SELECT").arg(db).cmd("DBSIZE");
            }
            pipe.cmd("SELECT").arg(back);
            let replies = self.many(&pipe).await?;
            // A failed SELECT back leaves the session on another DB, so later brackets refuse.
            match replies.last() {
                Some(Value::ServerError(e)) => {
                    self.forget_db();
                    let code = e.code().to_owned();
                    let message = e.details().unwrap_or(&code).to_owned();
                    return Err(IntrospectError::Execute(ExecuteError::database(
                        code, message,
                    )));
                }
                None => {
                    self.forget_db();
                    return Err(IntrospectError::Execute(ExecuteError::database(
                        "ERR",
                        "no reply to the SELECT back",
                    )));
                }
                Some(_) => {}
            }
            let counts = fold_counts(
                replies
                    .get(..replies.len().saturating_sub(1))
                    .unwrap_or(&[]),
            );
            Ok(counts
                .into_iter()
                .map(|(db, keys)| SchemaInfo::new(format!("db{db}")).with_key_count(keys))
                .collect())
        }

        /// The server's `COMMAND DOCS`, sorted by name.
        pub(in crate::redis) async fn command_docs(
            &self,
        ) -> Result<Vec<CommandDoc>, KeyBrowseError> {
            if self.in_multi() {
                return Err(KeyBrowseError::Unsupported {
                    reason: IN_MULTI.to_owned(),
                });
            }
            let reply = server_error(self.one(::redis::cmd("COMMAND").arg("DOCS")).await?)?;
            command_docs_of(&reply).map_err(|message| KeyBrowseError::Database {
                code: "ERR".to_owned(),
                message,
            })
        }
    }

    /// The number in a `CONFIG GET databases` reply; `None` for an error or anything else.
    fn database_count(reply: &Value) -> Option<u32> {
        let Value::Map(pairs) = reply else {
            return None;
        };
        let (_, count) = pairs.first()?;
        text_of(count)?.parse().ok()
    }

    /// `(index, DBSIZE)` from the replies to SELECT i, DBSIZE for i in 0..m; stops at the first
    /// failed SELECT.
    pub(crate) fn fold_counts(replies: &[Value]) -> Vec<(u32, u64)> {
        replies
            .as_chunks::<2>()
            .0
            .iter()
            .zip(0..)
            .map_while(|(pair, db)| match pair {
                [Value::ServerError(_), _] => None,
                [_, Value::Int(keys)] => Some((db, u64::try_from(*keys).ok()?)),
                _ => None,
            })
            .collect()
    }

    /// The docs in a `COMMAND DOCS` reply, a map of name to doc, sorted by name.
    pub(crate) fn command_docs_of(reply: &Value) -> Result<Vec<CommandDoc>, String> {
        let mut docs = map(reply, "COMMAND DOCS")?
            .iter()
            .map(|(name, doc)| command_doc(name, doc))
            .collect::<Result<Vec<_>, _>>()?;
        docs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(docs)
    }

    fn command_doc(name: &Value, doc: &Value) -> Result<CommandDoc, String> {
        let name = text_of(name).ok_or("a command name that is not text")?;
        let mut out = CommandDoc {
            summary: String::new(),
            arguments: Vec::new(),
            subcommands: Vec::new(),
            name,
        };
        for (field, value) in map(doc, &out.name)? {
            match text_of(field).as_deref() {
                Some("summary") => out.summary = text_of(value).unwrap_or_default(),
                Some("arguments") => out.arguments = arg_docs(value, &out.name)?,
                Some("subcommands") => out.subcommands = command_docs_of(value)?,
                _ => {}
            }
        }
        Ok(out)
    }

    fn arg_docs(list: &Value, owner: &str) -> Result<Vec<ArgDoc>, String> {
        let (Value::Array(items) | Value::Set(items)) = list else {
            return Err(format!("{owner}: arguments that are not a list"));
        };
        items.iter().map(|item| arg_doc(item, owner)).collect()
    }

    fn arg_doc(item: &Value, owner: &str) -> Result<ArgDoc, String> {
        let mut arg = ArgDoc {
            name: String::new(),
            kind: String::new(),
            token: None,
            optional: false,
            multiple: false,
            arguments: Vec::new(),
        };
        for (field, value) in map(item, owner)? {
            match text_of(field).as_deref() {
                Some("name") => arg.name = text_of(value).unwrap_or_default(),
                Some("type") => arg.kind = text_of(value).unwrap_or_default(),
                Some("token") => arg.token = text_of(value),
                Some("arguments") => arg.arguments = arg_docs(value, owner)?,
                Some("flags") => {
                    if let Value::Array(flags) | Value::Set(flags) = value {
                        for flag in flags {
                            match text_of(flag).as_deref() {
                                Some("optional") => arg.optional = true,
                                Some("multiple") => arg.multiple = true,
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(arg)
    }

    fn map<'a>(value: &'a Value, what: &str) -> Result<&'a [(Value, Value)], String> {
        match value {
            Value::Map(pairs) => Ok(pairs),
            _ => Err(format!("{what}: a reply that is not a map")),
        }
    }
}
