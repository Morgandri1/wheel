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

## UPDATE (operator, 2026-09-07): non-commercial, single-user — risk DOWNGRADED to low/accepted
Operator: "this isn't supposed to be a commercial product, i'm not that worried about TOS violations."
The high-risk patterns (hosted product, storing OTHERS' tokens, reselling capacity) do not apply to a
personal, single-user, own-token setup — that is the lowest-risk end of the gradient above. Residual is only
"automated always-on use of one's own subscription," which is minor for personal use. Status: ACCEPTED for
non-commercial single-user use; does NOT block the cloud-board work. The flag RE-ARMS if Wheel ever becomes
commercial or stores other users' tokens — at that point the API auth-model recommendation applies and must be
verified with Anthropic before shipping.

## FINAL (operator, 2026-09-07): risk consciously accepted, proceed on OAuth for the dogfood
Operator: "if my personal account gets banned it's not the end of the world, i'll make another." This is an
informed acceptance of the residual risk INCLUDING the account-ban consequence, for the non-commercial
single-user dogfood. Decision: PROCEED with the subscription-OAuth auth model for the personal dogfood; PM
stops flagging it. The API-auth recommendation and the re-arm condition (commercial / multi-user / storing
others' tokens) still stand for any future productization — this acceptance is scoped to the operator's own
account and own use.
