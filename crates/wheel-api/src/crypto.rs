//! Envelope encryption for per-project secrets, plus a secret wrapper that resists being logged.

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Key, Nonce};
use anyhow::{bail, Context, Result};
use base64::Engine as _;

/// A string that must never appear in logs, traces, or error messages.
///
/// `Debug` is redacted, and there is deliberately no `Display`. The only way to see the contents is
/// `expose()`, which is easy to grep for in review — the point is that leaking becomes a visible,
/// deliberate act rather than the default behaviour of `{:?}` in a log line someone added later.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Generate a fresh 32-byte secret, base64 (URL-safe, unpadded) encoded.
pub fn generate_secret() -> Secret {
    use aes_gcm::aead::rand_core::RngCore;
    let mut buf = [0u8; 32];
    OsRng.fill_bytes(&mut buf);
    Secret::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

/// Encrypt under the API master key. Output layout: `nonce (12 bytes) || ciphertext || tag`.
pub fn seal(master_key: &[u8; 32], plaintext: &Secret) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(master_key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let mut out = nonce.to_vec();
    let ct = cipher
        .encrypt(&nonce, plaintext.expose().as_bytes())
        .map_err(|_| anyhow::anyhow!("aead encryption failed"))?;
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn open(master_key: &[u8; 32], sealed: &[u8]) -> Result<Secret> {
    if sealed.len() < 12 + 16 {
        bail!("sealed blob is too short to be valid");
    }
    let (nonce_bytes, ct) = sealed.split_at(12);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(master_key));
    let pt = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ct)
        // A failure here means the key rotated, the row was tampered with, or the blob is
        // corrupt. All three are operator problems; none should reveal which to a client.
        .map_err(|_| anyhow::anyhow!("aead decryption failed (wrong master key or tampered ciphertext)"))?;
    Ok(Secret::new(
        String::from_utf8(pt).context("decrypted secret was not valid utf-8")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; 32] {
        [7u8; 32]
    }

    #[test]
    fn roundtrip() {
        let s = generate_secret();
        let sealed = seal(&key(), &s).unwrap();
        assert_eq!(open(&key(), &sealed).unwrap(), s);
    }

    #[test]
    fn ciphertext_differs_each_time() {
        // A fixed nonce would be catastrophic for GCM; prove the nonce actually varies.
        let s = Secret::new("same-plaintext");
        let a = seal(&key(), &s).unwrap();
        let b = seal(&key(), &s).unwrap();
        assert_ne!(a, b, "nonce reuse: identical plaintext produced identical ciphertext");
    }

    #[test]
    fn wrong_key_fails() {
        let sealed = seal(&key(), &Secret::new("x")).unwrap();
        assert!(open(&[9u8; 32], &sealed).is_err());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let mut sealed = seal(&key(), &Secret::new("x")).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(open(&key(), &sealed).is_err(), "GCM tag did not reject tampering");
    }

    #[test]
    fn secret_debug_is_redacted() {
        let s = Secret::new("hunter2-super-secret");
        assert!(!format!("{s:?}").contains("hunter2"));
    }
}
