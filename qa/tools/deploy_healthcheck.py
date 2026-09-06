#!/usr/bin/env python3
"""DEPLOY-healthz — prove the deployed service answers on the path its config declares.

Incident #2: a Railway service deployed "successfully" while its configured healthcheckPath
answered nothing. A deploy that reports success is not evidence that anything is reachable;
the only evidence is a request from outside, after the deploy, to the path the config names.

Reads the path from infra/railway/settings.json rather than taking one as an argument, so
this checks THE CONFIGURED path. A gate that curls a path someone typed here would keep
passing after the config changed underneath it -- it would be testing this file.

  WHEEL_DEPLOY_API   base URL of the deployed API (required; else SKIP)
  WHEEL_DEPLOY_HOST  optional: an API-proxied liveness route for wheel-host, which has no
                     public domain of its own. Skipped by name until API adds one.
"""
import json, os, re, sys, urllib.error, urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "integration"))
from wheel_client import Results  # noqa: E402

SKIP = 77
R = Results()
ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))


def services():
    try:
        with open(os.path.join(ROOT, "infra", "railway", "settings.json")) as fh:
            return json.load(fh).get("services", {}) or {}
    except (OSError, ValueError):
        return {}


def configured_path(service):
    """The healthcheckPath this service declares, or None if it declares none."""
    return (services().get(service) or {}).get("healthcheckPath")


def get(url, timeout=20):
    req = urllib.request.Request(url, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode(errors="replace")[:400]
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace")[:400]
    except Exception as e:
        return 0, str(e)


def main():
    base = (os.environ.get("WHEEL_DEPLOY_API") or "").rstrip("/")
    if not base:
        print("WHEEL_DEPLOY_API not set — nothing was deployed for this run to check")
        return SKIP

    # Every service should declare one. A service with no healthcheckPath is called healthy
    # by Railway the moment its container boots, which is the state incident #2 was actually
    # about: "deployed successfully" meaning "the process started", not "it can serve".
    undeclared = sorted(n for n, cfg in services().items()
                        if not (cfg or {}).get("healthcheckPath"))
    R.check("DEPLOY-healthz-declared", not undeclared,
            "these services declare no healthcheckPath, so Railway calls them healthy as "
            "soon as the container boots: %s" % undeclared)

    path = configured_path("wheel-api")
    if not R.check("DEPLOY-healthz-configured", bool(path),
                   "infra/railway/settings.json declares no healthcheckPath for wheel-api, so a "
                   "deploy has nothing to gate on and Railway will call it healthy on boot"):
        return R.report("deploy-healthcheck")

    status, body = get(base + path)
    R.check("DEPLOY-healthz-reachable", status == 200,
            "GET %s%s -> %s %s. The deploy reported success; this is the path its own "
            "settings.json names, from outside." % (base, path, status, body))

    # 200 from a proxy or a parked page is not the service answering. The body has to come
    # from OUR handler, or "reachable" means only that something on the internet replied.
    ok_shape = False
    try:
        ok_shape = json.loads(body).get("ok") is True
    except Exception:
        pass
    R.check("DEPLOY-healthz-is-ours", ok_shape,
            "%s%s answered %s but not with our health body — a parked page, an edge cache "
            "or another service on the domain answers 200 just as happily: %r"
            % (base, path, status, body))

    # API merged the proxied liveness route (b2da35a), so this no longer needs configuring
    # by hand: it hangs off the API base at a known path. The env var stays an override.
    api_base = (os.environ.get("WHEEL_DEPLOY_API") or "").strip().rstrip("/")
    host_url = (os.environ.get("WHEEL_DEPLOY_HOST") or "").strip()
    if not host_url and api_base:
        host_url = api_base + "/v1/host/healthz"

    if not host_url:
        # Skipped BY NAME rather than quietly dropped: the host holds every project's
        # engine secret, and "we never checked it" must not read like "it is fine".
        R.skip("DEPLOY-healthz-host",
               "no deployed API base (set WHEEL_DEPLOY_API) — the host has no public "
               "domain, so it is reachable only through the API's proxied route")
    else:
        status, body = get(host_url)
        # A green API does NOT imply a green host: the API stayed healthy through an outage
        # where the host was stopped and every project create hung. That is why this route
        # exists, so the check must hit the HOST path and never settle for /healthz.
        ok = R.check("DEPLOY-healthz-host", status == 200,
                     "GET %s -> %s %s" % (host_url, status, body))
        if ok:
            # Liveness ONLY. The route is unauthenticated by design, so whatever it says is
            # said to the whole internet: which sandbox backend is in use, how many projects
            # are running, or an upstream error string are each free reconnaissance. API
            # withheld all three deliberately; this is the check that it stays that way,
            # because the tempting next commit is "include the reason so we can debug it".
            text = body.lower() if isinstance(body, str) else json.dumps(body).lower()
            leaks = [w for w in ("docker", "process", "railway", "projects_running",
                                 "backend", "traceback", "refused", "timed out", "secret")
                     if w in text]
            R.check("DEPLOY-healthz-host/liveness-only", not leaks,
                    "the unauthenticated host liveness route disclosed %s in %r — it may "
                    "say whether the pair is serving, and nothing else" % (sorted(leaks), body))

    return R.report("deploy-healthcheck")


if __name__ == "__main__":
    sys.exit(main())
