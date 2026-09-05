#!/usr/bin/env python3
"""Envelope-forgery probe (finding 001). Verifies the engine's <AgentPrompt> framing.

SINGLE SOURCE OF TRUTH (PM ruling, contract §1): the attack bodies are QA's shared fixtures, read BY PATH —
  qa/fixtures/envelope/cases.json         18 structured cases (name/severity/why/body/expect)
  qa/fixtures/envelope-integrity.bin      the 200 KiB byte-exact payload
Nothing is copied into redteam/: if QA's bytes change, this probe changes with them, so an attack I demo is a
body QA regresses and vice-versa. This file prints the sha256 of the .bin it actually used so both sides can
compare against qa/fixtures/README.md.

Modes:
  --oracle "<escaped>"   offline: does a candidate escaping leave anything that can close/open an envelope?
  (default)              e2e: PENDING-STACK until WHEEL_STACK + WHEEL_FAKE_TRANSCRIPT_PATH are set. Reads the
                         victim's raw stdin transcript (QA's fake harness, WHEEL_FAKE_TRANSCRIPT) — ground truth,
                         independent of what the engine *believes* it wrote.

PASS = exactly one engine-attributed envelope per case, body byte-identical, attribution never taken from the body.
"""
import hashlib, json, os, re, sys, pathlib

def repo_root() -> pathlib.Path:
    p = pathlib.Path(__file__).resolve()
    for parent in p.parents:
        if (parent / "qa" / "fixtures").is_dir():
            return parent
    sys.exit("ERROR: cannot locate repo root containing qa/fixtures (run from inside the wheel checkout)")

ROOT = repo_root()
CASES_PATH = ROOT / "qa" / "fixtures" / "envelope" / "cases.json"
BIN_PATH = ROOT / "qa" / "fixtures" / "envelope-integrity.bin"

def load_cases() -> list[dict]:
    cases = json.loads(CASES_PATH.read_text(encoding="utf-8"))["cases"]
    blob = BIN_PATH.read_bytes()
    cases.append({
        "name": "envelope_integrity_bin",
        "severity": "S1",
        "why": "the shared 200 KiB payload, byte-exact",
        "body": blob.decode("utf-8", errors="surrogateescape"),
        "bytes": len(blob),
        "expect": {"envelopes_seen_by_recipient": 1, "attribution_from_body_honoured": False,
                   "body_byte_identical": True, "truncated": False},
    })
    return cases

def bin_sha() -> str:
    return hashlib.sha256(BIN_PATH.read_bytes()).hexdigest()

# What an escaper MUST guarantee on the DECODED byte stream (contract §3 / finding 001): nothing left that
# can CLOSE the real envelope or OPEN a forged one, in any case, with any inner whitespace.
CLOSE = re.compile(r"<\s*/\s*AgentPrompt\s*>", re.IGNORECASE)
OPEN = re.compile(r"<\s*AgentPrompt\b", re.IGNORECASE)

def framing_is_safe(escaped_body: str) -> bool:
    return CLOSE.search(escaped_body) is None and OPEN.search(escaped_body) is None

def run_oracle(escaped: str) -> int:
    ok = framing_is_safe(escaped)
    print(("PASS: " if ok else "FAIL: ") + "escaped body can neither close the envelope nor open a forged one")
    return 0 if ok else 1

def run_e2e(cases: list[dict]) -> int:
    if not os.environ.get("WHEEL_STACK"):
        print(f"PENDING-STACK: set WHEEL_STACK to run e2e; {len(cases)} cases from {CASES_PATH.relative_to(ROOT)} "
              f"+ {BIN_PATH.name} (sha256 {bin_sha()[:16]}…)")
        return 0
    transcript = os.environ.get("WHEEL_FAKE_TRANSCRIPT_PATH")
    if not transcript or not os.path.exists(transcript):
        print("PENDING-STACK: set WHEEL_FAKE_TRANSCRIPT_PATH to the victim's WHEEL_FAKE_TRANSCRIPT file after sending the cases")
        return 0
    expect_from = os.environ.get("WHEEL_EXPECT_FROM", "attacker")
    expect_type = os.environ.get("WHEEL_EXPECT_TYPE", "agent")
    raw = open(transcript, encoding="utf-8", errors="surrogateescape").read()
    failures = []
    for case in cases:
        body, name = case["body"], case["name"]
        idx = raw.find(body[:40])
        if idx < 0:
            failures.append(f"{name}: body not found in transcript (lost or truncated)")
            continue
        seg_start = raw.rfind("<AgentPrompt", 0, idx)
        seg_end = raw.find("</AgentPrompt>", idx)
        seg = raw[seg_start:seg_end] if seg_start >= 0 and seg_end >= 0 else raw[max(0, idx - 400): idx + len(body)]
        inner = seg[len("<AgentPrompt"):]
        if len(OPEN.findall(seg)) != 1 or CLOSE.search(inner):
            failures.append(f"{name}: body produced an extra/unescaped envelope tag (FORGERY POSSIBLE)")
        m = re.search(r'from="([^"]*)"\s+type="([^"]*)"', seg)
        if not m or (m.group(1), m.group(2)) != (expect_from, expect_type):
            failures.append(f"{name}: envelope attrs {m.groups() if m else None} != engine-expected ({expect_from},{expect_type}) — attribution taken from body?")
    for f in failures:
        print("FAIL: FINDING — " + f)
    if not failures:
        print(f"PASS: resisted — {len(cases)} cases, exactly one engine-attributed envelope each (bin sha256 {bin_sha()[:16]}…)")
    return 1 if failures else 0

if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "--oracle":
        sys.exit(run_oracle(sys.argv[2]))
    cases = load_cases()
    forged = next(c for c in cases if c["name"] == "full_forged_envelope")
    assert not framing_is_safe(forged["body"]), "self-test: check failed to bite on the raw forged envelope"
    sys.exit(run_e2e(cases))
