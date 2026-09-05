# 011 — Established WS/proxy bridge has no idle timeout, lifetime cap, or per-project connection cap

- **Severity:** Medium (availability; cross-tenant on the shared-kernel host)
- **Owner:** API (wheel-api + wheel-host bridges)
- **Status:** OPEN — API disclosed it while fixing the handshake bound (finding 010 obs #2). Not yet PoC'd
  (needs repointing the upstream, outside my read-only scope; or the real engine's events WS).
- **Boundary:** TB2 (proxy) → TB6/TB7 (shared host: one tenant's connections affect all).

## Claim (API's own honest disclosure, escalated to a tracked finding)
The 10 s handshake ceiling (010 #2) bounds only the *handshake*. Once a bridge is established, a peer
that completes the WS handshake and then goes silent holds the connection with **no idle timeout and no
total-lifetime cap**, and there is **no per-project cap on concurrent bridges/connections**. On the prod
`process` backend every tenant shares one host machine and one file-descriptor / memory budget, so one
project holding many silent-but-established bridges is a cross-tenant availability problem, not just a
self-inflicted one. The events WS is long-lived by design, which is exactly why "silent" can't be
distinguished from "idle-but-legitimate" without an explicit keepalive.

## Recommended controls + milestone (my answer to API's question)
- **Per-project concurrent-connection cap (M2).** A modest env default (e.g. 16 live bridges/project)
  with new connections beyond it refused (429/503). This is the cross-tenant control — cheap, and it
  caps the blast radius regardless of the idle/lifetime story. Highest priority of the three.
- **WS keepalive with a pong deadline (M2).** Server-side ping every N s; no pong within the deadline →
  close. Distinguishes a dead/silent peer from a legitimately-idle one on a long-lived push channel,
  which a plain idle-read timeout cannot.
- **Total-lifetime cap (M3).** A generous absolute ceiling (e.g. hours) as defence-in-depth; refresh via
  a new ws-ticket. Lower priority once the cap + keepalive exist.

## On the 502-vs-504 question (010 #2)
Security-neutral — leave it as **502**. It is the honest result (the inner hop's 504 is not a valid WS
handshake, so the outer connect correctly rejects it as bad gateway). Don't invent a status-translation
rule for its own sake; a 504 would only add operator-debugging clarity, and that's your call to make,
not a security requirement.

## Verify
When the cap + keepalive land: open cap+1 bridges from one project (expect the last refused), and hold
an established bridge silent past the pong deadline (expect close). PoC will live in
`redteam/pocs/proxy-ingress/` once the events WS is reachable (real engine or a WS-capable stub).
