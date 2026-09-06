#!/usr/bin/env python3
"""The F015 inherited-env allowlist may not change without a recorded reason.

`INHERITED_ENV` in the engine's supervisor decides what an agent child — untrusted code —
can read out of the engine's own environment. Getting it wrong once already handed every
agent the control-plane bearer and the key to every vault in the project (ADVERSARY F015).

ADVERSARY asked to review changes to that list. This gate is what makes the review durable
rather than dependent on somebody remembering to mention it: the list is pinned here with a
reason per entry, and any addition, removal or rename fails until the pin is updated in the
same change. A name with no reason is the shape that lets an allowlist grow one convenience
at a time until it is a deny-list again.

It is deliberately NOT a judgement about whether an entry is safe — that is ADVERSARY's call
and PM's ruling. It only guarantees nobody gets to make that call silently.
"""
import json
import os
import re
import sys

SKIP = 77
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
SRC = os.path.join(ROOT, "crates", "wheel-engine", "src", "supervisor", "mod.rs")
PIN = os.path.join(os.path.dirname(os.path.abspath(__file__)), "env-allowlist.json")

BLOCK = re.compile(r"const INHERITED_ENV:\s*&\[&str\]\s*=\s*&\[(.*?)\];", re.S)


def main():
    if not os.path.exists(SRC):
        print("engine supervisor not found — has the layout moved?")
        return SKIP
    src = open(SRC).read()
    m = BLOCK.search(src)
    if not m:
        # The constant is the subject. If it cannot be found the gate is not passing,
        # it is blind, and blind must not read like clean.
        print("could not find `const INHERITED_ENV` in %s — this gate is now checking "
              "nothing; re-pin it against wherever the allowlist moved." % SRC)
        return 1

    actual = set(re.findall(r'"([A-Za-z_][A-Za-z0-9_]*)"', m.group(1)))
    pinned = json.load(open(PIN))
    allowed = pinned.get("allowed") or {}
    expected = set(allowed)

    added = sorted(actual - expected)
    removed = sorted(expected - actual)
    unexplained = sorted(k for k, v in allowed.items() if not str(v).strip())

    print("inherited-env allowlist: %d entr%s in the engine, %d pinned"
          % (len(actual), "y" if len(actual) == 1 else "ies", len(expected)))

    fails = []
    if added:
        fails.append(
            "ADDED to the F015 allowlist without a recorded reason: %s.\n"
            "    Every name here is something untrusted code can read. If this is intended, "
            "add it to qa/contract/env-allowlist.json IN THE SAME CHANGE with why it is safe, "
            "and tell ADVERSARY — they asked to review this list." % ", ".join(added))
    if removed:
        fails.append(
            "REMOVED from the allowlist but still pinned: %s.\n"
            "    Tightening is welcome and still has to be deliberate: drop it from the pin "
            "too, so the file keeps describing the engine." % ", ".join(removed))
    if unexplained:
        fails.append(
            "pinned with an empty reason: %s — a name with no reason is how an allowlist "
            "grows one convenience at a time." % ", ".join(unexplained))

    if fails:
        print()
        for f in fails:
            print("  - " + f)
        return 1
    print("inherited-env allowlist: unchanged, every entry has a recorded reason")
    return 0


if __name__ == "__main__":
    sys.exit(main())
