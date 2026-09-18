// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Masking another project member's identifier for a guest-tier caller.
//!
//! A member's identifier (`Member.user_id`, `Project.owner_id`, `Message.on_behalf_of`) is a
//! principal (`auth::principal` in `wheel-api`): an opaque UUID under local auth, or an external
//! provider's `sub` under `jwks` — which, for some providers, IS the person's email. §5's own tier
//! model makes a guest view-only; showing another member's raw identifier in every response surface
//! that carries one is the same shape of leak `NodeConfig::redact_credentials` already exists to
//! close for vault key names — server-side, because client-side masking alone does not protect the
//! raw API (an operator using `curl` directly sees exactly what the response carries).
//!
//! Product spec (Morgan, via PM): first two characters, a fixed three-dot mask, last two characters,
//! for anything shaped like `local@domain`; the domain is never masked. A non-email identifier (no
//! `@`) gets the identical rule applied to the whole string, since a principal that happens to BE an
//! email under `jwks` and one that is not are the same field wearing two different shapes.

/// The mask, always this exact width — a mask sized to the hidden part's length would leak that
/// length, which is most of what masking exists to hide in the first place.
const MASK: &str = "•••";

/// Mask a member identifier for a guest-tier caller.
///
/// `so•••en@example.com`, `a•••@x.com` (local part ≤ 4 chars gets one leading character, not two —
/// `ab@x.com` and `abcd@x.com` are indistinguishable once masked, which is the point: a two-character
/// local part revealing BOTH its characters is not meaningfully masked at all), `a•••b` for a non-email
/// identifier.
///
/// Counts **Unicode scalar values**, not bytes — slicing on byte offsets could split a multi-byte
/// character and either panic or produce a mangled string, and "first two characters" means what a
/// person reading it would call the first two characters, not the first two bytes of its UTF-8
/// encoding.
pub fn mask_identifier(identifier: &str) -> String {
    match identifier.split_once('@') {
        Some((local, domain)) => format!("{}@{domain}", mask_part(local)),
        None => mask_part(identifier),
    }
}

/// Mask one string in isolation — the email local part, or a whole non-email identifier.
fn mask_part(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    match chars.len() {
        0 => MASK.to_string(),
        1..=4 => format!("{}{MASK}", chars[0]),
        n => {
            let head: String = chars[..2].iter().collect();
            let tail: String = chars[n - 2..].iter().collect();
            format!("{head}{MASK}{tail}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typical_email_shows_two_and_two_with_the_full_domain() {
        // "someone" is s-o-m-e-o-n-e: the first two characters are "so", the LAST two are "ne" —
        // not "en". Worth a comment because it is easy to eyeball this wrong once, the same way
        // this test's own first draft did.
        assert_eq!(
            mask_identifier("someone@example.com"),
            "so•••ne@example.com"
        );
    }

    #[test]
    fn a_subdomain_is_never_masked() {
        assert_eq!(
            mask_identifier("alice@mail.corp.example.com"),
            "al•••ce@mail.corp.example.com"
        );
    }

    #[test]
    fn a_local_part_at_the_four_char_boundary_shows_only_the_first_character() {
        // Both collapse to the identical masked form: showing two distinct-length results here
        // would leak exactly the length difference masking exists to hide.
        assert_eq!(mask_identifier("ab@x.com"), "a•••@x.com");
        assert_eq!(mask_identifier("abcd@x.com"), "a•••@x.com");
    }

    #[test]
    fn a_five_char_local_part_crosses_into_the_normal_two_and_two_rule() {
        assert_eq!(mask_identifier("abcde@x.com"), "ab•••de@x.com");
    }

    #[test]
    fn a_single_character_local_part_still_shows_just_that_one_character() {
        assert_eq!(mask_identifier("a@x.com"), "a•••@x.com");
    }

    #[test]
    fn a_non_email_identifier_masks_the_whole_string_by_the_same_rule() {
        // A local-auth uuid principal: no `@`, so the whole string is the "local part".
        assert_eq!(
            mask_identifier("3f2504e0-4f89-11d3-9a0c-0305e82c3301"),
            "3f•••01"
        );
    }

    #[test]
    fn a_short_non_email_identifier_uses_the_short_rule_too() {
        assert_eq!(mask_identifier("abc"), "a•••");
    }

    #[test]
    fn unicode_is_counted_in_code_points_not_bytes_and_never_split() {
        // Each of these is a single multi-byte scalar value; byte-slicing would panic or corrupt
        // the string mid-character. 6 code points total, so the 5+ char rule applies: first two
        // and last two characters, not the first/last two BYTES of a multi-byte encoding.
        let identifier = "日本語ですね@example.com";
        let masked = mask_identifier(identifier);
        assert_eq!(masked, "日本•••すね@example.com");
        // The strongest possible check: confirms the visible character COUNT is exactly right
        // (2 head + 2 tail, not some byte-offset-derived count that happened to also compile).
        assert_eq!(masked.chars().filter(|c| *c != '•').count(), 4 + 12);
    }

    #[test]
    fn an_empty_local_part_does_not_panic() {
        assert_eq!(mask_identifier("@example.com"), "•••@example.com");
    }

    #[test]
    fn an_empty_identifier_does_not_panic() {
        assert_eq!(mask_identifier(""), "•••");
    }

    #[test]
    fn masking_is_never_reversible_by_construction() {
        // Not a cryptographic claim -- just confirming the middle is a constant, not a function of
        // the hidden characters, so two different identifiers with the same visible head/tail are
        // indistinguishable once masked.
        assert_eq!(
            mask_identifier("aaaaa@x.com"),
            mask_identifier("aabaa@x.com")
        );
    }
}
