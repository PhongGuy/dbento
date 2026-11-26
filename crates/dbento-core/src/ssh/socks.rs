//! The server half of a SOCKS5 handshake (RFC 1928 with RFC 1929 login): it reads a CONNECT
//! request from any stream and answers with the fixed reply shapes the MongoDB driver expects.

#![cfg_attr(not(test), allow(dead_code))] // the tunnel's SOCKS endpoint is the only caller

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};

use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

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
    // Only the endpoint reads the error; the tests match the variant alone.
    Io(#[cfg_attr(test, allow(dead_code))] io::Error),
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
