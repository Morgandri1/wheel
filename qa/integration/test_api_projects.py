"""Project lifecycle and the public ingress path.

Ingress is the only route on the whole API that is deliberately unauthenticated, which makes it
the most interesting surface here: the capability gate is the ONLY thing standing between the
public internet and a tenant's container.
"""
import json, uuid
import pytest
from wheel_client import call, mint, new_project, api_up

pytestmark = pytest.mark.skipif(not api_up(), reason="API not running — `make test-int` brings the stack up")

ALICE = "user_alice_projects"


@pytest.fixture(scope="module")
def alice():
    return mint(ALICE)


@pytest.fixture(scope="module")
def project(alice):
    return new_project(alice, "qa-ingress")


def test_create_list_get(alice):
    """API-project-crud"""
    p = new_project(alice, "qa-crud")
    assert p["name"] == "qa-crud"
    assert p["owner_id"] == ALICE, "owner_id must be the JWT sub"

    listed = call("GET", "/v1/projects", token=alice)
    assert listed.status == 200
    assert any(x["id"] == p["id"] for x in listed.json), "created project missing from the list"

    got = call("GET", "/v1/projects/" + p["id"], token=alice)
    assert got.status == 200 and got.json["id"] == p["id"]


def test_list_does_not_require_project_header(alice):
    """API-project-crud — GET /v1/projects must work without x-project-id."""
    assert call("GET", "/v1/projects", token=alice).status == 200


def test_project_shape(project):
    """API-project-crud — the documented Project shape."""
    for k in ("id", "owner_id", "name", "capabilities", "status", "created_at", "updated_at"):
        assert k in project, "Project is missing %r: %r" % (k, project)
    assert "http" in project["capabilities"]
    assert project["status"] in ("stopped", "starting", "running", "error")


def test_list_is_scoped_to_owner(alice):
    """API-auth-owner-404 · S1 — one tenant must never see another's projects."""
    new_project(alice, "qa-alice-private")
    others = call("GET", "/v1/projects", token=mint("user_stranger"))
    assert others.status == 200
    assert all(p["owner_id"] == "user_stranger" for p in others.json), \
        "another user's projects appear in this user's list"


def test_unknown_project_is_404(alice):
    """API-auth-owner-404"""
    assert call("GET", "/v1/projects/" + str(uuid.uuid4()), token=alice).status == 404


def test_malformed_project_id_does_not_500(alice):
    """API-project-crud — a non-uuid id is a client error, never a server error."""
    r = call("GET", "/v1/projects/not-a-uuid", token=alice)
    assert r.status < 500, "malformed project id produced %s" % r.status
    assert r.status in (400, 404), "expected 400/404 for a malformed id, got %s" % r.status


class TestIngress:
    """The public, unauthenticated path. `http` capability is the only gate."""

    def _set_http(self, alice, pid, on):
        r = call("PATCH", "/v1/projects/" + pid, token=alice, body={"capabilities": {"http": on}})
        assert r.status in (200, 204), "could not set http=%s: %r" % (on, r)

    def test_disabled_capability_is_403(self, alice, project):
        """ING-cap-off · S1 — with http:false the request must not reach the container at all."""
        self._set_http(alice, project["id"], False)
        r = call("GET", "/p/%s/anything" % project["id"])
        assert r.status == 403, "ingress reachable with http capability disabled (%s)" % r.status

    def test_ingress_needs_no_auth_token(self, alice, project):
        """ING-no-auth — ingress is public by design; assert that explicitly."""
        self._set_http(alice, project["id"], True)
        r = call("GET", "/p/%s/anything" % project["id"])
        assert r.status != 401, "ingress demanded a token; it is public by design"

    def test_ingress_cannot_reach_the_control_plane(self, alice, project):
        """ING-traversal · S1.

        The prize for an attacker is /v1/* on the engine. Every encoding of `..` must fail to
        get there. A 200 carrying a board is the catastrophic outcome; anything else is fine.
        """
        self._set_http(alice, project["id"], True)
        pid = project["id"]
        for path in [
            "/p/%s/../v1/board" % pid,
            "/p/%s/%%2e%%2e/v1/board" % pid,
            "/p/%s/%%252e%%252e/v1/board" % pid,
            "/p/%s/..%%2fv1%%2fboard" % pid,
            "/p/%s/....//v1/board" % pid,
            "/p/%s/..\\v1\\board" % pid,
        ]:
            r = call("GET", path)
            # A JSON body parses to a dict; a refusal is usually text. Normalise both, or
            # the assertion below crashes on the successful-traversal case it exists to catch.
            body = (json.dumps(r.body) if isinstance(r.body, (dict, list)) else (r.body or "")).lower()
            assert not (r.status == 200 and '"nodes"' in body), \
                "TRAVERSAL: %s reached the engine control plane" % path

    def test_ingress_for_unknown_project_does_not_leak(self):
        """ING-cap-off / API-auth-owner-404 — an unknown project must not be distinguishable."""
        r = call("GET", "/p/%s/x" % uuid.uuid4())
        assert r.status in (403, 404), "unexpected %s for an unknown project's ingress" % r.status
        assert r.status < 500


class TestLifecycle:
    def test_start_reports_status(self, alice, project):
        """API-lifecycle"""
        r = call("POST", "/v1/projects/%s/start" % project["id"], token=alice)
        assert r.status < 500, "start returned %s: %r" % (r.status, r.body)

    def test_stop_is_idempotent(self, alice, project):
        """API-lifecycle — stop twice must not error the second time."""
        pid = project["id"]
        first = call("POST", "/v1/projects/%s/stop" % pid, token=alice)
        second = call("POST", "/v1/projects/%s/stop" % pid, token=alice)
        assert first.status < 500 and second.status < 500, \
            "stop is not idempotent: %s then %s" % (first.status, second.status)
