//! A Moto server: the one `DBENTO_DYNAMO_URL` (or, for S3, `DBENTO_S3_URL`) names, or a capped
//! container of the gate image killed when its last owner drops. Each DynamoDB handle owns one;
//! the S3 handles of a process share one. One Moto image serves both APIs.

use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use testcontainers_modules::testcontainers::core::ContainerPort;
use testcontainers_modules::testcontainers::{
    ContainerAsync, GenericImage, ImageExt, runners::AsyncRunner,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{ATTEMPT_LABEL, NEXT_ATTEMPT, engine, remove_labelled, retry_start, sweep_orphans};

/// The gate image's tag.
pub const MOTO_TAG: &str = "5.2.5";

pub(crate) const URL_VAR: &str = "DBENTO_DYNAMO_URL";
const MOTO_PORT: u16 = 5000;
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Where the Moto tests get their server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DySource {
    /// A running Moto at this endpoint.
    Url(String),
    /// A container of the gate image.
    Container,
}

/// A non-empty `DBENTO_DYNAMO_URL` wins; anything else means a container.
pub fn dy_source(env: impl Fn(&str) -> Option<String>) -> DySource {
    env(URL_VAR)
        .filter(|u| !u.is_empty())
        .map_or(DySource::Container, DySource::Url)
}

/// A Moto server held for one handle. One holding a container must be dropped inside a tokio
/// runtime.
pub(crate) struct Moto {
    endpoint: String,
    // Declared before the permit, so the container is gone before another may start.
    container: Option<ContainerAsync<GenericImage>>,
    _permit: Option<OwnedSemaphorePermit>,
}

impl Moto {
    /// The server's `http://host:port`.
    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

/// The URL server as is, or a fresh container started and waited on.
pub(crate) async fn moto(source: DySource) -> Moto {
    match source {
        DySource::Url(endpoint) => Moto {
            endpoint,
            container: None,
            _permit: None,
        },
        DySource::Container => {
            let (endpoint, c, permit) = container().await;
            Moto {
                endpoint,
                container: Some(c),
                _permit: Some(permit),
            }
        }
    }
}

impl Drop for Moto {
    // Runs on its own thread and runtime and never panics. A container is killed; a URL server
    // keeps its data, which each harness scopes or clears itself.
    fn drop(&mut self) {
        let Some(id) = self.container.as_ref().map(|c| c.id().to_owned()) else {
            return;
        };
        let done = std::thread::spawn(move || -> Result<(), String> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            runtime.block_on(async {
                engine()?
                    .kill_container(&id, None)
                    .await
                    .map_err(|e| format!("cannot kill container {id}: {e}"))
            })
        })
        .join();
        match done {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("dbento-testkit: cannot drop the Moto container: {e}"),
            Err(_) => eprintln!("dbento-testkit: dropping the Moto container panicked"),
        }
    }
}

/// Two Moto containers at a time in this process.
fn slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(SLOTS.get_or_init(|| Arc::new(Semaphore::new(2))))
}

/// Starts the gate image labelled and memory-capped, retried when its host port turns out to be
/// taken, and waits until it answers.
async fn container() -> (String, ContainerAsync<GenericImage>, OwnedSemaphorePermit) {
    let permit = slots()
        .acquire_owned()
        .await
        .expect("the container semaphore is never closed");
    sweep_orphans().await;
    let c = retry_start(|_| {
        let attempt = format!(
            "{}-{}",
            std::process::id(),
            NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed)
        );
        let request = GenericImage::new("motoserver/moto", MOTO_TAG)
            .with_exposed_port(ContainerPort::Tcp(MOTO_PORT))
            .with_host_config_modifier(|h| h.memory = Some(512 << 20))
            .with_label("dbento.role", "test")
            .with_label("dbento.pid", std::process::id().to_string())
            .with_label(ATTEMPT_LABEL, attempt.clone());
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
    .unwrap_or_else(|e| {
        panic!(
            "cannot start the Moto test container: {e}\n\
             Rootless podman: set DOCKER_HOST=unix://$XDG_RUNTIME_DIR/podman/podman.sock\n\
             Or skip containers and point {URL_VAR} at a running server."
        )
    });
    let port = c
        .get_host_port_ipv4(MOTO_PORT)
        .await
        .expect("no mapped Moto port");
    wait_ready(port).await;
    (format!("http://127.0.0.1:{port}"), c, permit)
}

/// Polls `GET /moto-api/` every 100 ms until it answers 200, for up to a minute; a probe that
/// stalls on connect or read is cut off at the same deadline.
async fn wait_ready(port: u16) {
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if let Ok(true) = tokio::time::timeout_at(deadline, answers_200(port)).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the Moto test container did not answer on port {port} within a minute");
}

async fn answers_200(port: u16) -> bool {
    let Ok(mut stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await else {
        return false;
    };
    if stream
        .write_all(b"GET /moto-api/ HTTP/1.0\r\n\r\n")
        .await
        .is_err()
    {
        return false;
    }
    let mut head = [0u8; 12];
    stream.read_exact(&mut head).await.is_ok() && head.ends_with(b" 200")
}
