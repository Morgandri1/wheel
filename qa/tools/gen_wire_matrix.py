#!/usr/bin/env python3
"""Expand the wire-matrix contract into all 9x9x3 = 243 cells.

SINGLE SOURCE OF TRUTH for wire authorisation expectations. docs/TESTPLAN.md and the
integration suite both consume qa/fixtures/wire_matrix.json, so the doc and the tests
cannot drift. Regenerate with:  python3 qa/tools/gen_wire_matrix.py --write

Derived line-by-line from ARCHITECTURE.md §3 "Wire semantics matrix". Default is DENY:
anything not explicitly listed as allowed must be rejected at creation time by BOTH the
API and the engine.
"""
import json, os, sys

NODE_TYPES = ["agent", "ctx", "table", "endpoint", "script", "mcp", "vault", "chest", "tool"]
WIRE_TYPES = ["read", "write", "send"]

# (from, to, type) -> why it is allowed. Everything else is denied.
ALLOWED = {
    ("agent", "agent", "send"):   "wheel msg <agent> delivers into the target's inbox",
    ("agent", "ctx", "read"):     "wheel read <ctx> returns the markdown",
    ("agent", "ctx", "write"):    "wheel write <ctx> --file f.md",
    ("agent", "table", "read"):   "wheel table query <t> '<SELECT...>' (read-only)",
    ("agent", "table", "write"):  "INSERT/UPDATE/DELETE; write implies read",
    ("agent", "vault", "read"):   "keys exported as env at spawn + wheel secret get",
    ("agent", "chest", "read"):   "wheel chest get|ls",
    ("agent", "chest", "write"):  "wheel chest put|rm; write implies read",
    ("agent", "script", "read"):  "wheel run <script> [args...]",
    ("agent", "mcp", "read"):     "MCP server attached to the harness config at next start",
    ("agent", "tool", "read"):    "wheel tool ls/call; enabled ops also exposed as MCP <tool>__<op>",
    ("ctx", "agent", "send"):     "INJECTION: ctx markdown prepended to the agent's prompt",
    ("endpoint", "agent", "send"):  "each HTTP hit delivered as a message",
    ("endpoint", "table", "write"): "JSON body inserted as a row",
    ("endpoint", "script", "send"): "script invoked with the request",
    ("script", "agent", "send"):  "wheel msg from inside the script (token scoped to ITS wires)",
    ("script", "ctx", "read"):    "same as agent",
    ("script", "ctx", "write"):   "same as agent",
    ("script", "table", "read"):  "same as agent",
    ("script", "table", "write"): "same as agent",
    ("script", "vault", "read"):  "same as agent",
    ("script", "chest", "read"):  "same as agent",
    ("script", "chest", "write"): "same as agent",
    ("script", "tool", "read"):   "same as agent",
    ("tool", "vault", "read"):    "tool resolves {mode:vault} fills from that vault at call time",
}

# Why a whole row is empty - used to give denied cells a readable rationale.
NO_OUTGOING = {
    "ctx":   "ctx has no outgoing wires except send->agent (injection)",
    "table": "table has no outgoing wires",
    "vault": "vault has no outgoing wires",
    "chest": "chest has no outgoing wires",
    "mcp":   "mcp has no outgoing wires",
}

TOOL_ONLY = "tool's only outgoing wire is read->vault"

def deny_reason(f, t, w):
    if f == "tool":
        return TOOL_ONLY
    if f in NO_OUTGOING:
        return NO_OUTGOING[f]
    if f == "endpoint":
        return "endpoint may only send->agent, send->script, write->table"
    if w == "send" and t not in ("agent", "script"):
        return "send is only meaningful into agent (and endpoint->script)"
    if f == t == "agent" and w in ("read", "write"):
        return "agents do not read/write each other; they message"
    return "not listed in the matrix; default DENY"

def build():
    cells = []
    for f in NODE_TYPES:
        for t in NODE_TYPES:
            for w in WIRE_TYPES:
                allowed = (f, t, w) in ALLOWED
                cells.append({
                    "id": "WM-%s-%s-%s" % (f, t, w),
                    "from": f, "to": t, "type": w,
                    "expect": "allow" if allowed else "deny",
                    "why": ALLOWED[(f, t, w)] if allowed else deny_reason(f, t, w),
                })
    return cells

def main():
    cells = build()
    n_allow = sum(1 for c in cells if c["expect"] == "allow")
    assert len(cells) == 243, len(cells)
    assert len(set(c["id"] for c in cells)) == 243, "duplicate ids"
    assert n_allow == len(ALLOWED), (n_allow, len(ALLOWED))

    root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..")
    out = os.path.normpath(os.path.join(root, "qa/fixtures/wire_matrix.json"))
    payload = {
        "_generated_by": "qa/tools/gen_wire_matrix.py — do not hand-edit",
        "_source": "docs/ARCHITECTURE.md §3 wire semantics matrix",
        "node_types": NODE_TYPES, "wire_types": WIRE_TYPES,
        "total": len(cells), "allow": n_allow, "deny": len(cells) - n_allow,
        "cells": cells,
    }
    if "--write" in sys.argv:
        os.makedirs(os.path.dirname(out), exist_ok=True)
        with open(out, "w") as fh:
            json.dump(payload, fh, indent=2)
            fh.write("\n")
        print("wrote %s (%d cells: %d allow / %d deny)" % (out, len(cells), n_allow, len(cells) - n_allow))
    elif "--check" in sys.argv:
        # make check: fail if the committed fixture has drifted from the contract.
        with open(out) as fh:
            have = json.load(fh)
        if have.get("cells") != cells:
            print("wire_matrix.json is STALE — run: python3 qa/tools/gen_wire_matrix.py --write")
            return 1
        print("wire matrix: %d cells, %d allow / %d deny — in sync" % (len(cells), n_allow, len(cells) - n_allow))
    elif "--markdown" in sys.argv:
        print("| from \\ to | " + " | ".join(NODE_TYPES) + " |")
        print("|---" * (len(NODE_TYPES) + 1) + "|")
        for f in NODE_TYPES:
            row = []
            for t in NODE_TYPES:
                ok = [w for w in WIRE_TYPES if (f, t, w) in ALLOWED]
                row.append(", ".join(ok) if ok else "—")
            print("| **%s** | %s |" % (f, " | ".join(row)))
    else:
        print(__doc__)
    return 0

if __name__ == "__main__":
    sys.exit(main())
