# Auth-model TOS/compliance risk — subscription OAuth tokens vs the API (PM risk-flag, 2026-09-07)

NOT legal advice and NOT a TOS ruling — a risk the operator must verify against current Anthropic terms and
ideally with Anthropic directly. Raised by the operator: "are we violating Anthropic's TOS by storing oauth
tokens?" Recorded so it is a tracked decision, not discovered at launch.

## The concern
`CLAUDE_CODE_OAUTH_TOKEN` (stored in the vault, used to authenticate the board's agents) is a SUBSCRIPTION
credential (Claude Pro/Max), not the API. Anthropic's Consumer Terms + Usage Policy generally scope
subscription credentials to the individual account holder's own use, and tend to restrict:
- credential storage/sharing — storing a user's OAuth token and using it programmatically;
- automated/programmatic use of a subscription — an always-on, many-agent cloud workload is not interactive
  Claude Code use;
- reselling/providing capacity — a hosted product running agents on users' subscriptions.

The SANCTIONED path for programmatic / hosted / multi-user Claude usage is the Anthropic API (API keys,
commercial terms, metered billing), not subscription OAuth.

## Risk gradient
- TODAY (operator's own token, own agents): lowest-risk end, but "automated, always-on, multi-agent" may still
  stretch subscription terms.
- AS THE PRODUCT ("agents authenticated by the user", each user's token stored + used): higher-risk end —
  storing others' credentials + running automated workloads on their subscriptions.

## Recommendation
1. Plan the product's auth model around the Anthropic API (API keys), not subscription OAuth. Support OAuth
   only for the narrow single-user-own-token case, if at all.
2. Verify against the CURRENT Claude Code + Anthropic Commercial Terms + Usage Policy, and ask Anthropic
   directly before shipping anything that stores users' OAuth tokens. This flag is not the ruling.
3. Treat stored OAuth tokens as a SECURITY liability regardless of TOS — a vault breach exposes users' Claude
   accounts. Same instinct as the no-PAT-on-disk / credential-handling work.

## Status
OPEN — product/legal decision, operator's. Not an engineering fix. Blocks shipping OAuth-token storage as a
product until verified. Does not block the current single-user proof work.
