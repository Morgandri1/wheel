#!/usr/bin/env python3
"""Contract test: the engine's documented routes must match ARCHITECTURE.md §4.

TESTPLAN: ENG-route-exists, ENG-route-undocumented, ENG-route-parity.

Three layers, in order of what's available:
  1. CONTRACT vs PROTOCOL (always runs)  — §4's route table vs docs/PROTOCOL.md.
  2. PROTOCOL vs LIVE ENGINE (WHEEL_ENGINE_URL set) — probe each documented route and
     distinguish 404 (route absent) from 405 (route present, wrong method). A documented
     route that 404s is a bug in the code or the doc; either way it's a bug.
  3. LIVE ENGINE vs PROTOCOL — an undocumented route is undocumented attack surface.

Layer 1 runs today with no engine, and catches divergence between the two documents
before anyone writes a line of code against the wrong one.
"""
import os, re, sys

SKIP = 77
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
ARCH = os.path.join(ROOT, "docs", "ARCHITECTURE.md")
PROTO = os.path.join(ROOT, "docs", "PROTOCOL.md")

METHODS = "GET|POST|PUT|PATCH|DELETE|ANY"
ROUTE_RE = re.compile(r"\b(%s)\s+`?(/[A-Za-z0-9_:/*\-{}.]+(?:\|[A-Za-z0-9_\-]+)*)" % METHODS)

# Routes the contract defines but PROTOCOL.md v1 legitimately defers to a later milestone.
DEFERRED = {
    # Every /v1/tools/* entry that used to live here has been retired: PROTOCOL.md now
    # documents them, and the marker expires by BREAKING rather than by being remembered.
    ("ANY", "/ingress/*"): "M2 — ingress (§2)",
}

def extract(path, section=None):
    txt = open(path).read()
    if section:
        i = txt.find(section)
        if i < 0:
            return None
        j = txt.find("\n## ", i + 1)
        txt = txt[i:j if j > 0 else len(txt)]
    found = set()
    for m in ROUTE_RE.finditer(txt):
        method, raw = m.group(1), m.group(2).rstrip(".,`")
        raw = raw.split("?")[0]
        # `/v1/agents/:id/start|stop|restart` -> three routes
        if "|" in raw:
            head, _, alts = raw.partition("|")
            base = head.rsplit("/", 1)[0]
            found.add((method, head))
            for a in alts.split("|"):
                found.add((method, "%s/%s" % (base, a)))
        else:
            found.add((method, raw))
    return found


# ---------------------------------------------------------------------------
# 4. HANDLERS vs ROUTER vs PROTOCOL  (ADVERSARY 025, PM 2026-09-06)
#
# The three checks above all start from a documented or served route, so they
# are blind in one direction: a handler that exists, compiles, and is wired to
# NOTHING is invisible to every one of them. That is exactly how the engine's
# tool_ls/tool_call CLI handlers sat unreachable -- undocumented, so check 1
# could not miss them; unrouted, so checks 2 and 3 never saw them. ADVERSARY
# found it by reading the source, which is not a gate.
#
# So: walk the handlers themselves. Every handler must be reachable through a
# registered route AND its path must be in PROTOCOL.md.
# ---------------------------------------------------------------------------
API_DIR = os.path.join(ROOT, "crates", "wheel-engine", "src", "api")
ROUTER = os.path.join(API_DIR, "mod.rs")

# A handler is an async fn that takes an axum EXTRACTOR. `run_operation` in
# tool_routes.rs is `pub async fn` too, and is a shared helper called by both the
# operator route and the CLI path -- it takes &AppState, not State(s): State<_>.
# Distinguishing structurally beats an allowlist of known non-handlers, which
# would need a human to remember to update it, which is the failure mode here.
EXTRACTOR = re.compile(
    r"\b(?:State|Path|Json|Query|Extension|Form|Multipart)\s*\(|"
    r"\bHeaderMap\b|\bWebSocketUpgrade\b|\bRequest\s*<"
)
HANDLER_DEF = re.compile(r"^pub(?:\(crate\))?\s+async\s+fn\s+(\w+)\s*\(", re.M)


def rust_handlers():
    """{module: {fn_name}} for every handler defined under src/api/."""
    out = {}
    if not os.path.isdir(API_DIR):
        return out
    for name in sorted(os.listdir(API_DIR)):
        if not name.endswith(".rs") or name == "mod.rs":
            continue
        module = name[:-3]
        src = open(os.path.join(API_DIR, name)).read()
        for m in HANDLER_DEF.finditer(src):
            # Signature runs to the closing paren of the parameter list.
            tail = src[m.end():m.end() + 600]
            sig = tail.split(") ->")[0] if ") ->" in tail else tail
            if EXTRACTOR.search(sig):
                out.setdefault(module, set()).add(m.group(1))
    return out


def routed_handlers(router_src):
    """{(module, fn)} referenced anywhere in the router, and fn -> path."""
    refs = set(re.findall(r"\b(\w+_routes?)::(\w+)\b", router_src))
    paths = {}
    for m in re.finditer(r'\.route\(\s*"([^"]+)"\s*,(.*?)\)\s*(?=\.route\(|;|\.)',
                         router_src, re.S):
        path, body = m.group(1), m.group(2)
        for mod_, fn in re.findall(r"\b(\w+_routes?)::(\w+)\b", body):
            paths.setdefault((mod_, fn), path)
    return refs, paths


def mount_prefixes(router_src):
    """Every prefix the engine nests a sub-router under, read from the source.

    `.nest("/v1/cli", cli)` means cli_routes::whoami is served at /v1/cli/whoami, not
    /v1/whoami. Hardcoding "/v1" reported all eight CLI routes as undocumented. Reading
    the mounts means a future `.nest("/v2", ...)` is handled without anyone remembering
    to update this file -- and if a mount is REMOVED, the routes under it stop resolving
    and this check goes red, which is the correct direction.
    """
    found = re.findall(r'\.nest\(\s*"([^"]+)"', router_src)
    # Longest first, so /v1/cli wins over /v1 for a path both could prefix.
    return sorted(set(found + [""]), key=len, reverse=True)


MOUNTS = [""]


def documented(path, proto_text):
    """Is this router path written down in PROTOCOL.md?

    The router and the document spell the same route differently: axum uses
    `/vault/{id}/{key}` and is mounted under `/v1`, while PROTOCOL.md writes
    `PUT /v1/vault/:id/:key`. Comparing the raw strings reported fifteen documented
    routes as undocumented -- a gate that cries wolf fifteen times is one nobody reads
    the sixteenth time, which would have buried the three real orphans below it.

    Parameter NAMES are deliberately ignored: `{id}` and `:node` name the same hole, and
    forcing them to agree would fail on a rename that changes nothing a caller can see.
    """
    variants = {path} | {prefix + path for prefix in MOUNTS}
    out = set()
    for v in variants:
        out.add(v)
        out.add(re.sub(r"\{(\w+)\}", r":\1", v))          # {id}  -> :id
        out.add(re.sub(r"\{(\w+)\}", "<PARAM>", v))        # name-insensitive form
    if any(v in proto_text for v in out):
        return True
    # Last resort: compare shape with parameter names blanked on both sides.
    shapes = [re.sub(r"[:{]\w+\}?", "<PARAM>", pfx + path) for pfx in MOUNTS]
    blanked = re.sub(r"[:{]\w+\}?", "<PARAM>", proto_text)
    return any(sh in blanked for sh in shapes)


def check_handlers(proto_text):
    fails = []
    defined = rust_handlers()
    if not defined:
        return ["no handlers found under crates/wheel-engine/src/api — this check just "
                "stopped checking anything; has the layout moved?"]
    router = open(ROUTER).read() if os.path.exists(ROUTER) else ""
    if not router:
        return ["crates/wheel-engine/src/api/mod.rs not found — cannot tell routed from orphaned"]
    global MOUNTS
    MOUNTS = mount_prefixes(router)
    refs, paths = routed_handlers(router)

    total = 0
    for module, fns in sorted(defined.items()):
        for fn in sorted(fns):
            total += 1
            if (module, fn) not in refs:
                fails.append(
                    "%s::%s is a handler wired to NOTHING — it compiles, it is dead, and "
                    "no route-vs-doc check can see it. Register it in api/mod.rs or delete it."
                    % (module, fn))
                continue
            path = paths.get((module, fn))
            if path and not documented(path, proto_text):
                fails.append(
                    "%s::%s serves %s, which is NOT in PROTOCOL.md — served surface nobody "
                    "documented is surface nobody reviewed." % (module, fn, path))
    print("handlers: %d checked across %d module(s)" % (total, len(defined)))
    return fails


def main():
    if not os.path.exists(PROTO):
        print("docs/PROTOCOL.md not written yet (SDK)")
        return SKIP

    contract = extract(ARCH, "## 4. Engine control plane")
    if contract is None:
        print("could not find '## 4. Engine control plane' in ARCHITECTURE.md — "
              "the section was renamed; QA needs to re-pin this test")
        return 1
    proto = extract(PROTO)

    fails, gaps, notes = [], [], []

    proto_txt = open(PROTO).read()
    for r in sorted(contract - proto):
        why = DEFERRED.get(r)
        if why:
            gaps.append("%s %s — in the contract, not yet in PROTOCOL.md (%s)" % (r[0], r[1], why))
        elif r[1].rstrip("*") in proto_txt:
            # The path is referenced (auth table, prose) but has no method-level entry.
            # Not absent, but not specified either — a caller cannot implement from this.
            gaps.append("%s %s — mentioned in PROTOCOL.md but with no request/response "
                        "documentation" % (r[0], r[1]))
        else:
            fails.append("%s %s is in ARCHITECTURE.md §4 but NOT documented in PROTOCOL.md"
                         % (r[0], r[1]))

    for r in sorted(proto - contract):
        # PROTOCOL.md adding detail the contract summarised is fine and expected; it is
        # only worth noting, not failing. Undocumented LIVE routes are the real risk, and
        # that is layer 3 below.
        notes.append("%s %s documented in PROTOCOL.md, not listed in ARCHITECTURE.md §4" % r)

    for r in sorted(DEFERRED):
        if r in proto:
            fails.append("%s %s is marked DEFERRED but is now documented — remove it from "
                         "DEFERRED in this test" % r)

    print("ARCHITECTURE.md §4: %d routes" % len(contract))
    print("PROTOCOL.md:        %d routes" % len(proto))
    if notes:
        print("\n%d route(s) documented beyond the contract summary (informational):" % len(notes))
        for n in notes:
            print("  ·", n)
    if gaps:
        print("\n%d deferred route(s):" % len(gaps))
        for g in gaps:
            print("  -", g)

    # Direction 4: from the handlers outward (ADVERSARY 025).
    fails.extend(check_handlers(proto_txt))

    url = os.environ.get("WHEEL_ENGINE_URL")
    if not url:
        print("\nlive probe skipped — set WHEEL_ENGINE_URL to probe 404-vs-405 "
              "against a running engine (ENG-route-exists / ENG-route-undocumented)")

    if fails:
        print("\nroute parity: %d FAILED" % len(fails))
        for f in fails:
            print("  -", f)
        return 1
    print("\nroute parity: contract and PROTOCOL.md agree (%d deferred)" % len(gaps))
    return 0

if __name__ == "__main__":
    sys.exit(main())
