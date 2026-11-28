//! Turns `ssh -G` output into a [`ResolvedSsh`]. A second `-G` run with `-F none` is the
//! baseline: a key whose values match it was left at OpenSSH's default, so only keys that
//! differ count as set by the user's config.

use super::resolve::{DEFAULT_CONNECT_TIMEOUT, DEFAULT_KEYS, ResolvedSsh, SshHome, expand_key};
use crate::ConnectionConfig;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Why `ssh -G` output cannot become a [`ResolvedSsh`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refusal {
    /// A construct dbento cannot honour: "ProxyJump", "ProxyCommand", "HostKeyAlias",
    /// "KnownHostsCommand", "UserKnownHostsFile", "GlobalKnownHostsFile", "RevokedHostKeys",
    /// "CanonicalizeHostname" or "a % token".
    Unsupported(&'static str),
    /// The output lacks or garbles a needed key; the message names the key.
    Malformed(String),
}

/// Keys that change routing or host-key trust, which dbento would silently ignore; any
/// difference from the baseline refuses, in this order.
const REFUSED: [(&str, &str); 8] = [
    ("proxyjump", "ProxyJump"),
    ("proxycommand", "ProxyCommand"),
    ("hostkeyalias", "HostKeyAlias"),
    ("knownhostscommand", "KnownHostsCommand"),
    ("userknownhostsfile", "UserKnownHostsFile"),
    ("globalknownhostsfile", "GlobalKnownHostsFile"),
    ("revokedhostkeys", "RevokedHostKeys"),
    ("canonicalizehostname", "CanonicalizeHostname"),
];

/// `out` is `ssh -G` for the alias, `baseline` the same call with `-F none`.
pub(super) fn from_ssh_g(
    out: &str,
    baseline: &str,
    conn: &ConnectionConfig,
    ws_dir: &Path,
    home: &SshHome,
) -> Result<ResolvedSsh, Refusal> {
    let ssh = conn
        .ssh()
        .ok_or_else(|| Refusal::Malformed("no ssh table".to_owned()))?;
    let options = parse(out);
    let base = parse(baseline);

    // A broken ssh (empty or truncated output) must read as broken, not as a refused
    // construct, so the needed keys are checked before the baseline diff.
    let host_name = single(&options, "hostname")?;
    if host_name.chars().any(char::is_whitespace) {
        return Err(garbled("hostname", host_name));
    }
    let user = single(&options, "user")?;
    let port_text = single(&options, "port")?;
    let port: u16 = port_text.parse().map_err(|_| garbled("port", port_text))?;
    let timeout = single(&options, "connecttimeout")?;
    let connect_timeout = match timeout {
        "none" => DEFAULT_CONNECT_TIMEOUT,
        secs => Duration::from_secs(secs.parse().map_err(|_| garbled("connecttimeout", secs))?),
    };

    if let Some((_, construct)) = REFUSED
        .iter()
        .find(|(key, _)| options.get(key) != base.get(key))
    {
        return Err(Refusal::Unsupported(construct));
    }

    // dbento.toml's key replaces the whole list; an unchanged list means OpenSSH's defaults,
    // which dbento narrows to the keys that exist.
    let tokens = Tokens {
        home,
        host_name,
        user,
        port,
    };
    let identity_files = match ssh.identity_file() {
        Some(written) => vec![expand_key(written, home, ws_dir)],
        None if options.get("identityfile") == base.get("identityfile") => DEFAULT_KEYS
            .iter()
            .map(|name| home.dir.join(".ssh").join(name))
            .filter(|path| path.is_file())
            .collect(),
        None => options
            .get("identityfile")
            .into_iter()
            .flatten()
            .map(|written| tokens.key_path(written))
            .collect::<Result<_, _>>()?,
    };
    Ok(ResolvedSsh {
        alias: ssh.host().to_owned(),
        host_name: host_name.to_owned(),
        port,
        user: user.to_owned(),
        identity_files,
        auth: ssh.auth(),
        keepalive: ssh.keepalive(),
        connect_timeout,
    })
}

/// Each line's first space-separated word as the key, the rest of the line unsplit as one
/// value; a repeated key keeps every value in order.
fn parse(text: &str) -> HashMap<&str, Vec<&str>> {
    let mut options: HashMap<&str, Vec<&str>> = HashMap::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        options.entry(key).or_default().push(value);
    }
    options
}

/// The first value of `key`, refusing when it is absent or empty.
fn single<'a>(options: &HashMap<&str, Vec<&'a str>>, key: &str) -> Result<&'a str, Refusal> {
    options
        .get(key)
        .and_then(|values| values.first().copied())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Refusal::Malformed(format!("ssh -G printed no {key}")))
}

fn garbled(key: &str, value: &str) -> Refusal {
    Refusal::Malformed(format!("ssh -G printed an unreadable {key} `{value}`"))
}

/// The values the `%` tokens of an identityfile line expand to.
struct Tokens<'a> {
    home: &'a SshHome,
    host_name: &'a str,
    user: &'a str,
    port: u16,
}

impl Tokens<'_> {
    /// An identityfile line as a path: `~` and `~/` against the home, the known `%` tokens
    /// expanded, and a relative result joined to the home (ssh -G's working directory).
    /// `~user` and `${VAR}` stay literal.
    fn key_path(&self, written: &str) -> Result<PathBuf, Refusal> {
        let home = &self.home.dir;
        if written == "~" {
            return Ok(home.clone());
        }
        let path = match written.strip_prefix("~/") {
            Some(rest) => home.join(self.expand(rest)?),
            None => PathBuf::from(self.expand(written)?),
        };
        Ok(if path.is_relative() {
            home.join(path)
        } else {
            path
        })
    }

    fn expand(&self, text: &str) -> Result<String, Refusal> {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('%') => out.push('%'),
                Some('d') => out.push_str(&self.home.dir.to_string_lossy()),
                Some('u') => out.push_str(&self.home.user),
                Some('h') => out.push_str(self.host_name),
                Some('r') => out.push_str(self.user),
                Some('p') => out.push_str(&self.port.to_string()),
                Some(_) | None => return Err(Refusal::Unsupported("a % token")),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::{Refusal, from_ssh_g};
    use crate::{ConnectionConfig, SshAuth, SshHome, Workspace};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const BASE: &str = "user tester\nhostname jump\nport 22\ncanonicalizehostname false\nidentityfile ~/.ssh/id_rsa\nidentityfile ~/.ssh/id_ed25519\nglobalknownhostsfile /etc/ssh/ssh_known_hosts /etc/ssh/ssh_known_hosts2\nuserknownhostsfile /home/t/.ssh/known_hosts /home/t/.ssh/known_hosts2\nrevokedhostkeys none\nconnecttimeout none\n";

    fn conn(extra: &str) -> ConnectionConfig {
        let text = format!(
            "dbento = 1\n[connections.c]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\n[connections.c.ssh]\nhost = \"jump\"\n{extra}"
        );
        Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), &text)
            .unwrap()
            .declared("c")
            .unwrap()
            .clone()
    }

    /// `base` with every `key` line replaced by one line per value; no values removes the key.
    fn set(base: &str, key: &str, values: &[&str]) -> String {
        let mut out: String = base
            .lines()
            .filter(|l| l.split(' ').next() != Some(key))
            .map(|l| format!("{l}\n"))
            .collect();
        for v in values {
            out.push_str(&format!("{key} {v}\n"));
        }
        out
    }

    fn home(keys: &[&str]) -> (tempfile::TempDir, SshHome) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ssh")).unwrap();
        for k in keys {
            std::fs::write(dir.path().join(".ssh").join(k), "").unwrap();
        }
        let home = SshHome::new(dir.path(), "tester");
        (dir, home)
    }

    fn moved() -> String {
        set(
            &set(&set(BASE, "hostname", &["10.0.0.5"]), "port", &["2200"]),
            "user",
            &["ops"],
        )
    }

    #[test]
    fn fields_come_from_ssh_g_and_an_unchanged_identity_list_means_the_default_keys() {
        let (d, home) = home(&["id_rsa", "id_ed25519", "unrelated"]);
        let r = from_ssh_g(&moved(), BASE, &conn(""), Path::new("/w"), &home).unwrap();
        assert_eq!(r.alias, "jump", "the alias as written");
        assert_eq!(
            r.login(),
            "ops@10.0.0.5:2200",
            "hostname, port and user from ssh -G"
        );
        let ssh = d.path().join(".ssh");
        assert_eq!(
            r.identity_files,
            vec![ssh.join("id_ed25519"), ssh.join("id_rsa")],
            "present default keys in Dbento's order"
        );
        assert_eq!(
            r.connect_timeout,
            Duration::from_secs(15),
            "connecttimeout none is 15 s"
        );
        assert_eq!(
            (r.auth, r.keepalive),
            (None, true),
            "auth and keepalive from dbento.toml"
        );
    }

    #[test]
    fn connect_timeout_and_the_dbento_toml_overlay_apply() {
        let (_d, home) = home(&[]);
        let out = set(
            &set(BASE, "connecttimeout", &["7"]),
            "identityfile",
            &["~/.ssh/%C"],
        );
        let c = conn("auth = \"key\"\nidentity_file = \"keys/id\"\nkeepalive = false\n");
        let r = from_ssh_g(&out, BASE, &c, Path::new("/w"), &home).unwrap();
        assert_eq!(
            r.connect_timeout,
            Duration::from_secs(7),
            "ConnectTimeout in seconds"
        );
        assert_eq!(
            r.identity_files,
            vec![Path::new("/w").join("keys/id")],
            "dbento.toml replaces the list, so its %C is never read"
        );
        assert_eq!(
            (r.auth, r.keepalive),
            (Some(SshAuth::Key), false),
            "auth and keepalive as written"
        );
    }

    #[test]
    fn a_trust_or_routing_key_that_differs_from_the_baseline_refuses() {
        let (_d, home) = home(&[]);
        for (key, value, construct) in [
            ("proxyjump", "hop", "ProxyJump"),
            ("proxycommand", "ssh -W %h:%p hop", "ProxyCommand"),
            ("hostkeyalias", "other", "HostKeyAlias"),
            ("knownhostscommand", "/usr/bin/kh %H", "KnownHostsCommand"),
            ("userknownhostsfile", "/dev/null", "UserKnownHostsFile"),
            ("globalknownhostsfile", "/dev/null", "GlobalKnownHostsFile"),
            ("revokedhostkeys", "/etc/ssh/revoked", "RevokedHostKeys"),
            ("canonicalizehostname", "yes", "CanonicalizeHostname"),
        ] {
            let out = set(BASE, key, &[value]);
            let got = from_ssh_g(&out, BASE, &conn(""), Path::new("/w"), &home).map(|r| r.login());
            assert_eq!(got, Err(Refusal::Unsupported(construct)), "{key}");
        }
    }

    #[test]
    fn other_keys_may_differ_from_the_baseline() {
        let (_d, home) = home(&[]);
        let out =
            format!("{BASE}stricthostkeychecking no\nidentitiesonly yes\nsomefutureoption 1\n");
        let r = from_ssh_g(&out, BASE, &conn(""), Path::new("/w"), &home);
        assert!(r.is_ok(), "keys outside the refuse list are ignored: {r:?}");
    }

    #[test]
    fn a_changed_identity_list_is_taken_with_its_tokens_expanded() {
        let (d, home) = home(&["id_ed25519"]);
        let out = set(
            &moved(),
            "identityfile",
            &[
                "~/.ssh/jump_key",
                "%d/.ssh/%u_%r_%h_%p",
                "~/.ssh/100%%",
                "~",
                "${HOME}/k",
            ],
        );
        let r = from_ssh_g(&out, BASE, &conn(""), Path::new("/w"), &home).unwrap();
        let h = d.path();
        assert_eq!(
            r.identity_files,
            vec![
                h.join(".ssh/jump_key"),
                h.join(".ssh/tester_ops_10.0.0.5_2200"),
                h.join(".ssh/100%"),
                h.to_path_buf(),
                h.join("${HOME}/k")
            ],
            "~, %d, %u, %r, %h, %p and %% expand; ${{VAR}} stays literal; relative joins the home"
        );
    }

    #[test]
    fn an_unknown_percent_token_refuses() {
        let (_d, home) = home(&[]);
        for token in ["%C", "%i", "%L", "%n", "%k", "%"] {
            let file = format!("~/.ssh/key{token}");
            let out = set(BASE, "identityfile", &[file.as_str()]);
            let got = from_ssh_g(&out, BASE, &conn(""), Path::new("/w"), &home).map(|r| r.login());
            assert_eq!(got, Err(Refusal::Unsupported("a % token")), "{token}");
        }
    }

    #[test]
    fn output_missing_or_garbling_a_needed_key_is_malformed() {
        let (_d, home) = home(&[]);
        for (out, key) in [
            (String::new(), "hostname"),
            (set(BASE, "hostname", &[]), "hostname"),
            (set(BASE, "user", &[]), "user"),
            (set(BASE, "port", &[]), "port"),
            (set(BASE, "port", &["abc"]), "port"),
            (set(BASE, "connecttimeout", &["soon"]), "connecttimeout"),
        ] {
            match from_ssh_g(&out, BASE, &conn(""), Path::new("/w"), &home) {
                Err(Refusal::Malformed(m)) => assert!(m.contains(key), "{key}: {m}"),
                other => panic!("{key}: {other:?}"),
            }
        }
    }
}
