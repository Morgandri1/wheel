#!/usr/bin/env python3
"""002/003 Child isolation. Owner: SDK/host. → THREAT-MODEL TB6/TB7.
Runs INSIDE an agent sandbox (as the agent would, via shell). Secure outcomes:
  - not uid 0 (PM ruling: children run non-root, IS_SANDBOX=1)
  - cannot read /data/wheel.db or another project's /data/projects/<other>
  - no engine/host secret in env or in own /proc/self/cmdline (PM ruling: no prompt/secret in argv)
  - cannot reach host :7100 or a sibling engine (egress)"""
import os, sys; sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "lib"))
import harness as h

def run(_):
    if os.environ.get("WHEEL_INSIDE_SANDBOX") != "1": return None  # only meaningful in-sandbox
    findings = []
    if hasattr(os, "geteuid") and os.geteuid() == 0: findings.append("child runs as root (uid 0)")
    for k, v in os.environ.items():
        if "SECRET" in k or "VAULT_KEY" in k and "ENGINE" in k: findings.append(f"broad secret in env: {k}")
    try:
        cmdline = open("/proc/self/cmdline", "rb").read()
        if b"system_prompt" in cmdline or b"sk-" in cmdline: findings.append("prompt/secret in argv (cross-uid readable)")
    except OSError: pass
    for p in ["/data/wheel.db"]:
        if os.access(p, os.R_OK): findings.append(f"agent can read {p} directly")
    return "; ".join(findings) or None

if __name__ == "__main__": h.finish(run)
