# 058 — `?token=` query-param bearer auth compares the raw, still-percent-encoded value

- **Severity:** Low. Narrow footgun, not an authentication bypass — it fails *closed* (a
  correctly percent-encoded secret containing a reserved byte never matches, so the caller
  gets 401, not unauthorized access). Found incidentally while reviewing PR #103
  (`sdk/ingress-bearer-auth-diagnostics`) for the live `EndpointAuth::Bearer` failure —
  distinct from that bug and not what is causing it.
- **Owner:** SDK (`crates/wheel-engine/src/api/ingress.rs::authenticate`).
- **Status:** OPEN, reported to sdk; sdk to decide whether to fix inline in #103 or as its
  own follow-up.

## The gap
`authenticate()`'s query-param fallback for a presented Bearer credential:

```rust
uri.query().and_then(|q| {
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == "token")
        .map(|(_, v)| v.to_string())
})
```

takes `v` straight from the raw query string with no percent-decoding. The three header-based
forms (`Authorization: Bearer`, `x-telegram-bot-api-secret-token`, `x-wheel-secret`) don't have
this problem — HTTP header values aren't percent-encoded, so what's on the wire is what gets
compared. The query string is the one path where a spec-compliant caller is *expected* to
percent-encode reserved bytes, and this code never undoes that before `constant_time_eq`.

## Concrete failure
A vault secret containing any of `%`, `&`, `+`, `=`, `#`, `/`, or a non-ASCII byte, presented
via `?token=<value>` by a caller that correctly percent-encodes it per RFC 3986, will never
authenticate: `constant_time_eq(percent_encoded_bytes, raw_secret_bytes)` compares two
different byte strings. A caller that sends the RAW unencoded bytes (skipping encoding) would
happen to match today — which is also backwards: the code currently rewards the caller for
*not* following the URL spec.

## Impact
Low — this is a usability/correctness bug in the query-token convenience path, not a security
hole: no bypass, no weakening of the comparison, just spurious 401s for secrets containing
reserved characters when a spec-correct caller uses them. It's also the least-used of the four
presented forms (header-based auth is the documented path for real providers per §3d/§4 of the
contract; `?token=` exists for senders that can only be given a URL). Worth fixing because a
secret's byte content is arbitrary (vault values are unconstrained free text) and an operator
has no way to know in advance that a `%`-containing secret will silently fail this one path.

## Recommendation
Percent-decode the extracted `v` before the comparison (`percent_encoding::percent_decode_str`
or equivalent already available if the crate is in the tree; otherwise a minimal decoder is
~10 lines). Add a test with a secret containing `%`, `&`, and `+` presented via `?token=`
correctly percent-encoded, asserting it authenticates. No change needed to the header-based
paths.

## What would change my mind
If the query-token path is documented/intended as "raw bytes only, do not percent-encode" (i.e.
this is a deliberate simplification rather than an oversight), this is a documentation gap, not
a bug — downgrade to a one-line note in PROTOCOL.md instead of a code fix. I did not find any
such documentation in PROTOCOL.md or the ingress code comments, so I'm treating it as unintended.
