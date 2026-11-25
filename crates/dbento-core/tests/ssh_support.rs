//! Shared pieces of the SSH tests: a workspace whose Postgres connection `pg` goes through
//! the sshd fixture, a temporary SSH home and state folder, and SSH secrets.
#![allow(dead_code)]

use dbento_core::{
    ConnectError, Secret, SshEnv, SshHome, SshSecrets, StatePaths, Workspace, ssh_channel_check,
};
use dbento_testkit::{SSH_USER, SshNet};
use tempfile::TempDir;

pub struct Tunnel {
    pub dir: TempDir,
    /// The SSH home; never the real one.
    pub home: TempDir,
    pub state: TempDir,
    pub ws: Workspace,
}

/// `ssh` for the password user.
pub fn password_login() -> String {
    format!("user = \"{SSH_USER}\"\nauth = \"password\"\n")
}

/// `pg` on the fixture's database, `ssh` after the table's host and port, `pg_extra` in
/// the connection's own table.
pub fn tunnel(net: &SshNet, ssh: &str, pg_extra: &str) -> Tunnel {
    tunnel_with(net, ssh, pg_extra, &[])
}

/// As `tunnel`, with `files` written into the workspace first.
pub fn tunnel_with(net: &SshNet, ssh: &str, pg_extra: &str, files: &[(&str, &str)]) -> Tunnel {
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    let toml = format!(
        "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"{}\"\nport = {}\ndatabase = \"{}\"\nuser = \"{}\"\n{pg_extra}\n[connections.pg.ssh]\nhost = \"127.0.0.1\"\nport = {}\n{ssh}\n",
        net.db_host(),
        net.db_port(),
        net.pg().database(),
        net.pg().user(),
        net.ssh_port(),
    );
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
    Tunnel {
        dir,
        home,
        state: tempfile::tempdir().unwrap(),
        ws,
    }
}

impl Tunnel {
    pub fn paths(&self) -> StatePaths {
        StatePaths::in_dir(self.state.path())
    }

    pub fn env(&self) -> SshEnv {
        SshEnv::new(
            SshHome::new(self.home.path(), "tester"),
            self.paths().ssh_known_hosts,
        )
    }

    /// Writes the fixture's host key into Dbento's own known_hosts.
    pub fn trust(&self, net: &SshNet) {
        let line = format!("[127.0.0.1]:{} {}\n", net.ssh_port(), net.host_key());
        std::fs::write(self.paths().ssh_known_hosts, line).unwrap();
    }

    /// Opens the link and one channel to `host:port` as the SSH server resolves it.
    pub async fn channel(
        &self,
        secrets: &SshSecrets,
        host: &str,
        port: u16,
    ) -> Result<(), ConnectError> {
        let conn = self.ws.declared("pg").unwrap();
        ssh_channel_check(&self.ws, conn, &self.env(), secrets, host, port).await
    }
}

pub fn password(pw: &str) -> SshSecrets {
    let mut s = SshSecrets::default();
    s.password = Some(Secret::new(pw));
    s
}

pub fn passphrase(phrase: &str) -> SshSecrets {
    let mut s = SshSecrets::default();
    s.key_passphrase = Some(Secret::new(phrase));
    s
}
