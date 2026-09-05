#!/usr/bin/env python3
"""The dev-auth bypass must be interlocked at BOOT — TESTPLAN API-dev-interlock-boot.

API owns crates/wheel-api/tests/config_interlock.rs and invited QA to assert the same
guarantee independently. Worth doing: this is the single control preventing HS256 token
forgery against production, and a guarantee this load-bearing should not be checked only
by the code that makes it. This asserts it from OUTSIDE the process, against the shipped
image, which is the form the guarantee actually has to hold in.

The interesting case is UNSET. A deployment that forgets WHEEL_ENV must be treated as
production, because "nobody set it" is exactly the situation where a permissive default
kills you.
"""
import os, subprocess, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results

R = Results()
IMAGE = os.environ.get("WHEEL_API_IMAGE", "wheel-api:latest")

BASE = {
    "DATABASE_URL": "postgres://wheel:wheel@127.0.0.1:5432/wheel",
    "API_MASTER_KEY": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    "CLERK_ISSUER": "https://dev.wheel.local",
    "CLERK_JWKS_URL": "http://localhost/unused-jwks",
    "WHEEL_HOST_URL": "http://host:7100",
    "WHEEL_HOST_SECRET": "dev-host-secret-at-least-16-chars",
    "BIND_ADDR": "0.0.0.0:8080",
}


def boot(env, timeout=60):
    """Start the API image with env; return (exit_code, output). None = still running."""
    args = ["docker", "run", "--rm", "--network", "none"]
    for k, v in {**BASE, **env}.items():
        if v is not None:
            args += ["-e", "%s=%s" % (k, v)]
    args.append(IMAGE)
    try:
        p = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
        return p.returncode, (p.stdout + p.stderr)
    except subprocess.TimeoutExpired as e:
        # Still running past the timeout = it did NOT refuse to boot.
        out = (e.stdout or b"") + (e.stderr or b"")
        return None, out.decode(errors="replace") if isinstance(out, bytes) else str(out)


def image_present():
    return subprocess.run(["docker", "image", "inspect", IMAGE],
                          capture_output=True).returncode == 0


def main():
    if subprocess.run(["docker", "info"], capture_output=True).returncode != 0:
        print("docker not running")
        return 77
    if not image_present():
        print("%s not built yet (API owns docker/Dockerfile.api)" % IMAGE)
        return 77

    # Each of these MUST refuse to boot: a dev secret outside a dev environment.
    for label, env in (
        ("prod", {"WHEEL_ENV": "prod", "AUTH_DEV_SECRET": "leaked"}),
        ("unset", {"WHEEL_ENV": None, "AUTH_DEV_SECRET": "leaked"}),
        ("typo", {"WHEEL_ENV": "development", "AUTH_DEV_SECRET": "leaked"}),
        ("staging", {"WHEEL_ENV": "staging", "AUTH_DEV_SECRET": "leaked"}),
    ):
        rc, out = boot(env)
        R.check("API-dev-interlock-boot/%s" % label, rc is not None and rc != 0,
                "exit=%r — it BOOTED with a dev secret outside dev; output: %s"
                % (rc, out[-300:].replace("\n", " ")))
        if rc:
            # Either explanation is correct. For an invalid WHEEL_ENV the API rejects the
            # value itself before it ever looks at AUTH_DEV_SECRET, which is stricter than
            # this test originally assumed: a typo is refused on its own terms rather than
            # only when it happens to be paired with a dev secret.
            R.check("API-dev-interlock-boot/%s-explains" % label,
                    "AUTH_DEV_SECRET" in out or "WHEEL_ENV" in out,
                    "refusal named neither AUTH_DEV_SECRET nor WHEEL_ENV: %s"
                    % out[-200:].replace("\n", " "))

    # An EMPTY dev secret must be treated as absent, never as an empty HMAC key —
    # an empty key would make every forged token verify.
    rc, out = boot({"WHEEL_ENV": "prod", "AUTH_DEV_SECRET": ""})
    R.check("API-dev-interlock-boot/empty-secret",
            rc is None or "AUTH_DEV_SECRET" not in out,
            "empty AUTH_DEV_SECRET treated as set: %s" % out[-200:].replace("\n", " "))

    return R.report("api-boot-interlock")


if __name__ == "__main__":
    sys.exit(main())
