//! Decrypting DBeaver's credentials file, which is AES-128-CBC under a key every DBeaver
//! install shares, and keeping only each connection's user and password from it.

use super::DBeaverError;
use crate::Secret;
use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use zeroize::Zeroizing;

/// The largest credentials file read; a bigger one is refused before decrypting.
pub(crate) const MAX_CREDENTIALS_BYTES: u64 = 1 << 20;

// DBeaver's public, hard-coded key.
const KEY: [u8; 16] = [
    0xba, 0xbb, 0x4a, 0x9f, 0x77, 0x4a, 0xb8, 0x53, 0xc9, 0x6c, 0x2d, 0x65, 0x3d, 0xfe, 0x54, 0x4a,
];

/// One connection's saved login.
#[derive(Debug, Default)]
pub(crate) struct Creds {
    pub(crate) user: Option<String>,
    pub(crate) password: Option<Secret>,
}

// Only the `#connection` node matters; SSH tunnel and other nodes are skipped unread.
#[derive(Deserialize)]
struct Entry<N> {
    #[serde(rename = "#connection")]
    connection: Option<N>,
}

#[derive(Deserialize)]
struct WithPassword {
    user: Option<String>,
    password: Option<SecretField>,
}

// Without secrets the password field is skipped as an unknown field, so no string is built.
#[derive(Deserialize)]
struct WithoutPassword {
    user: Option<String>,
}

// Moves the decoded password straight into a `Secret`, which zeroizes it on drop.
struct SecretField(Secret);

impl<'de> Deserialize<'de> for SecretField {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(|s| SecretField(Secret::new(s)))
    }
}

/// Users and passwords by connection id from `#connection` nodes; a missing file is empty.
/// With `secrets` false no password is kept. Entries without `#connection` are absent.
pub(crate) fn read_credentials(
    path: &Path,
    secrets: bool,
) -> Result<HashMap<String, Creds>, DBeaverError> {
    let refused = || DBeaverError::Credentials {
        path: path.to_path_buf(),
    };
    let io = |e: std::io::Error| DBeaverError::Io {
        path: path.to_path_buf(),
        message: e.kind().to_string(),
    };
    let file = match std::fs::File::open(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        r => r.map_err(io)?,
    };
    if file.metadata().map_err(io)?.len() > MAX_CREDENTIALS_BYTES {
        return Err(refused());
    }
    // The cap holds even if the file grows after the size check.
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_CREDENTIALS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > MAX_CREDENTIALS_BYTES {
        return Err(refused());
    }
    let plain = decrypt(&bytes).ok_or_else(refused)?;
    // serde_json's message can quote plaintext, so any parse failure is the bare error.
    let creds = if secrets {
        parse::<WithPassword>(&plain)
            .ok_or_else(refused)?
            .map(|(id, n)| {
                (
                    id,
                    Creds {
                        user: n.user,
                        password: n.password.map(|p| p.0),
                    },
                )
            })
            .collect()
    } else {
        parse::<WithoutPassword>(&plain)
            .ok_or_else(refused)?
            .map(|(id, n)| {
                (
                    id,
                    Creds {
                        user: n.user,
                        password: None,
                    },
                )
            })
            .collect()
    };
    Ok(creds)
}

// The plaintext must be a JSON object of objects; ids without a `#connection` node drop.
fn parse<N: DeserializeOwned>(plain: &[u8]) -> Option<impl Iterator<Item = (String, N)>> {
    let entries: HashMap<String, Entry<N>> = serde_json::from_slice(plain).ok()?;
    Some(
        entries
            .into_iter()
            .filter_map(|(id, e)| Some((id, e.connection?))),
    )
}

/// IV (16 bytes) then AES-128-CBC with PKCS#7 under the public key; None on any failure.
pub(crate) fn decrypt(bytes: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    if bytes.len() < 32 {
        return None;
    }
    let (iv, body) = bytes.split_at(16);
    let iv: [u8; 16] = iv.try_into().ok()?;
    let mut buf = Zeroizing::new(body.to_vec());
    let len = cbc::Decryptor::<aes::Aes128>::new(&KEY.into(), &iv.into())
        .decrypt_padded::<Pkcs7>(&mut buf)
        .ok()?
        .len();
    buf.truncate(len);
    Some(buf)
}

/// Encrypts as DBeaver writes the file: the IV first, then the padded ciphertext.
#[cfg(test)]
pub(crate) fn encrypt(plain: &[u8], iv: [u8; 16]) -> Vec<u8> {
    use aes::cipher::BlockModeEncrypt;
    let mut buf = vec![0; plain.len() + 16];
    buf[..plain.len()].copy_from_slice(plain);
    let len = cbc::Encryptor::<aes::Aes128>::new(&KEY.into(), &iv.into())
        .encrypt_padded::<Pkcs7>(&mut buf, plain.len())
        .expect("one spare block holds the padding")
        .len();
    let mut out = iv.to_vec();
    out.extend_from_slice(&buf[..len]);
    out
}

#[cfg(test)]
mod tests;
