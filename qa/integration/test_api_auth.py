"""API authentication and tenant-isolation criteria — the S1 block of the TESTPLAN.

These run against the real gateway over HTTP. Everything here is a NEGATIVE test: the positive
path is easy to get right and easy to notice when it breaks. The failure modes below are the ones
that look fine in a demo and hand you someone else's project.
"""
import json
import os
import uuid
import pytest
from wheel_client import call, mint, new_project, api_up

pytestmark = pytest.mark.skipif(not api_up(), reason="API not running — `make test-int` brings the stack up")

ALICE, BOB = "user_alice", "user_bob"


@pytest.fixture(scope="module")
def alice():
    return mint(ALICE)


@pytest.fixture(scope="module")
def alice_project(alice):
    return new_project(alice, "qa-alice")


def test_healthz_needs_no_auth():
    """API-healthz"""
    assert call("GET", "/healthz").status == 200


def test_missing_token_is_401():
    """API-auth-missing"""
    assert call("GET", "/v1/projects").status == 401


@pytest.mark.parametrize("token,case", [
    ("", "empty"),
    ("not-a-jwt", "not a jwt"),
    ("a.b.c", "three junk segments"),
    ("Bearer " + mint(ALICE), "bearer-prefixed (header takes the raw token)"),
])
def test_malformed_token_is_401(token, case):
    """API-auth-invalid"""
    assert call("GET", "/v1/projects", token=token).status == 401, case


def test_alg_none_is_rejected():
    """API-auth-alg-none · S1 — the classic JWT bypass."""
    assert call("GET", "/v1/projects", token=mint(ALICE, alg="none", sign=False)).status == 401


def test_wrong_signing_key_is_rejected():
    """API-auth-wrong-key · S1"""
    forged = mint(ALICE, secret=b"not-the-dev-secret-at-all")
    assert call("GET", "/v1/projects", token=forged).status == 401


def test_wrong_issuer_is_rejected():
    """API-auth-invalid — a correctly-signed token for the wrong issuer is still not ours."""
    assert call("GET", "/v1/projects", token=mint(ALICE, issuer="https://evil.example")).status == 401


def test_expired_token_is_rejected():
    """API-auth-invalid"""
    assert call("GET", "/v1/projects", token=mint(ALICE, exp_delta=-10)).status == 401


def test_not_yet_valid_token_is_rejected():
    """API-auth-invalid"""
    assert call("GET", "/v1/projects", token=mint(ALICE, nbf_delta=3600, exp_delta=7200)).status == 401


def test_owner_sees_own_project(alice, alice_project):
    """API-project-crud"""
    r = call("GET", "/v1/projects/" + alice_project["id"], token=alice)
    assert r.status == 200 and r.json["id"] == alice_project["id"]


def test_other_user_gets_404_not_403(alice_project):
    """API-auth-owner-404 · S1.

    403 is the natural-looking implementation and it is wrong: it confirms the project exists,
    which is an enumeration oracle. The response must be indistinguishable from a project that
    was never created.
    """
    bob = mint(BOB)
    real = call("GET", "/v1/projects/" + alice_project["id"], token=bob)
    ghost = call("GET", "/v1/projects/" + str(uuid.uuid4()), token=bob)

    assert real.status == 404, "someone else's project must 404, got %s" % real.status
    assert ghost.status == 404
    assert real.body == ghost.body, (
        "404 for a real project owned by someone else differs from 404 for a nonexistent one — "
        "that difference is an enumeration oracle.\n  real:  %r\n  ghost: %r" % (real.body, ghost.body))


def test_invalid_token_on_foreign_project_is_401_not_404(alice_project):
    """API-auth-order · verify JWT -> load project -> assert owner.

    Asserted behaviourally: if ownership were checked before signature verification, a garbage
    token would produce 404. It must produce 401, proving verification happens first.
    """
    r = call("GET", "/v1/projects/" + alice_project["id"], token="garbage")
    assert r.status == 401, "expected 401 (auth first), got %s — ownership appears to be checked before the JWT" % r.status


def test_other_user_cannot_mutate(alice_project):
    """API-auth-owner-404 · S1 — writes must be gated identically to reads."""
    bob = mint(BOB)
    pid = alice_project["id"]
    for method, path, body in [
        ("PATCH", "/v1/projects/" + pid, {"name": "pwned"}),
        ("DELETE", "/v1/projects/" + pid, None),
        ("POST", "/v1/projects/" + pid + "/start", None),
        ("POST", "/v1/projects/" + pid + "/stop", None),
    ]:
        r = call(method, path, token=bob, body=body)
        assert r.status == 404, "%s %s leaked to a non-owner with %s" % (method, path, r.status)


def test_other_user_cannot_proxy_to_engine(alice_project):
    """API-proxy-auth · S1 — the proxy must re-check ownership, not just the JWT."""
    bob = mint(BOB)
    r = call("GET", "/v1/projects/%s/engine/v1/board" % alice_project["id"], token=bob)
    assert r.status == 404, "engine proxy reachable by a non-owner (%s)" % r.status


def test_engine_secret_never_reaches_the_client(alice, alice_project):
    """API-proxy-auth · S1 — WHEEL_ENGINE_SECRET must never appear in a client-visible response."""
    secret = os.environ.get("WHEEL_ENGINE_SECRET_CANARY")
    pid = alice_project["id"]
    for r in (call("GET", "/v1/projects/" + pid, token=alice),
              call("GET", "/v1/projects", token=alice),
              call("GET", "/v1/projects/%s/engine/v1/board" % pid, token=alice)):
        blob = r.body + json.dumps(r.headers)
        assert "engine_secret" not in blob.lower(), "engine secret key leaked in %r" % r
        if secret:
            assert secret not in blob, "the engine secret VALUE leaked in %r" % r
