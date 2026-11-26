//! A SOCKS5 server (RFC 1928 with RFC 1929 login) on loopback that carries each CONNECT over a
//! [`Dialer`], so the MongoDB driver reaches every member through the session's SSH link.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::{AbortHandle, JoinSet};

use super::Dialer;
use super::stream::{self, DialError};

/// How long a client has to finish its handshake once it holds a slot.
pub(crate) const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);
/// How many handshakes may run at once; later clients wait for a slot.
pub(crate) const HANDSHAKE_SLOTS: usize = 64;
/// The most a refused client may still send before its socket is dropped.
const REFUSED_DRAIN: u64 = 64 * 1024;

/// A SOCKS5 server on `127.0.0.1` with fresh random credentials. Dropping it closes the port
/// and ends every relay.
pub(crate) struct SocksEndpoint {
    addr: SocketAddr,
    user: String,
    pass: String,
    refusal: Arc<Mutex<Option<String>>>,
    task: AbortHandle,
}

impl SocksEndpoint {
    /// [`SocksEndpoint::start_with`] at [`HANDSHAKE_DEADLINE`].
    pub(crate) async fn start(dialer: Dialer) -> io::Result<SocksEndpoint> {
        Self::start_with(dialer, HANDSHAKE_DEADLINE).await
    }

    /// Binds a loopback port and serves it on the link's runtime (the caller's for `Direct`),
    /// giving each client `deadline` to finish its handshake.
    pub(crate) async fn start_with(
        dialer: Dialer,
        deadline: Duration,
    ) -> io::Result<SocksEndpoint> {
        let user = credential()?;
        let pass = credential()?;
        let runtime = match &dialer {
            Dialer::Ssh(link) => link.runtime().clone(),
            Dialer::Direct => tokio::runtime::Handle::current(),
        };
        // The listener registers with the reactor of the runtime that will serve it.
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        std_listener.set_nonblocking(true)?;
        let listener = {
            let _entered = runtime.enter();
            TcpListener::from_std(std_listener)?
        };
        let addr = listener.local_addr()?;
        let refusal = Arc::new(Mutex::new(None));
        let serve = Serve {
            dialer,
            user: user.clone(),
            pass: pass.clone(),
            refusal: Arc::clone(&refusal),
            deadline,
        };
        let task = runtime.spawn(serve.run(listener)).abort_handle();
        Ok(SocksEndpoint {
            addr,
            user,
            pass,
            refusal,
            task,
        })
    }

    pub(crate) fn port(&self) -> u16 {
        self.addr.port()
    }

    #[cfg(test)]
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The login the endpoint accepts: `(user, password)`.
    pub(crate) fn credentials(&self) -> (&str, &str) {
        (&self.user, &self.pass)
    }

    /// Why the most recent CONNECT could not reach its target, if one could not.
    pub(crate) fn last_refusal(&self) -> Option<String> {
        self.refusal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for SocksEndpoint {
    fn drop(&mut self) {
        // Aborting needs no runtime; the task's JoinSet then aborts every relay with it.
        self.task.abort();
    }
}

/// 32 random bytes as 43 characters of unpadded base64url.
fn credential() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The accept task's state, shared by every connection it serves.
#[derive(Clone)]
struct Serve {
    dialer: Dialer,
    user: String,
    pass: String,
    refusal: Arc<Mutex<Option<String>>>,
    deadline: Duration,
}

impl Serve {
    /// Accepts until aborted; every connection runs in the JoinSet so aborting this task ends
    /// them all, and finished ones are reaped as the loop goes.
    async fn run(self, listener: TcpListener) {
        let slots = Arc::new(Semaphore::new(HANDSHAKE_SLOTS));
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    match accepted {
                        Ok((client, _)) => {
                            connections.spawn(self.clone().serve(client, Arc::clone(&slots)));
                        }
                        // Out of descriptors, say: back off rather than spin on the error.
                        Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                    }
                }
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    }

    /// The handshake holds a slot and runs under the deadline; the dial and relay do not.
    async fn serve(self, mut client: TcpStream, slots: Arc<Semaphore>) {
        let target = {
            let Ok(_slot) = slots.acquire_owned().await else {
                return;
            };
            let handshake = async {
                match negotiate(&mut client, &self.user, &self.pass).await {
                    Ok(target) => Some(target),
                    Err(_) => {
                        // Closing with unread input makes the kernel send a reset, so the
                        // refused client gets a FIN and a bounded drain before the drop.
                        let _ = client.shutdown().await;
                        let mut rest = (&mut client).take(REFUSED_DRAIN);
                        let _ = tokio::io::copy(&mut rest, &mut tokio::io::sink()).await;
                        None
                    }
                }
            };
            match tokio::time::timeout(self.deadline, handshake).await {
                Ok(Some(target)) => target,
                Ok(None) | Err(_) => return,
            }
        };
        match stream::dial(&self.dialer, &target.host, target.port).await {
            Ok(mut db) => {
                if reply(&mut client, REP_SUCCESS).await.is_ok() {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut db).await;
                }
            }
            Err(e) => {
                let why = match e {
                    DialError::Ssh(e) => e.to_string(),
                    DialError::Io(e) => e.to_string(),
                };
                *self.refusal.lock().unwrap_or_else(PoisonError::into_inner) = Some(why);
                let _ = reply(&mut client, REP_FAILURE).await;
            }
        }
    }
}

pub(crate) const REP_SUCCESS: u8 = 0x00;
pub(crate) const REP_FAILURE: u8 = 0x01;
const REP_NOT_ALLOWED: u8 = 0x02;
const REP_COMMAND_NOT_SUPPORTED: u8 = 0x07;
const REP_ADDRESS_NOT_SUPPORTED: u8 = 0x08;

const SOCKS5: u8 = 5;
const LOGIN_VERSION: u8 = 1;
const METHOD_PASSWORD: u8 = 0x02;
const NO_ACCEPTABLE_METHOD: u8 = 0xFF;
const CMD_CONNECT: u8 = 1;
const ATYP_IPV4: u8 = 1;
const ATYP_DOMAIN: u8 = 3;
const ATYP_IPV6: u8 = 4;

/// Where an accepted CONNECT asked to go: a host name or an IP literal, and a nonzero port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) host: String,
    pub(crate) port: u16,
}

/// Why [`negotiate`] turned a client away.
#[derive(Debug)]
pub(crate) enum Refusal {
    NotSocks5,
    NoAcceptableMethod,
    BadLogin,
    CommandNotSupported,
    AddressNotSupported,
    BadDomain,
    PortZero,
    // Kept for debugging; the endpoint drops a refused client without reading why.
    Io(#[allow(dead_code)] io::Error),
}

impl From<io::Error> for Refusal {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Reads the greeting, RFC 1929 login and request; on refusal it has already written the reply.
pub(crate) async fn negotiate<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    user: &str,
    pass: &str,
) -> Result<Target, Refusal> {
    // Anything that is not SOCKS5 (a stray HTTP request, say) gets no bytes back at all.
    if s.read_u8().await? != SOCKS5 {
        return Err(Refusal::NotSocks5);
    }
    let methods = read_field(s).await?;
    if !methods.contains(&METHOD_PASSWORD) {
        send(s, &[SOCKS5, NO_ACCEPTABLE_METHOD]).await?;
        return Err(Refusal::NoAcceptableMethod);
    }
    send(s, &[SOCKS5, METHOD_PASSWORD]).await?;

    // The whole login is read before it is judged, and both fields are compared in constant
    // time and combined, so a wrong user and a wrong password take the same path.
    let version = s.read_u8().await?;
    let given_user = read_field(s).await?;
    let given_pass = read_field(s).await?;
    let matches =
        given_user.as_slice().ct_eq(user.as_bytes()) & given_pass.as_slice().ct_eq(pass.as_bytes());
    let well_formed = version == LOGIN_VERSION && !given_user.is_empty() && !given_pass.is_empty();
    if !(bool::from(matches) && well_formed) {
        send(s, &[LOGIN_VERSION, 1]).await?;
        return Err(Refusal::BadLogin);
    }
    send(s, &[LOGIN_VERSION, 0]).await?;

    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    let [version, command, _reserved, atyp] = head;
    if version != SOCKS5 {
        return Err(Refusal::NotSocks5);
    }
    if command != CMD_CONNECT {
        reply(s, REP_COMMAND_NOT_SUPPORTED).await?;
        return Err(Refusal::CommandNotSupported);
    }
    let host = match atyp {
        ATYP_IPV4 => {
            let mut ip = [0u8; 4];
            s.read_exact(&mut ip).await?;
            Some(Ipv4Addr::from(ip).to_string())
        }
        ATYP_IPV6 => {
            let mut ip = [0u8; 16];
            s.read_exact(&mut ip).await?;
            Some(Ipv6Addr::from(ip).to_string())
        }
        ATYP_DOMAIN => String::from_utf8(read_field(s).await?)
            .ok()
            .filter(|name| !name.is_empty() && !name.contains('\0')),
        _ => {
            reply(s, REP_ADDRESS_NOT_SUPPORTED).await?;
            return Err(Refusal::AddressNotSupported);
        }
    };
    let port = s.read_u16().await?;
    let Some(host) = host else {
        reply(s, REP_FAILURE).await?;
        return Err(Refusal::BadDomain);
    };
    if port == 0 {
        reply(s, REP_NOT_ALLOWED).await?;
        return Err(Refusal::PortZero);
    }
    Ok(Target { host, port })
}

/// Writes `05 <rep> 00 01 00 00 00 00 00 00`.
pub(crate) async fn reply<S: AsyncWrite + Unpin>(s: &mut S, rep: u8) -> io::Result<()> {
    send(s, &[SOCKS5, rep, 0, ATYP_IPV4, 0, 0, 0, 0, 0, 0]).await
}

/// A length-prefixed field: one length byte, then that many bytes.
async fn read_field<S: AsyncRead + Unpin>(s: &mut S) -> io::Result<Vec<u8>> {
    let mut field = vec![0u8; usize::from(s.read_u8().await?)];
    s.read_exact(&mut field).await?;
    Ok(field)
}

async fn send<S: AsyncWrite + Unpin>(s: &mut S, bytes: &[u8]) -> io::Result<()> {
    s.write_all(bytes).await?;
    s.flush().await
}

#[cfg(test)]
mod tests;
