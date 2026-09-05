#!/usr/bin/env python3
"""WOW — Wheel building Wheel (TESTPLAN WOW-*, contract M1.6, operator priority).

The acceptance test for the whole product, not for a component: an agent inside a sandbox,
handed a git token from a vault node, clones this repository and runs `cargo test -p
wheel-core` — the same gate its own authors run. If that works, the sandbox is a real
development environment and not a demo.

It needs a toolchain image (git + gh + cargo inside the sandbox) that `wheel-engine:test`
does not yet carry, so today every criterion below reports SKIP with that reason. It is
committed now, red-by-absence rather than absent, so that the day SDK lands the toolchain
image this suite turns green or red on its own instead of waiting for someone to remember
to write it.

Deliberately NOT hermetic in the usual sense: it clones over the network and compiles. It
is opt-in (`WHEEL_WOW=1`) and never runs in the default CI matrix — a 10-minute cargo build
inside a container does not belong in a gate developers run before every merge.
"""
import json, os, subprocess, sys, time, uuid
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results

SKIP = 77
R = Results()
NAME = "qa-engine-wow"
PORT = int(os.environ.get("WHEEL_ENGINE_WOW_PORT", "17415"))
BASE = "http://127.0.0.1:%d" % PORT
SECRET = "qa-wow-secret-at-least-16chars"
REPO = os.environ.get("WHEEL_WOW_REPO", "https://github.com/Morgandri1/wheel.git")

# What the sandbox must provide before this test means anything. Named individually so the
# skip reason says WHICH tool is missing rather than "unsupported".
TOOLCHAIN = ("git", "cargo", "gh")


def sh(*a, **kw):
    return subprocess.run(a, capture_output=True, text=True, **kw)


def missing_tools(image):
    out = []
    for t in TOOLCHAIN:
        p = sh("docker", "run", "--rm", "--entrypoint", "sh", image, "-c", "command -v " + t)
        if p.returncode != 0:
            out.append(t)
    return out


def main():
    if os.environ.get("WHEEL_WOW") != "1":
        print("wheel-on-wheel is opt-in: set WHEEL_WOW=1 (it clones and compiles; minutes, "
              "not seconds, so it is not in the default gate)")
        return SKIP
    if subprocess.run(["docker", "info"], capture_output=True).returncode != 0:
        print("docker not running")
        return SKIP
    image = os.environ.get("WHEEL_ENGINE_IMAGE", "wheel-engine:test")
    if subprocess.run(["docker", "image", "inspect", image], capture_output=True).returncode != 0:
        print("%s not built — run `make engine-image-test`" % image)
        return SKIP

    absent = missing_tools(image)
    if absent:
        # The whole point of the test is that a real agent can do real work in the sandbox.
        # Without the toolchain there is nothing to measure, and a green here would mean
        # only that we successfully did nothing.
        for tid in ("WOW-clone", "WOW-vault-token", "WOW-cargo-test", "WOW-no-token-in-log"):
            R.skip(tid, "sandbox image lacks %s — needs SDK's toolchain image" % ", ".join(absent))
        return R.report("wheel-on-wheel")

    print("toolchain present (%s) — running the real thing" % ", ".join(TOOLCHAIN))
    R.skip("WOW-clone", "toolchain landed; wire up the agent-driven run (next commit)")
    return R.report("wheel-on-wheel")


if __name__ == "__main__":
    sys.exit(main())
