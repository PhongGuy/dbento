//! One socket type for drivers that dial for themselves: TCP when connecting directly, an SSH
//! channel when the connection goes through a tunnel.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use super::{Dialer, SshStream};
use crate::ConnectError;

/// A database socket: TCP for `Dialer::Direct`, an SSH channel for `Dialer::Ssh`.
pub(crate) enum DbStream {
    Tcp(TcpStream),
    Ssh(SshStream),
}

/// Why [`dial`] failed: the direct socket's own error, or the tunnel's refusal.
pub(crate) enum DialError {
    Io(io::Error),
    Ssh(ConnectError),
}

/// Direct: a TCP connection to `host:port` with Nagle off, as the drivers set it themselves;
/// SSH: a channel the link's server opens to `host:port`.
pub(crate) async fn dial(dialer: &Dialer, host: &str, port: u16) -> Result<DbStream, DialError> {
    match dialer {
        Dialer::Direct => {
            let tcp = TcpStream::connect((host, port))
                .await
                .map_err(DialError::Io)?;
            tcp.set_nodelay(true).map_err(DialError::Io)?;
            Ok(DbStream::Tcp(tcp))
        }
        Dialer::Ssh(link) => link
            .channel(host, port)
            .await
            .map(DbStream::Ssh)
            .map_err(DialError::Ssh),
    }
}

// Both inner streams are Unpin, so each call pins the active arm in place.
impl AsyncRead for DbStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            Self::Ssh(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for DbStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_write(cx, buf),
            Self::Ssh(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            Self::Ssh(s) => Pin::new(s).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Tcp(s) => s.is_write_vectored(),
            Self::Ssh(s) => s.is_write_vectored(),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_flush(cx),
            Self::Ssh(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_shutdown(cx),
            Self::Ssh(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[tokio::test]
    async fn a_direct_dial_is_a_nodelay_tcp_stream_that_carries_bytes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let echo = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4];
            socket.read_exact(&mut buf).await.unwrap();
            socket.write_all(&buf).await.unwrap();
        });
        let mut s = match dial(&Dialer::Direct, "127.0.0.1", port).await {
            Ok(s) => s,
            Err(DialError::Io(e)) => panic!("{e}"),
            Err(DialError::Ssh(e)) => panic!("{e}"),
        };
        let DbStream::Tcp(tcp) = &s else {
            panic!("not TCP")
        };
        assert!(tcp.nodelay().unwrap());
        s.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        s.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");
        echo.await.unwrap();
    }

    #[tokio::test]
    async fn a_refused_direct_dial_keeps_its_io_error() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        match dial(&Dialer::Direct, "127.0.0.1", port).await {
            Err(DialError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::ConnectionRefused),
            Err(DialError::Ssh(e)) => panic!("{e}"),
            Ok(_) => panic!("connected"),
        }
    }
}
