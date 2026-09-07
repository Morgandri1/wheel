# Cross-tenant network-egress probe (prod, operator-approved)

Places a throwaway project + script node that does SINGLE read-only connects from inside the
sandbox to internal Railway names (postgres.railway.internal:5432, wheel-host.railway.internal:7100,
wheel-api.railway.internal), writing results to a table read back via the API. Real metadata IP
EXCLUDED per RoE. Bounded, no DoS, throwaway project torn down after.

STATUS (build 0b81bc0, 2026-09-07): COULD NOT COMPLETE — script EXECUTION is not implemented in
the engine on this build (no run_script/python spawn; endpoint->script queues the delivery but
nothing executes it), and the agent path needs vault credentials the throwaway account lacks. So a
sandbox cannot be driven to make the connects via the public API yet. NO data returned from any
target (no incident). This probe becomes runnable when script execution lands (M2).

ARCHITECTURAL ANSWER meanwhile (source + ops): §5b runs wheel-host in its OWN Railway project,
separate private network from Postgres/API, so postgres.railway.internal / wheel-api.railway.internal
resolve only in the OTHER project's network and are unreachable from a sandbox on the host's network.
infra/prune-probe-projects.railway.sh states this directly ("resolve only inside the project's
network, and no container there has both a database client and an HTTP client"). The host's own
:7100 is bearer-gated. The tool-executor SSRF policy (host_is_denied) also blocks *.railway.internal.
Fill TOK with a throwaway session token to run once script execution exists.

## Why this is gate 2 for script execution (PM, b7a764c)
A raw Python script's sockets are constrained by NOTHING: the SSRF denylist (host_is_denied / resolve-and-pin /
ip_is_denied — verified sound in 045 and the SSRF verdict) guards ONLY tool-node and mcp URLs; it does not sit
in the path of a script's `socket.connect`. So on the co-located deployment (finding 048) a script would have
unconstrained raw reachability to postgres:5432 and wheel-api:8080 — guarded by neither the (tool/mcp-only)
SSRF policy nor the (absent, 048) network segmentation. Hence: this probe must go GREEN (raw agent/script
egress to the internal targets denied on the real topology) before script execution ships. 037/038
(impersonation) are gate 1; this is gate 2.
