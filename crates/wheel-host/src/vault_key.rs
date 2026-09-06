//! Canonicalising the per-project vault key.
//!
//! The key travels API -> host -> engine as base64, and the engine decodes it with the *standard*
//! alphabet. A key minted in the URL-safe alphabet is the same 32 bytes but does not decode there,
//! and the engine then starts without a usable key: every vault write fails at runtime, far from
//! the mistake. The host is the last hop before the engine, so it is the right place to make the
//! contract it is implementing true.
//!
//! This is a re-encoding, never a rotation. The bytes are unchanged, so anything already encrypted
//! under the key still decrypts — which is what makes it safe to apply to keys already on disk.

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;

/// Re-encode a vault key into the exact form the engine parses.
pub fn canonical(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let bytes = [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD]
        .iter()
        .find_map(|e| e.decode(trimmed).ok())
        .context("vault key is not base64 in any known alphabet")?;
    if bytes.len() != 32 {
        bail!(
            "vault key must be 32 bytes for AES-256, got {}",
            bytes.len()
        );
    }
    Ok(STANDARD.encode(bytes))
}

/// Canonicalise if we can, otherwise pass the value through untouched.
///
/// A key we cannot parse is not ours to reject here: the engine reports an unusable key with a
/// message naming the problem, and swallowing the project at the host would replace that with
/// silence. Warn, and let the value reach the component that can explain it.
pub fn canonical_or_passthrough(raw: &str) -> String {
    match canonical(raw) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(
                error = %format_args!("{e:#}"),
                "vault key could not be canonicalised; passing it through unchanged"
            );
            raw.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_safe_key_becomes_standard_without_changing_the_bytes() {
        // 0xfb repeated encodes to '+'/'/' in standard and '-'/'_' in url-safe, so the two forms
        // genuinely differ.
        let bytes = [0xfbu8; 32];
        let fixed = canonical(&URL_SAFE_NO_PAD.encode(bytes)).unwrap();
        assert_eq!(STANDARD.decode(&fixed).unwrap(), bytes);
        assert_eq!(fixed, STANDARD.encode(bytes));
    }

    #[test]
    fn a_canonical_key_is_returned_unchanged() {
        let k = STANDARD.encode([7u8; 32]);
        assert_eq!(canonical(&k).unwrap(), k);
    }

    #[test]
    fn surrounding_whitespace_does_not_make_a_key_unusable() {
        let k = STANDARD.encode([3u8; 32]);
        assert_eq!(canonical(&format!("  {k}\n")).unwrap(), k);
    }

    #[test]
    fn a_key_of_the_wrong_length_is_refused_rather_than_padded() {
        assert!(canonical(&STANDARD.encode([1u8; 16])).is_err());
        assert!(canonical(&STANDARD.encode([1u8; 64])).is_err());
    }

    #[test]
    fn something_that_is_not_base64_is_refused() {
        assert!(canonical("not base64 at all !!").is_err());
        assert!(canonical("").is_err());
    }

    /// A key we cannot parse must still reach the engine, which reports it with a message that
    /// names the problem. Dropping it here would turn a clear error into a silent one.
    #[test]
    fn an_unparseable_key_passes_through_untouched() {
        assert_eq!(canonical_or_passthrough("garbage"), "garbage");
    }

    #[test]
    fn passthrough_canonicalises_when_it_can() {
        let bytes = [0xfbu8; 32];
        assert_eq!(
            canonical_or_passthrough(&URL_SAFE_NO_PAD.encode(bytes)),
            STANDARD.encode(bytes)
        );
    }
}
