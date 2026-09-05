#!/usr/bin/env python3
"""002/§4 Exit-code confusion primitive. Owner: SDK.

QA's fake reproduces the real CLI refusing `--permission-mode bypassPermissions` under uid 0:
WHEEL_FAKE_ROOT=1 -> exit 1, EMPTY stdout — byte-for-byte indistinguishable from "not logged in" by
exit code alone. Contract §4: `needs_auth` is NEVER inferred from the exit code, only from stderr / the
auth probe. If the engine classifies on exit code, an attacker (or any crash) can force a spurious
`needs_auth` (masking a real fault as "please log in", or vice-versa) — a state-confusion primitive.

Secure outcome: with WHEEL_FAKE_ROOT=1 and NO auth string on stderr, the agent's state must be
`error` (with last_error about the root refusal), NOT `needs_auth`.
"""
import json, os, sys
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "lib"))
import harness as h

def run(_):
    tok, pid, aid = (os.environ.get(k) for k in ("WHEEL_TOKEN_A", "WHEEL_PROJECT_A", "WHEEL_AGENT_A"))
    if not (tok and pid and aid):
        return None
    # TODO(stack): start `aid` on wheel-engine:test with WHEEL_FAKE_ROOT=1 (no WHEEL_FAKE_AUTH), wait.
    base = f"/v1/projects/{pid}/engine/v1"
    st, _, body = h.req("GET", f"{base}/agents/{aid}/auth", token=tok, project=pid)
    st2, _, board = h.req("GET", f"{base}/board", token=tok, project=pid)
    if st2 != 200:
        return None
    node = next((n for n in json.loads(board).get("nodes", []) if n.get("id") == aid), None)
    status = ((node or {}).get("state") or {}).get("status")
    if status == "needs_auth":
        return "engine inferred needs_auth from exit 1 + empty stdout (root refusal) — classified on exit code, not stderr/probe"
    return None

if __name__ == "__main__":
    h.finish(run)
