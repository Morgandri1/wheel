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

/// Generate a fresh 32-byte bearer secret, base64 (URL-safe, unpadded) encoded.
///
/// URL-safe because bearer secrets travel in headers and, in dev, in URLs.
pub fn generate_secret() -> Secret {
    Secret::new(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_32()))
}

/// Generate a project vault key: 32 random bytes in **standard, padded** base64.
///
/// The alphabet is part of the engine spawn contract, not a detail: the engine decodes
/// `WHEEL_VAULT_KEY` with standard base64, so a URL-safe or unpadded key is not a key it can use.
pub fn generate_vault_key() -> Secret {
    Secret::new(base64::engine::general_purpose::STANDARD.encode(random_32()))
}

/// Re-encode a stored vault key into the exact form the engine parses.
///
/// This is a transport re-encoding, never a rotation: the 32 bytes are unchanged, so ciphertext
/// written under the old spelling still decrypts. It exists so projects provisioned with a key the
/// engine could not decode heal on their next start instead of needing a new vault.
pub fn canonical_vault_key(key: &Secret) -> Result<Secret> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    let raw = key.expose().trim();
    let bytes = [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD]
        .iter()
        .find_map(|e| e.decode(raw).ok())
        .context("vault key is not base64 in any known alphabet")?;
    if bytes.len() != 32 {
        bail!(
            "vault key must be 32 bytes for AES-256, got {}",
            bytes.len()
        );
    }
    Ok(Secret::new(STANDARD.encode(bytes)))
}

fn random_32() -> [u8; 32] {
    use aes_gcm::aead::rand_core::RngCore;
    let mut buf = [0u8; 32];
    OsRng.fill_bytes(&mut buf);
    buf
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
        .map_err(|_| {
            anyhow::anyhow!("aead decryption failed (wrong master key or tampered ciphertext)")
        })?;
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

    /// The engine decodes `WHEEL_VAULT_KEY` with standard base64. This test is the contract:
    /// if it fails, every vault write in production fails with "no usable vault key".
    #[test]
    fn a_generated_vault_key_decodes_the_way_the_engine_decodes_it() {
        use base64::engine::general_purpose::STANDARD;
        for _ in 0..64 {
            let k = generate_vault_key();
            let bytes = STANDARD
                .decode(k.expose())
                .expect("engine-side standard base64 decode");
            assert_eq!(bytes.len(), 32);
        }
    }

    #[test]
    fn a_url_safe_key_is_re_encoded_without_changing_the_key_material() {
        use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
        // Bytes chosen so the two alphabets actually differ (`-`/`_` vs `+`/`/`).
        let bytes = [0xfbu8; 32];
        let legacy = Secret::new(URL_SAFE_NO_PAD.encode(bytes));
        let fixed = canonical_vault_key(&legacy).unwrap();
        assert_ne!(fixed, legacy);
        assert_eq!(STANDARD.decode(fixed.expose()).unwrap(), bytes);
    }

    #[test]
    fn a_canonical_key_survives_canonicalisation_unchanged() {
        let k = generate_vault_key();
        assert_eq!(canonical_vault_key(&k).unwrap(), k);
    }

    #[test]
    fn a_key_of_the_wrong_length_is_refused_rather_than_padded() {
        use base64::engine::general_purpose::STANDARD;
        let short = Secret::new(STANDARD.encode([1u8; 16]));
        assert!(canonical_vault_key(&short).is_err());
        assert!(canonical_vault_key(&Secret::new("not base64 at all!!")).is_err());
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
        assert_ne!(
            a, b,
            "nonce reuse: identical plaintext produced identical ciphertext"
        );
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
        assert!(
            open(&key(), &sealed).is_err(),
            "GCM tag did not reject tampering"
        );
    }

    #[test]
    fn secret_debug_is_redacted() {
        let s = Secret::new("hunter2-super-secret");
        assert!(!format!("{s:?}").contains("hunter2"));
    }
}
