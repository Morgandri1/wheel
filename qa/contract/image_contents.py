#!/usr/bin/env python3
"""The engine image must actually CONTAIN the binaries the contract depends on.

TESTPLAN: ENG-image-contents.

This exists because of BUG-010. docker/Dockerfile.host built `--bin wheel-cli` (no such
binary; the crate is wheel-cli but the bin is `wheel`) under `|| true`, then copied
`wheel-cl[i]` — an optional-glob COPY that matches nothing and does not fail. Two layers
of deliberate silence combined to ship an image with no `wheel` CLI at all, which is the
agent's entire interface to the board. Nothing failed; the image just quietly lacked it.

So: assert presence explicitly, by asking the image, rather than trusting the build.
"""
import os, subprocess, sys

SKIP = 77
REQUIRED = ["wheel", "wheel-engine", "claude", "codex", "python3"]


def have(image, binary):
    p = subprocess.run(
        ["docker", "run", "--rm", "--entrypoint", "sh", image, "-c",
         "command -v %s >/dev/null 2>&1" % binary],
        capture_output=True)
    return p.returncode == 0


def main():
    # Prefer whichever image is actually here. The CI job that builds an image builds
    # `wheel-engine:dev` (`make engine-image`); only the integration job builds `:test`.
    # Hardcoding :test made this gate exit 77 in the job I had just moved it to, and `-e`
    # turned that skip into a red job -- the gate relocation broke the job it moved to,
    # which is a tidier version of the mistake the relocation guard exists to prevent.
    image = os.environ.get("WHEEL_IMAGE")
    if not image:
        for candidate in ("wheel-engine:dev", "wheel-engine:test"):
            if subprocess.run(["docker", "image", "inspect", candidate],
                              capture_output=True).returncode == 0:
                image = candidate
                break
        else:
            image = "wheel-engine:test"
    if subprocess.run(["docker", "info"], capture_output=True).returncode != 0:
        print("docker not running")
        return SKIP
    if subprocess.run(["docker", "image", "inspect", image],
                      capture_output=True).returncode != 0:
        print("no engine image present (tried wheel-engine:dev and :test) — build one with `make engine-image` or `make engine-image-test`")
        return SKIP

    fails = []
    for b in REQUIRED:
        if have(image, b):
            print("  ok   %s is on PATH" % b)
        else:
            fails.append(b)
            print("  FAIL %s is NOT on PATH in %s" % (b, image))

    print()
    if fails:
        print("image contents: missing %s" % ", ".join(fails))
        print("  the agent cannot use a binary the image does not contain; see qa/BUGS.md #010")
        return 1
    print("image contents: %s has every binary the contract requires" % image)
    return 0


if __name__ == "__main__":
    sys.exit(main())
