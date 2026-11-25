//! How the MySQL driver opens each connection, directly or through the session's SSH link.
//!
//! mysql_async only dials sockets itself, so a tunnelled connection dials a one-shot listener
//! on loopback that pipes to a fresh SSH channel, with TLS still naming the configured host.

use std::net::Ipv4Addr;
use std::sync::{Arc, Weak};
use std::time::Duration;

use mysql_async::{Conn, Opts, OptsBuilder};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::session::ConnectError;
use crate::ssh::{Dialer, SshLink};
use crate::workspace::ConnectionConfig;

/// How long a one-shot listener waits for mysql_async to dial it.
pub(crate) const ACCEPT_WINDOW: Duration = Duration::from_secs(10);

/// Listens on 127.0.0.1:0 for one connection within `ACCEPT_WINDOW`, closes the listener and
/// pipes that connection to `stream` until both directions end; answers the port to dial.
pub(crate) async fn one_shot<S>(stream: S) -> std::io::Result<(u16, JoinHandle<()>)>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = listener.local_addr()?.port();
    // The task owns the stream, so an SSH channel always drops inside the runtime. The
    // listener closes as soon as it has its one connection, or none came in time.
    let piping = tokio::spawn(async move {
        let mut stream = stream;
        let accepted = tokio::time::timeout(ACCEPT_WINDOW, listener.accept()).await;
        drop(listener);
        if let Ok(Ok((mut local, _))) = accepted {
            // Either side failing ends the pipe; the connection then sees its socket close.
            local.set_nodelay(true).ok();
            tokio::io::copy_bidirectional(&mut local, &mut stream)
                .await
                .ok();
        }
    });
    Ok((port, piping))
}

/// `base` aimed at the listener on `port`, its TLS (if any) verifying `real_host`.
pub(crate) fn tunnel_opts(base: &Opts, real_host: &str, port: u16) -> Opts {
    let ssl = base
        .ssl_opts()
        .cloned()
        .map(|s| s.with_danger_tls_hostname_override(Some(real_host.to_owned())));
    OptsBuilder::from_opts(base.clone())
        .ip_or_hostname("127.0.0.1")
        .tcp_port(port)
        .ssl_opts(ssl)
        .into()
}

/// Opens connections to one server: the session's own and the kill door's side ones.
#[derive(Clone)]
pub(crate) struct MyDial {
    opts: Opts,
    via: Via,
}

#[derive(Clone)]
enum Via {
    Direct,
    /// Weak, so a held cancel handle never keeps the SSH connection open.
    Ssh {
        link: Weak<SshLink>,
        host: String,
        port: u16,
    },
}

/// Why a dial failed: the tunnel, or MySQL itself.
pub(crate) enum Dialed {
    Ssh(ConnectError),
    Mysql(mysql_async::Error),
}

impl MyDial {
    pub(crate) fn new(opts: Opts, conn: &ConnectionConfig, dialer: &Dialer) -> MyDial {
        match dialer {
            Dialer::Direct => MyDial::direct(opts),
            Dialer::Ssh(link) => MyDial {
                opts,
                via: Via::Ssh {
                    link: Arc::downgrade(link),
                    host: conn.host().unwrap_or("localhost").to_owned(),
                    port: conn.port().unwrap_or(3306),
                },
            },
        }
    }

    /// A `Dialer::Direct` dial of `opts`; `new` returns this for `Dialer::Direct`.
    pub(crate) fn direct(opts: Opts) -> MyDial {
        MyDial {
            opts,
            via: Via::Direct,
        }
    }

    pub(crate) async fn conn(&self) -> Result<Conn, Dialed> {
        let (link, host, port) = match &self.via {
            Via::Direct => return Conn::new(self.opts.clone()).await.map_err(Dialed::Mysql),
            Via::Ssh { link, host, port } => (link, host, *port),
        };
        let tunnel = |message: String| Dialed::Ssh(ConnectError::SshTunnel { message });
        let Some(link) = link.upgrade() else {
            return Err(tunnel("the session's SSH connection is closed".to_owned()));
        };
        // The channel opens first, so a refused forward fails before mysql_async runs.
        let stream = link.channel(host, port).await.map_err(Dialed::Ssh)?;
        drop(link);
        let (local, _piping) = one_shot(stream).await.map_err(|e| {
            tunnel(format!(
                "cannot listen on 127.0.0.1 for the SSH channel: {e}"
            ))
        })?;
        Conn::new(tunnel_opts(&self.opts, host, local))
            .await
            .map_err(Dialed::Mysql)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mysql_async::{OptsBuilder, SslOpts};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[tokio::test]
    async fn the_listener_pipes_its_first_connection_then_refuses_the_next() {
        let (near, mut far) = tokio::io::duplex(1024);
        let (port, piping) = one_shot(near).await.unwrap();
        let mut first = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        first.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        far.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");
        far.write_all(b"pong").await.unwrap();
        first.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"pong");
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err()
        );
        drop(far);
        assert_eq!(first.read(&mut got).await.unwrap(), 0);
        drop(first);
        piping.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn an_unclaimed_listener_closes_once_its_window_passes() {
        let (near, _far) = tokio::io::duplex(1024);
        let (port, piping) = one_shot(near).await.unwrap();
        piping.await.unwrap();
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err()
        );
    }

    #[test]
    fn a_tunneled_dial_aims_at_the_listener_and_verifies_the_real_host() {
        let base: Opts = OptsBuilder::default()
            .ip_or_hostname("db.internal")
            .tcp_port(3306)
            .prefer_socket(false)
            .ssl_opts(SslOpts::default())
            .into();
        let o = tunnel_opts(&base, "db.internal", 40123);
        assert_eq!((o.ip_or_hostname(), o.tcp_port()), ("127.0.0.1", 40123));
        assert!(!o.prefer_socket());
        let ssl = o.ssl_opts().unwrap();
        assert_eq!(ssl.tls_hostname_override(), Some("db.internal"));
        assert!(!ssl.skip_domain_validation() && !ssl.accept_invalid_certs());
    }

    #[test]
    fn a_tunneled_dial_without_tls_stays_without_tls() {
        let base: Opts = OptsBuilder::default().ip_or_hostname("db.internal").into();
        assert!(
            tunnel_opts(&base, "db.internal", 40123)
                .ssl_opts()
                .is_none()
        );
    }
}
