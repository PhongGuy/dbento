//! An sshd bastion and a Postgres server on one user-defined network, for tunnel tests. The
//! sshd image is built locally from Alpine, with its host and client keys generated at build
//! time and read out of the container, so no private key is ever committed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::bollard::models::{HostConfig, PortBinding};
use testcontainers_modules::testcontainers::bollard::query_parameters::InspectContainerOptions;
use testcontainers_modules::testcontainers::core::{BuildImageOptions, ContainerPort, ExecCommand};
use testcontainers_modules::testcontainers::runners::{AsyncBuilder, AsyncRunner};
use testcontainers_modules::testcontainers::{
    ContainerAsync, GenericBuildableImage, GenericImage, ImageExt,
};
use tokio::io::AsyncBufReadExt as _;

use super::{
    ATTEMPT_LABEL, NEXT_ATTEMPT, PgDb, engine, remove_labelled, retry_start, server_tls, start,
    start_hint, tls_script, wait_ready,
};

/// Password login, forwarding allowed.
pub const SSH_USER: &str = "tunnel";
/// Password login, forwarding refused.
pub const SSH_NOFWD_USER: &str = "nofwd";
/// The password of both password users.
pub const SSH_PASSWORD: &str = "tunnel-pw";
/// Public-key login only.
pub const SSH_KEY_USER: &str = "keyed";
/// The passphrase of the `ed25519_enc` client key.
pub const SSH_KEY_PASSPHRASE: &str = "fixture-phrase";

const IMAGE: &str = "dbento-sshd";

const DOCKERFILE: &str = r#"FROM docker.io/library/alpine:3.24
RUN apk add --no-cache openssh-server openssh-keygen \
 && ssh-keygen -q -t ed25519 -N '' -C '' -f /etc/ssh/ssh_host_ed25519_key \
 && mkdir /fixture \
 && ssh-keygen -q -t ed25519 -N '' -C '' -f /fixture/ed25519 \
 && ssh-keygen -q -t ed25519 -N fixture-phrase -C '' -f /fixture/ed25519_enc \
 && ssh-keygen -q -t rsa -b 3072 -N '' -C '' -f /fixture/rsa \
 && for n in 1 2 3; do ssh-keygen -q -t ed25519 -N '' -C '' -f /fixture/stranger$n; done \
 && adduser -D tunnel && adduser -D nofwd && adduser -D keyed \
 && printf 'tunnel:tunnel-pw\nnofwd:tunnel-pw\n' | chpasswd \
 && sed -i 's/^keyed:!:/keyed:*:/' /etc/shadow \
 && mkdir -m 700 /home/keyed/.ssh \
 && cat /fixture/ed25519.pub /fixture/ed25519_enc.pub /fixture/rsa.pub > /home/keyed/.ssh/authorized_keys \
 && chmod 600 /home/keyed/.ssh/authorized_keys \
 && chown -R keyed:keyed /home/keyed/.ssh
COPY sshd_config /etc/ssh/sshd_config
CMD ["/usr/sbin/sshd", "-D", "-e"]
"#;

// `adduser -D` leaves `keyed` locked, which the shadow edit above undoes so key logins work.
const SSHD_CONFIG: &str = "Port 22
HostKey /etc/ssh/ssh_host_ed25519_key
PermitRootLogin no
PasswordAuthentication yes
KbdInteractiveAuthentication no
PubkeyAuthentication yes
AllowTcpForwarding yes
AllowAgentForwarding no
X11Forwarding no
MaxAuthTries 3
Match User keyed
    PasswordAuthentication no
Match User nofwd
    AllowTcpForwarding no
";

const CLIENT_KEYS: [&str; 6] = [
    "ed25519",
    "ed25519_enc",
    "rsa",
    "stranger1",
    "stranger2",
    "stranger3",
];

static NEXT_NET: AtomicU64 = AtomicU64::new(0);

/// An sshd container and a Postgres container on one user-defined network. Ports are
/// published on 127.0.0.1 only. Drop inside a tokio runtime.
pub struct SshNet {
    // Declared first so the bastion goes before the database it fronts.
    sshd: ContainerAsync<GenericImage>,
    pg: PgDb,
    keys: tempfile::TempDir,
    ssh_port: u16,
    host_key: String,
    fingerprint: String,
    db_host: String,
    ca_pem: String,
}

impl SshNet {
    /// sshd's port 22, as published on 127.0.0.1.
    pub fn ssh_port(&self) -> u16 {
        self.ssh_port
    }
    /// The host key as `ssh-ed25519 AAAA...`, without a comment.
    pub fn host_key(&self) -> &str {
        &self.host_key
    }
    /// The host key's `SHA256:...` fingerprint, as `ssh-keygen -l` prints it.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    /// Postgres's name as the bastion reaches it on the shared network.
    pub fn db_host(&self) -> &str {
        &self.db_host
    }
    /// Postgres's port as the bastion reaches it.
    pub fn db_port(&self) -> u16 {
        5432
    }
    /// The same server, from the host.
    pub fn pg(&self) -> &PgDb {
        &self.pg
    }
    /// The CA that signed Postgres's certificate.
    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }
    /// "ed25519", "ed25519_enc" ([`SSH_KEY_PASSPHRASE`]), "rsa" (authorized for
    /// [`SSH_KEY_USER`]), "stranger1".."stranger3" (not authorized); `<name>.pub` sits beside each.
    pub fn client_key(&self, name: &str) -> PathBuf {
        self.keys.path().join(name)
    }

    /// The `HostIp` of every port either container publishes.
    pub async fn published_ips(&self) -> Vec<String> {
        let docker = engine().expect("reaching the container engine");
        let ids = [Some(self.sshd.id()), self.pg.container_id()];
        let mut ips = Vec::new();
        for id in ids.into_iter().flatten() {
            let inspected = docker
                .inspect_container(id, None::<InspectContainerOptions>)
                .await
                .expect("inspecting a fixture container");
            let ports = inspected
                .network_settings
                .and_then(|n| n.ports)
                .unwrap_or_default();
            for binding in ports.into_values().flatten().flatten() {
                ips.push(binding.host_ip.unwrap_or_default());
            }
        }
        ips
    }

    /// How many TCP sessions on port 22 sshd holds established.
    pub async fn ssh_sessions(&self) -> usize {
        // grep -c exits 1 on a zero count, so that code is a count too.
        let out = exec_stdout(
            &self.sshd,
            ["sh", "-c", "netstat -tn | grep -c ':22 .*ESTABLISHED'"],
            &[0, 1],
        )
        .await;
        out.trim()
            .parse()
            .unwrap_or_else(|e| panic!("netstat count {out:?}: {e}"))
    }
}

/// Postgres has ssl = on with a certificate naming `db_host()`.
pub async fn ssh_net() -> SshNet {
    net(None).await
}

/// As [`ssh_net`], the certificate naming `name` instead.
pub async fn ssh_net_with_cert_name(name: &str) -> SshNet {
    net(Some(name)).await
}

async fn net(cert_name: Option<&str>) -> SshNet {
    let pid = std::process::id();
    let n = NEXT_NET.fetch_add(1, Ordering::Relaxed);
    let network = format!("dbento-ssh-{pid}-{n}");
    let alias = format!("dbento-pg-{pid}-{n}");
    let tls = server_tls(&[cert_name.unwrap_or(&alias)]);
    let script = tls_script(&tls);
    // The fixed name is the bastion's DNS name for Postgres; `start` frees it before a retry.
    let pg = start(
        || {
            Postgres::default()
                .with_network(network.clone())
                .with_container_name(alias.clone())
                .with_copy_to(
                    "/docker-entrypoint-initdb.d/tls.sh",
                    script.as_bytes().to_vec(),
                )
                .with_host_config_modifier(|h| loopback_only(h, "5432/tcp"))
        },
        "127.0.0.1",
    )
    .await;
    let sshd = start_sshd(&network).await;
    let ssh_port = sshd
        .get_host_port_ipv4(22)
        .await
        .expect("no mapped sshd port");
    wait_ready(&sshd, "sshd", || async move { ssh_banner(ssh_port).await }).await;

    let keys = tempfile::tempdir().expect("client key directory");
    for name in CLIENT_KEYS {
        for file in [name.to_owned(), format!("{name}.pub")] {
            let text = exec_stdout(&sshd, ["cat", &format!("/fixture/{file}")], &[0]).await;
            write_private(&keys.path().join(&file), &text);
        }
    }
    let host_pub = exec_stdout(&sshd, ["cat", "/etc/ssh/ssh_host_ed25519_key.pub"], &[0]).await;
    let host_key = host_pub
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let listed = exec_stdout(
        &sshd,
        ["ssh-keygen", "-lf", "/etc/ssh/ssh_host_ed25519_key.pub"],
        &[0],
    )
    .await;
    let fingerprint = listed
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("ssh-keygen -l printed {listed:?}"))
        .to_owned();
    SshNet {
        sshd,
        pg,
        keys,
        ssh_port,
        host_key,
        fingerprint,
        db_host: alias,
        ca_pem: tls.ca_pem,
    }
}

/// Publishes only `port` and only on 127.0.0.1, with a host port the engine picks. It replaces
/// the publish-everything default testcontainers sets on a networked request.
fn loopback_only(host: &mut HostConfig, port: &str) {
    host.publish_all_ports = Some(false);
    host.port_bindings = Some(HashMap::from([(
        port.to_owned(),
        Some(vec![PortBinding {
            host_ip: Some("127.0.0.1".to_owned()),
            host_port: None,
        }]),
    )]));
}

/// Builds the image once per content (its tag hashes the build inputs) and starts it on
/// `network`, labelled like the Postgres containers so a failed or orphaned start is removed.
async fn start_sshd(network: &str) -> ContainerAsync<GenericImage> {
    let tag = format!("{:016x}", fnv1a(&[DOCKERFILE, SSHD_CONFIG]));
    let image = GenericBuildableImage::new(IMAGE, &tag)
        .with_dockerfile_string(DOCKERFILE)
        .with_data(SSHD_CONFIG, "./sshd_config")
        .build_image_with(BuildImageOptions::new().with_skip_if_exists(true))
        .await
        .unwrap_or_else(|e| panic!("cannot build the sshd test image: {e}"));
    retry_start(|_| {
        let attempt = format!(
            "{}-{}",
            std::process::id(),
            NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed)
        );
        let request = image
            .clone()
            .with_exposed_port(ContainerPort::Tcp(22))
            .with_network(network)
            .with_label("dbento.role", "test")
            .with_label("dbento.pid", std::process::id().to_string())
            .with_label(ATTEMPT_LABEL, attempt.clone())
            .with_host_config_modifier(|h| {
                loopback_only(h, "22/tcp");
                own_oom_score(h);
            });
        async move {
            match request.start().await {
                Ok(container) => Ok(container),
                Err(e) => {
                    remove_labelled(&format!("{ATTEMPT_LABEL}={attempt}")).await;
                    Err(e.to_string())
                }
            }
        }
    })
    .await
    .unwrap_or_else(|e| panic!("{}", start_hint(&e)))
}

/// Ready once sshd itself sends its banner: rootlessport accepts a connection before sshd
/// listens, then closes it.
async fn ssh_banner(port: u16) -> Result<(), String> {
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    tokio::io::BufReader::new(stream)
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?;
    if line.starts_with("SSH-2.0-") {
        Ok(())
    } else {
        Err(format!("no SSH banner yet: {line:?}"))
    }
}

/// Runs `cmd` in `container` and returns its stdout, panicking with its stderr when it exits
/// with a code `ok` does not list.
async fn exec_stdout<const N: usize>(
    container: &ContainerAsync<GenericImage>,
    cmd: [&str; N],
    ok: &[i64],
) -> String {
    let mut result = container
        .exec(ExecCommand::new(cmd))
        .await
        .unwrap_or_else(|e| panic!("exec {cmd:?}: {e}"));
    let out = result
        .stdout_to_vec()
        .await
        .unwrap_or_else(|e| panic!("stdout of {cmd:?}: {e}"));
    let code = result.exit_code().await.ok().flatten();
    if !code.is_some_and(|c| ok.contains(&c)) {
        let err = result.stderr_to_vec().await.unwrap_or_default();
        panic!(
            "exec {cmd:?} exited {code:?}: {}",
            String::from_utf8_lossy(&err)
        );
    }
    String::from_utf8(out).unwrap_or_else(|e| panic!("stdout of {cmd:?} is not UTF-8: {e}"))
}

/// Pins the container's OOM score to this process's own. Rootless podman otherwise records a
/// lower value it cannot apply to an exec, which crun then refuses.
fn own_oom_score(host: &mut HostConfig) {
    if let Some(adj) = std::fs::read_to_string("/proc/self/oom_score_adj")
        .ok()
        .and_then(|s| s.trim().parse().ok())
    {
        host.oom_score_adj = Some(adj);
    }
}

/// Writes a key file readable by its owner only, as ssh clients demand of a private key.
fn write_private(path: &std::path::Path, text: &str) {
    std::fs::write(path, text).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
    }
}

/// 64-bit FNV-1a over `parts` in order, a stable content tag across runs and toolchains.
fn fnv1a(parts: &[&str]) -> u64 {
    parts
        .iter()
        .flat_map(|p| p.bytes())
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        })
}
