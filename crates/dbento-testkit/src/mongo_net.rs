//! MongoDB members on an [`SshNet`](crate::SshNet)'s network, reachable only by name from inside
//! it: a standalone server or a two-member replica set, serving TLS optionally under one CA.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use testcontainers_modules::mongo::Mongo;
use testcontainers_modules::testcontainers::bollard::query_parameters::InspectContainerOptions;
use testcontainers_modules::testcontainers::{ContainerAsync, ContainerRequest, ImageExt};
use tokio::sync::OwnedSemaphorePermit;

use super::mongo::{cache_args, capped, slots, start};
use super::sshd::{exec_stdout, own_oom_score};
use super::{NEXT_DB, READY_TIMEOUT, engine, named_tls, sweep_orphans, wait_ready};

/// The DNS zone every member's name ends in.
pub const MG_ZONE: &str = "dbento.test";

const REPLICA_SET: &str = "rs0";
const PORT: u16 = 27017;

/// MongoDB members on one network, publishing no port. Drop inside a tokio runtime, before the
/// network's [`SshNet`](crate::SshNet).
pub struct MgNet {
    // Declared before the permit, so the containers are gone before another may start.
    containers: Vec<ContainerAsync<Mongo>>,
    names: Vec<String>,
    ips: Vec<String>,
    ca_pem: String,
    _permit: OwnedSemaphorePermit,
}

impl MgNet {
    /// Every member's name, which is its DNS name on the network; m0 first.
    pub fn names(&self) -> &[String] {
        &self.names
    }
    /// Member `i`'s name.
    pub fn name(&self, i: usize) -> &str {
        &self.names[i]
    }
    /// Member `i`'s address on the network.
    pub fn ip(&self, i: usize) -> &str {
        &self.ips[i]
    }
    /// The members' container ids, for [`published_ips`](crate::published_ips).
    pub fn ids(&self) -> Vec<&str> {
        self.containers.iter().map(|c| c.id()).collect()
    }
    /// The CA that signed every member's certificate.
    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }
    /// Runs `mongosh --quiet --eval js` in member `i` and returns its stdout; panics on a
    /// non-zero exit.
    pub async fn eval(&self, i: usize, js: &str) -> String {
        exec_stdout(
            &self.containers[i],
            ["mongosh", "--quiet", "--eval", js],
            &[0],
        )
        .await
    }
}

/// 1 member: standalone. 2 members: replica set `rs0`, m1 at priority 0; returns once m0 is
/// PRIMARY and m1 SECONDARY. Panics above 2.
pub async fn mongo_on(network: &str, members: usize) -> MgNet {
    assert!(
        (1..=2).contains(&members),
        "mongo_on takes 1 or 2 members, not {members}"
    );
    let permit = slots()
        .acquire_many_owned(members as u32)
        .await
        .expect("the container semaphore is never closed");
    sweep_orphans().await;
    let n = NEXT_DB.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let names: Vec<String> = (0..members)
        .map(|i| format!("m{i}-{pid}-{n}.{MG_ZONE}"))
        .collect();
    let tls = named_tls(&names.iter().map(String::as_str).collect::<Vec<_>>());
    let server_pem = format!("{}{}", tls.cert_pem, tls.key_pem);

    let mut containers = Vec::with_capacity(members);
    for name in &names {
        let c = start(|| member(network, name, members > 1, &tls.ca_pem, &server_pem)).await;
        wait_ready(&c, "MongoDB", || ping(&c)).await;
        containers.push(c);
    }
    let mut net = MgNet {
        containers,
        names,
        ips: Vec::new(),
        ca_pem: tls.ca_pem,
        _permit: permit,
    };
    for c in &net.containers {
        net.ips.push(network_ip(c.id(), network).await);
    }
    if members > 1 {
        initiate(&net).await;
    }
    net
}

/// One member's request: TLS allowed but not required, so in-container setup and replication
/// stay plain while a client asking for TLS verifies the member's name.
fn member(
    network: &str,
    name: &str,
    replica: bool,
    ca_pem: &str,
    server_pem: &str,
) -> ContainerRequest<Mongo> {
    let mut args = cache_args();
    args.extend(
        [
            "--bind_ip_all",
            "--port",
            "27017",
            "--tlsMode",
            "allowTLS",
            "--tlsCertificateKeyFile",
            "/etc/dbento-tls/server.pem",
            "--tlsCAFile",
            "/etc/dbento-tls/ca.pem",
            "--tlsAllowConnectionsWithoutCertificates",
        ]
        .map(str::to_owned),
    );
    if replica {
        args.extend(["--replSet", REPLICA_SET].map(str::to_owned));
    }
    capped(args)
        .with_network(network.to_owned())
        .with_container_name(name.to_owned())
        .with_copy_to("/etc/dbento-tls/ca.pem", ca_pem.as_bytes().to_vec())
        .with_copy_to("/etc/dbento-tls/server.pem", server_pem.as_bytes().to_vec())
        // The modifier replaces capped()'s, so it carries the memory cap too; a networked request
        // otherwise publishes every exposed port.
        .with_host_config_modifier(|h| {
            h.memory = Some(1 << 30);
            h.publish_all_ports = Some(false);
            h.port_bindings = None;
            own_oom_score(h);
        })
}

/// Ready once the member answers a ping from inside its own container.
async fn ping(c: &ContainerAsync<Mongo>) -> Result<(), String> {
    // A short selection timeout, so a server not yet listening fails this probe quickly.
    let uri = format!("mongodb://127.0.0.1:{PORT}/?serverSelectionTimeoutMS=1000");
    let out = exec_stdout(
        c,
        [
            "mongosh",
            &uri,
            "--quiet",
            "--eval",
            "db.runCommand({ping:1}).ok",
        ],
        &[0, 1],
    )
    .await;
    if out.trim() == "1" {
        Ok(())
    } else {
        Err(format!("no ping reply yet: {}", out.trim()))
    }
}

/// Container `id`'s address on `network`.
async fn network_ip(id: &str, network: &str) -> String {
    let docker = engine().expect("reaching the container engine");
    let inspected = docker
        .inspect_container(id, None::<InspectContainerOptions>)
        .await
        .expect("inspecting a MongoDB member");
    inspected
        .network_settings
        .and_then(|s| s.networks)
        .and_then(|mut n| n.remove(network))
        .and_then(|e| e.ip_address)
        .filter(|ip| !ip.is_empty())
        .unwrap_or_else(|| panic!("member {id} has no address on {network}"))
}

/// Initiates `rs0` from m0, then waits until m0 is PRIMARY and m1 SECONDARY.
async fn initiate(net: &MgNet) {
    let config = format!(
        "rs.initiate({{_id:\"{REPLICA_SET}\",members:[{{_id:0,host:\"{}:{PORT}\"}},{{_id:1,host:\"{}:{PORT}\",priority:0}}]}})",
        net.name(0),
        net.name(1)
    );
    net.eval(0, &config).await;
    // A status read can throw while the set is still forming; that is a not-yet, not a failure.
    let status = "try { print(rs.status().members.map(m => m.stateStr).join(',')) } \
                  catch (e) { print(e.codeName) }";
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let states = net.eval(0, status).await;
        if states.trim() == "PRIMARY,SECONDARY" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the MongoDB replica set never settled: {}",
            states.trim()
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
