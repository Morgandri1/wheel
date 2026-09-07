# 050 — create-only protects a node's CONFIG but not its WIRE GRAPH: an import can wire an existing node without consent

- **Severity:** Medium (untrusted-board integrity/privilege; the sharpest instance is a permanent prompt
  injection of pm). Owner: API (`crates/wheel-api/src/apply.rs`, the apply layer). Boundary TB1 (import) →
  existing-node capabilities. This is the seam API flagged themselves ("wiring TO an existing node still needs no
  flag … if you disagree, that is the seam") — I disagree, with the concrete escalation below.
- **Status:** Source-confirmed. create-only (`allow_patch`) gates `patch_nodes` (apply.rs:300-307); `create_wires`
  (apply.rs:317-327) filters only DUPLICATE wires — it does NOT check whether a wire's endpoint is an existing
  node. So a wire touching an existing node, in either direction, is created with no flag.

## Why "wiring is not modifying" is false for the thing that matters
It is true for a node's CONFIG (a wire does not change system_prompt/budget/etc.), which is what create-only
protects. But a WIRE IS A CAPABILITY — it is the entire security-relevant fact about a node (§3: "a node's wire
set is its capability set"). Adding a wire whose endpoint is an existing node changes THAT node's capability
graph / attack surface without touching its config, so create-only lets it through. An import with NO
`allow_patch` can:
- **Permanently inject an existing agent (the sharp one):** create a NEW `ctx` "evil" (a create, not a patch)
  and a wire `evil → pm (send)`. `ctx→agent send` = INJECTION — evil's markdown is prepended to pm's prompt on
  every start. pm's config is untouched (create-only satisfied), but pm is now permanently prompt-injected by
  attacker-controlled ctx. This is the most-capable agent on the board, injected via a create-only import.
- **Message-inject an existing agent:** create a new `agent` "evil" + `evil → pm (send)` → evil can deliver
  messages into pm's inbox (`type=agent`).
- **Grant an existing agent new secret access:** wire `pm → vault (read)` — to a board-supplied new vault, or
  to an EXISTING vault. pm now reads that vault's secrets (and gets its keys exported to env at spawn). A new
  outbound wire on an existing node = a new capability for it, again with no patch.
- **Open an internet→existing-agent channel:** create an `endpoint` (auth:none) + `endpoint → pm (send)` →
  ties to 043: unauthenticated ingress delivered into pm, added by a create-only import.
The engine's `add_wire` re-validates the MATRIX (real types) and the vault-ambiguity rule, so only
matrix-legal wires land — but the matrix permits all of the above (ctx→agent send, agent→agent send,
agent→vault read, endpoint→agent send). The engine does NOT check "did this board have consent to touch this
existing node," because per-node consent is exactly what create-only was meant to add — and it only added it
for config.

## Impact
create-only stops an untrusted board (template gallery, "JSON an LLM produced") from rewriting pm's prompt via
a config patch (049 #1, fixed) — but it does NOT stop the SAME board from injecting pm via `ctx→pm send`,
messaging pm, or granting an existing agent a vault. The protection has a wire-shaped hole the same size as the
config protection it complements. An owner importing a template believes create-only guards their existing
nodes; it guards half of what makes them dangerous.

## Fix (API)
Treat a wire whose endpoint (from OR to) is an EXISTING node as a MODIFICATION of that node, and gate it behind
`allow_patch` (or a distinct `allow_wire` consent, surfaced the same way — "this import will wire these existing
nodes: [list]"). Only a wire BETWEEN two newly-created nodes is create-only-free. Both directions matter:
`to`=existing adds an inbound channel (inject/message/ingress the node); `from`=existing grants the node a new
outbound capability (read a vault, manage/msg a peer). SDK's principle — "the builder named it is not consent"
— applies identically to "the builder wired it": naming an existing node in a wire is not consent to change
what it can do or what can reach it.

## Note (what is SOUND)
049 #1 (config patch) and #2 (type-mismatch → `NodeTypeMismatch` refusal, existing-wins) are correctly fixed;
the engine `add_wire`/`patch_node` backstops hold; 422-before-create and 207-not-200 are right; the
pre-validation-duplicates-engine-authority posture (reported-not-hidden) is the correct call — the engine is the
real gate. This finding is only that the create-only CONSENT boundary stops at config and must extend to any
wire that touches an existing node.
