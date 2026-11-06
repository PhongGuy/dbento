//! Decrypting values Beekeeper Studio stores with `simple-encryptor`: the `.key` file is
//! wrapped under a public constant, and saved passwords under the key that file holds.

use super::BeekeeperError;
use crate::Secret;
use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use zeroize::Zeroizing;

/// The largest `.key` file read; a bigger one is refused unread.
pub(crate) const MAX_KEY_BYTES: u64 = 64 * 1024;

/// Beekeeper's public constant that wraps `.key` (research note, section 3).
pub(crate) const WRAP_KEY: &str =
    "38782F413F442A472D4B6150645367566B59703373367639792442264529482B";

#[derive(Deserialize)]
struct KeyFile {
    #[serde(rename = "encryptionKey")]
    encryption_key: String,
}

/// The column key from `.key`: None when the file is absent; `Key` when it is larger than
/// MAX_KEY_BYTES, unreadable, fails to decrypt, or is not `{"encryptionKey": "<string>"}`.
pub(crate) fn read_key(path: &Path) -> Result<Option<Zeroizing<String>>, BeekeeperError> {
    let refused = || BeekeeperError::Key {
        path: path.to_path_buf(),
    };
    let file = match std::fs::File::open(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        r => r.map_err(|_| refused())?,
    };
    // Reading one byte past the cap catches a file that grew after it was opened.
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_KEY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| refused())?;
    if bytes.len() as u64 > MAX_KEY_BYTES {
        return Err(refused());
    }
    let text = std::str::from_utf8(bytes.trim_ascii()).map_err(|_| refused())?;
    unwrap_key(text).map(Some).ok_or_else(refused)
}

/// The `encryptionKey` string of a wrapped `.key` text, or None.
pub(crate) fn unwrap_key(wrapped: &str) -> Option<Zeroizing<String>> {
    let plain = decrypt(WRAP_KEY, wrapped)?;
    // serde_json's error can quote plaintext, so it is dropped unread.
    let file: KeyFile = serde_json::from_slice(&plain).ok()?;
    Some(Zeroizing::new(file.encryption_key))
}

// simple-encryptor with hmac: hex HMAC (64), hex IV (32), base64 ciphertext; the HMAC covers
// the ASCII of IV hex plus base64, under SHA-256 of the key string's UTF-8 bytes.
/// HMAC-checked AES-256-CBC plaintext of one stored value, or None on any failure.
pub(crate) fn decrypt(key: &str, stored: &str) -> Option<Zeroizing<Vec<u8>>> {
    if stored.len() < 96 || !stored.is_ascii() {
        return None;
    }
    let (tag, body) = stored.split_at(64);
    let k = Zeroizing::new(<[u8; 32]>::from(Sha256::digest(key.as_bytes())));
    let tag = hex::decode(tag).ok()?;
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&*k).ok()?;
    mac.update(body.as_bytes());
    mac.verify_slice(&tag).ok()?;
    let (iv, ct) = body.split_at(32);
    let iv: [u8; 16] = hex::decode(iv).ok()?.try_into().ok()?;
    let mut buf = Zeroizing::new(base64::engine::general_purpose::STANDARD.decode(ct).ok()?);
    let len = cbc::Decryptor::<aes::Aes256>::new(&(*k).into(), &iv.into())
        .decrypt_padded::<Pkcs7>(&mut buf)
        .ok()?
        .len();
    buf.truncate(len);
    Some(buf)
}

/// A stored value whose plaintext is a JSON string, as a Secret; None otherwise.
pub(crate) fn decrypt_string(key: &str, stored: &str) -> Option<Secret> {
    let plain = decrypt(key, stored)?;
    serde_json::from_slice::<String>(&plain)
        .ok()
        .map(Secret::new)
}

/// Encrypts as simple-encryptor does, with a fixed IV so fixtures are reproducible.
#[cfg(test)]
pub(crate) fn encrypt(key: &str, plain: &[u8], iv: [u8; 16]) -> String {
    use aes::cipher::BlockModeEncrypt;
    let k: [u8; 32] = Sha256::digest(key.as_bytes()).into();
    let mut buf = vec![0; plain.len() + 16];
    buf[..plain.len()].copy_from_slice(plain);
    let len = cbc::Encryptor::<aes::Aes256>::new(&k.into(), &iv.into())
        .encrypt_padded::<Pkcs7>(&mut buf, plain.len())
        .expect("one spare block holds the padding")
        .len();
    let body = format!(
        "{}{}",
        hex::encode(iv),
        base64::engine::general_purpose::STANDARD.encode(&buf[..len])
    );
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&k).expect("any key length");
    mac.update(body.as_bytes());
    format!("{}{body}", hex::encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests;
