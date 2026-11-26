//! The secret pin: the canonical text of the fields that choose a connection's server and
//! account, so a saved password can be tied to the target it was saved for.

use crate::workspace::{AuthLevel, ConnectionConfig, Engine};
use serde_json::Value;

/// The canonical JSON text of the fields that pick a connection's server and account, with
/// defaults filled and host spellings of one socket merged. Two connections with equal pins
/// send a saved password to the same place.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretPin(String);

impl SecretPin {
    /// The pin text, a JSON object with a fixed key order.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ConnectionConfig {
    /// The pin of the target this connection's password is sent to.
    pub fn secret_pin(&self) -> SecretPin {
        // Keys are written in a fixed order by hand, so the text never depends on whether
        // serde_json's `preserve_order` happens to be on in the build.
        let engine = self.engine();
        let mut fields = vec![("v", json(1)), ("engine", json(engine.name()))];
        let server = |fields: &mut Vec<(&str, String)>, default_port: u16| {
            fields.push(("user", opt(self.user())));
            fields.push(("host", opt(self.host().map(normal_host))));
            fields.push(("port", json(self.port().unwrap_or(default_port))));
        };
        match engine {
            Engine::Postgres => server(&mut fields, 5432),
            Engine::Mysql => server(&mut fields, 3306),
            Engine::Mssql => server(&mut fields, 1433),
            Engine::Redis => server(&mut fields, 6379),
            Engine::Surrealdb => {
                server(&mut fields, 8000);
                // Each level signs in to a different scope, so it pins only what it names.
                let (ns, db) = match self.auth_level() {
                    Some(AuthLevel::Database) => (true, true),
                    Some(AuthLevel::Namespace) => (true, false),
                    Some(AuthLevel::Root) | None => (false, false),
                };
                if ns {
                    fields.push(("namespace", opt(self.namespace())));
                }
                if db {
                    fields.push(("database", opt(self.database())));
                }
            }
            Engine::Mongodb => fields.push(("uri", opt(self.uri().map(str::trim)))),
            Engine::Dynamodb | Engine::S3 => {
                fields.push(("access_key_id", opt(self.access_key_id())));
                fields.push(("region", opt(self.region())));
                let endpoint = self.endpoint().map(|e| {
                    let e = e.trim();
                    e.strip_suffix('/').unwrap_or(e)
                });
                fields.push(("endpoint", opt(endpoint)));
            }
            Engine::Sqlite | Engine::Duckdb => {
                let file = self.file_path().map(|p| p.to_string_lossy().into_owned());
                fields.push(("file_path", opt(file)));
            }
        }
        if let Some(ssh) = self.ssh() {
            let via = render(&[
                ("host", json(normal_host(ssh.host()))),
                ("port", opt(ssh.port())),
            ]);
            fields.push(("via", via));
        }
        SecretPin(render(&fields))
    }
}

fn json(v: impl Into<Value>) -> String {
    v.into().to_string()
}

fn opt<T: Into<Value>>(v: Option<T>) -> String {
    v.map_or_else(|| Value::Null.to_string(), json)
}

// Keys are fixed ASCII literals, so only the values need JSON escaping.
fn render(fields: &[(&str, String)]) -> String {
    let body: Vec<String> = fields.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
    format!("{{{}}}", body.join(","))
}

/// Merges only spellings of one socket: no DNS, IDNA or percent decoding.
fn normal_host(raw: &str) -> String {
    if raw.starts_with('/') {
        return raw.to_owned();
    }
    let h = raw.trim();
    let h = h.strip_suffix('.').unwrap_or(h);
    let h = h
        .strip_prefix('[')
        .and_then(|x| x.strip_suffix(']'))
        .unwrap_or(h);
    match h.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.to_string(),
        Err(_) if h.is_ascii() => h.to_ascii_lowercase(),
        Err(_) => h.to_owned(),
    }
}
