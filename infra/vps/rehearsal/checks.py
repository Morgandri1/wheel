#!/usr/bin/env python3
# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

"""One rehearsal check per run: `checks.py <name>`. Exit 0 PASS, 1 FAIL, 3 SKIP (a check it builds on
failed first, and already said so).

Driven by infra/vps/rehearse.sh. Every request goes to 127.0.0.1 through Caddy, addressed to
$REHEARSE_DOMAIN, with TLS verified against Caddy's internal root at $REHEARSE_CA. State the checks
share (credentials, ids) lives in $REHEARSE_STATE, mode 0600.
"""

import base64
import hashlib
import json
import os
import secrets
import select
import socket
import ssl
import sys
import time
import uuid

HOST = os.environ.get("REHEARSE_DOMAIN", "localhost")
ORIGIN = f"https://{HOST}"
STATE = os.environ["REHEARSE_STATE"]
FAKE_HARNESS = os.environ.get("REHEARSE_FAKE_HARNESS") == "1"
CTX = ssl.create_default_context(cafile=os.environ["REHEARSE_CA"])
FORGED = "6.6.6.6"
EDGE_413 = b"over this route's limit at the proxy"
MiB = 1024 * 1024
WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


class Failed(Exception):
    pass


class Skipped(Exception):
    pass


def expect(condition, why):
    if not condition:
        raise Failed(why)


def load():
    with open(STATE) as f:
        return json.load(f)


def save(**values):
    state = load()
    state.update(values)
    with open(STATE, "w") as f:
        json.dump(state, f)


def need(*keys):
    state = load()
    missing = [k for k in keys if not state.get(k)]
    if missing:
        raise Skipped(f"needs {', '.join(missing)} from an earlier check")
    return state


class Reply:
    def __init__(self, status, headers, body):
        self.status, self.headers, self.body = status, headers, body

    def header(self, name):
        values = self.headers.get(name)
        return values[0] if values else None

    def json(self):
        return json.loads(self.body)

    def brief(self):
        return f"{self.status} {self.body[:160]!r}"


class Conn:
    """HTTP/1.1 over one socket to 127.0.0.1, so a rehearsal domain needs no DNS."""

    def __init__(self, tls=True, timeout=30):
        self.timeout = timeout
        raw = socket.create_connection(("127.0.0.1", 443 if tls else 80), timeout=timeout)
        self.sock = CTX.wrap_socket(raw, server_hostname=HOST) if tls else raw
        self.tls = tls
        self.buf = b""

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass

    def send(self, method, path, headers, body=None):
        names = {k.lower() for k in headers}
        lines = [f"{method} {path} HTTP/1.1", f"Host: {HOST}"]
        if "connection" not in names:
            lines.append("Connection: close")
        if body is not None:
            lines.append(f"Content-Length: {len(body)}")
        lines += [f"{k}: {v}" for k, v in headers.items()]
        self.sock.sendall(("\r\n".join(lines) + "\r\n\r\n").encode())
        if body:
            self._write_body(body)

    def _write_body(self, body):
        """Send the body, but stop the moment the server answers.

        A server that refuses an upload answers before it has read all of it. A blocking send would
        then fail with EPIPE, and a TLS connection that failed a write refuses to read the answer
        that already arrived. So nothing here ever blocks in send.
        """
        view = memoryview(body)
        sent = 0
        deadline = time.monotonic() + self.timeout
        self.sock.setblocking(False)
        try:
            while sent < len(view) and time.monotonic() < deadline:
                readable, writable, _ = select.select([self.sock], [self.sock], [], 1)
                if readable or (self.tls and self.sock.pending()):
                    try:
                        chunk = self.sock.recv(65536)
                    except (ssl.SSLWantReadError, ssl.SSLWantWriteError, BlockingIOError):
                        chunk = None
                    except (ssl.SSLError, OSError):
                        return
                    if chunk is not None:
                        self.buf += chunk
                        return
                if writable:
                    try:
                        sent += self.sock.send(view[sent : sent + 65536])
                    except (ssl.SSLWantReadError, ssl.SSLWantWriteError, BlockingIOError):
                        pass
                    except (ssl.SSLError, OSError):
                        return
        finally:
            self.sock.settimeout(self.timeout)

    def _fill(self, deadline):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("no answer in time")
        self.sock.settimeout(remaining)
        try:
            chunk = self.sock.recv(65536)
        except (ssl.SSLError, ConnectionResetError, BrokenPipeError):
            chunk = b""
        self.buf += chunk
        return bool(chunk)

    def head(self, timeout=None):
        deadline = time.monotonic() + (timeout or self.timeout)
        while b"\r\n\r\n" not in self.buf:
            if not self._fill(deadline):
                raise ConnectionError("closed before a response")
        head, self.buf = self.buf.split(b"\r\n\r\n", 1)
        lines = head.decode("latin-1").split("\r\n")
        headers = {}
        for line in lines[1:]:
            name, _, value = line.partition(":")
            headers.setdefault(name.strip().lower(), []).append(value.strip())
        return int(lines[0].split()[1]), headers

    def body(self, headers, timeout=None):
        deadline = time.monotonic() + (timeout or self.timeout)
        if "chunked" in ",".join(headers.get("transfer-encoding", [])).lower():
            out = b""
            while True:
                while b"\r\n" not in self.buf:
                    if not self._fill(deadline):
                        return out
                size_line, self.buf = self.buf.split(b"\r\n", 1)
                size = int(size_line.split(b";")[0], 16)
                if size == 0:
                    return out
                while len(self.buf) < size + 2:
                    if not self._fill(deadline):
                        return out + self.buf
                out, self.buf = out + self.buf[:size], self.buf[size + 2 :]
        if "content-length" in headers:
            size = int(headers["content-length"][0])
            while len(self.buf) < size and self._fill(deadline):
                pass
            return self.buf[:size]
        while self._fill(deadline):
            pass
        return self.buf

    def until(self, marker, timeout):
        """Stream until `marker` shows up; the bytes after it stay buffered."""
        deadline = time.monotonic() + timeout
        while marker not in self.buf:
            if not self._fill(deadline):
                raise ConnectionError(f"stream closed before {marker!r}")
        _, self.buf = self.buf.split(marker, 1)

    def ws_text(self, timeout):
        deadline = time.monotonic() + timeout

        def want(n):
            while len(self.buf) < n:
                if not self._fill(deadline):
                    raise ConnectionError("socket closed mid-frame")

        while True:
            want(2)
            opcode, length, offset = self.buf[0] & 0x0F, self.buf[1] & 0x7F, 2
            if length == 126:
                want(4)
                length, offset = int.from_bytes(self.buf[2:4], "big"), 4
            elif length == 127:
                want(10)
                length, offset = int.from_bytes(self.buf[2:10], "big"), 10
            want(offset + length)
            payload, self.buf = self.buf[offset : offset + length], self.buf[offset + length :]
            if opcode == 8:
                raise ConnectionError("server closed the socket")
            if opcode in (1, 2):
                return payload.decode()


def request(method, path, headers=None, body=None, tls=True, timeout=30):
    conn = Conn(tls, timeout)
    try:
        conn.send(method, path, headers or {}, body)
        status, reply_headers = conn.head()
        data = b"" if method == "HEAD" or status in (101, 204, 304) else conn.body(reply_headers)
        return Reply(status, reply_headers, data)
    finally:
        conn.close()


def api(method, path, token=None, payload=None, headers=None):
    all_headers = dict(headers or {})
    if token:
        all_headers["x-auth-token"] = token
    body = None
    if payload is not None:
        all_headers["content-type"] = "application/json"
        body = json.dumps(payload).encode()
    return request(method, path, all_headers, body)


def engine(pid):
    return f"/v1/projects/{pid}/engine/v1"


def inbox(pid, agent, token):
    reply = api("GET", f"{engine(pid)}/agents/{agent}/inbox", token)
    expect(reply.status == 200, f"inbox answered {reply.brief()}")
    return reply.json()["messages"]


CHECKS = {}


def check(fn):
    CHECKS[fn.__name__.replace("_", "-")] = fn
    return fn


@check
def web_served():
    page = request("GET", "/")
    expect(page.status == 200, f"GET / answered {page.status}")
    expect("text/html" in (page.header("content-type") or ""), f"GET / is {page.header('content-type')}")
    expect("nonce-" in (page.header("content-security-policy") or ""), "no per-request CSP nonce: not the web app")
    version = request("GET", "/version.json").json().get("version")
    return f"{ORIGIN}/ is the web app (text/html, per-request CSP nonce), version {version}"


@check
def edge_headers():
    missing = []
    for path in ("/version.json", "/v1/projects"):
        reply = request("GET", path)
        hsts = reply.header("strict-transport-security") or ""
        if "max-age=31536000" not in hsts:
            missing.append(f"{path}: HSTS {hsts!r}")
        if reply.header("x-content-type-options") != "nosniff":
            missing.append(f"{path}: nosniff")
        if not reply.header("x-frame-options"):
            missing.append(f"{path}: X-Frame-Options")
        if not reply.header("referrer-policy"):
            missing.append(f"{path}: Referrer-Policy")
        if reply.header("server"):
            missing.append(f"{path}: Server: {reply.header('server')}")
    expect(not missing, "; ".join(missing))
    plain = request("GET", "/version.json", tls=False)
    expect(
        plain.status in (301, 308) and plain.header("location") == f"{ORIGIN}/version.json",
        f"http:// answered {plain.status} Location {plain.header('location')}",
    )
    expect(plain.header("strict-transport-security") is None, "HSTS sent over plain HTTP")
    return "HSTS, nosniff, X-Frame-Options, Referrer-Policy on web and /v1; no Server; http:// redirects to https:// without HSTS"


@check
def signup_closed_at_edge():
    stranger = {"email": f"stranger-{uuid.uuid4().hex[:8]}@wheel.test", "password": secrets.token_urlsafe(18)}
    direct = api("POST", "/v1/auth/signup", payload=stranger)
    via_web = api("POST", "/api/session/signup", payload=stranger, headers={"origin": ORIGIN})
    for label, reply in (("/v1/auth/signup", direct), ("/api/session/signup", via_web)):
        expect(reply.status == 403 and b"signup_closed" in reply.body, f"{label} answered {reply.brief()}")
    return "POST /v1/auth/signup and /api/session/signup refused at the edge (403 signup_closed)"


@check
def operator_adds_account():
    state = need("wht")
    email, password = f"rehearsal-{uuid.uuid4().hex[:8]}@wheel.test", secrets.token_urlsafe(24)
    reply = api("POST", "/v1/auth/users", state["wht"], {"email": email, "password": password})
    expect(reply.status == 201, f"POST /v1/auth/users with the operator token answered {reply.brief()}")
    save(email=email, password=password)
    return f"the operator token added {email} through Caddy (POST /v1/auth/users → 201)"


@check
def sign_in():
    state = need("email", "password")
    credentials = {"email": state["email"], "password": state["password"]}
    forged = api("POST", "/api/session/login", payload=credentials, headers={"origin": "https://evil.example"})
    expect(forged.status == 403, f"a sign-in from Origin https://evil.example answered {forged.brief()}")
    reply = api("POST", "/api/session/login", payload=credentials, headers={"origin": ORIGIN})
    expect(reply.status == 200, f"web sign-in answered {reply.brief()}")
    cookie = next((c for c in reply.headers.get("set-cookie", []) if c.startswith("__Host-wheel_session=")), None)
    expect(cookie and "Secure" in cookie and "HttpOnly" in cookie, f"session cookie: {reply.headers.get('set-cookie')}")
    pair = cookie.split(";")[0]
    me = request("GET", "/api/session", {"cookie": pair})
    expect(me.status == 200 and (me.json().get("user") or {}).get("email") == state["email"], f"/api/session: {me.brief()}")
    login = api("POST", "/v1/auth/login", payload=credentials)
    expect(login.status == 200, f"/v1/auth/login answered {login.brief()}")
    save(cookie=pair, session=login.json()["token"])
    return "web sign-in set a Secure, HttpOnly __Host- cookie and /api/session knows the user; a foreign Origin was refused; /v1/auth/login issued an API session"


@check
def project():
    state = need("cookie", "session")
    created = api("POST", "/api/wheel/v1/projects", payload={"name": "rehearsal"}, headers={"cookie": state["cookie"], "origin": ORIGIN})
    expect(created.status == 201, f"creating through the web app's API proxy answered {created.brief()}")
    pid = created.json()["id"]
    ingress_base = created.json().get("ingress_base_url")
    expect(ingress_base == f"{ORIGIN}/p/{pid}", f"ingress_base_url is {ingress_base!r}, not the public {ORIGIN}/p/{pid}")
    started = api("POST", f"/v1/projects/{pid}/start", state["session"])
    expect(started.status == 200 and started.json().get("status") == "running", f"start answered {started.brief()}")
    save(project=pid)
    return f"project {pid} created through the web app (cookie → server → wheeld) and running; ingress_base_url {ingress_base}"


@check
def agent_message():
    state = need("project", "session")
    pid, token = state["project"], state["session"]
    node = api("POST", f"{engine(pid)}/nodes", token, {
        "name": "rehearsal-agent",
        "type": "agent",
        "config": {"harness": "claude", "system_prompt": "You are a rehearsal agent.", "run_on_startup": False, "ephemeral_context": False},
    })
    expect(node.status == 201, f"creating the agent answered {node.brief()}")
    agent = node.json()["id"]
    save(agent=agent)
    api("POST", f"{engine(pid)}/agents/{agent}/start", token)
    sent = api("POST", f"{engine(pid)}/agents/{agent}/send", token, {"body": "hello from the VPS rehearsal"})
    expect(sent.status in (200, 201, 202), f"send answered {sent.brief()}")
    message = sent.json()["id"]
    wanted = {"delivered", "consumed"} if FAKE_HARNESS else {"queued", "delivered", "consumed"}
    state_seen = None
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        row = next((m for m in inbox(pid, agent, token) if m["id"] == message), None)
        state_seen = row and row["state"]
        if state_seen in ("delivered", "consumed"):
            break
        time.sleep(1)
    board = api("GET", f"{engine(pid)}/board", token).json()
    status = next((n.get("state") or {} for n in board["nodes"] if n["id"] == agent), {}).get("status")
    expect(state_seen in wanted, f"message {message} is {state_seen!r} (agent {status}); wanted one of {sorted(wanted)}")
    note = "fake harness" if FAKE_HARNESS else "real claude CLI without a credential; a real run needs one in a vault"
    return f"agent created; message reached {state_seen}; agent status {status} ({note})"


@check
def token_auth():
    state = need("project", "session", "wht")
    pid = state["project"]
    anonymous = request("GET", "/v1/projects")
    expect(anonymous.status == 401, f"no token: {anonymous.brief()}")
    minted = api("POST", "/v1/auth/tokens", state["session"], {"name": "agentgrid-rehearsal"})
    expect(minted.status == 201 and minted.json()["token"].startswith("wht_"), f"minting: {minted.brief()}")
    user_wht = minted.json()["token"]
    save(user_wht=user_wht)
    cases = {
        "session as Bearer": {"authorization": f"Bearer {state['session']}"},
        "user wht_ as Bearer": {"authorization": f"Bearer {user_wht}"},
        "user wht_ as x-auth-token": {"x-auth-token": user_wht},
    }
    for label, headers in cases.items():
        reply = request("GET", "/v1/projects", headers)
        expect(reply.status == 200 and pid.encode() in reply.body, f"{label}: {reply.brief()}")
    operator = request("GET", "/v1/projects", {"x-auth-token": state["wht"]})
    expect(operator.status == 200, f"operator wht_: {operator.brief()}")
    unknown = request("GET", "/v1/projects", {"x-auth-token": "wht_" + "A" * 43})
    expect(unknown.status == 401, f"unknown wht_: {unknown.brief()}")
    not_owner = api("POST", "/v1/auth/users", state["session"], {"email": f"x-{uuid.uuid4().hex[:6]}@wheel.test", "password": secrets.token_urlsafe(18)})
    expect(not_owner.status == 403, f"a non-owner adding an account: {not_owner.brief()}")
    return "GET /v1/projects: no token 401, unknown wht_ 401; session and a user wht_ (Bearer or x-auth-token) 200 with the project; operator wht_ 200; only the owner may add accounts"


def ws_handshake(path, headers):
    key = base64.b64encode(os.urandom(16)).decode()
    conn = Conn(timeout=15)
    conn.send("GET", path, {"Upgrade": "websocket", "Connection": "Upgrade", "Sec-WebSocket-Key": key, "Sec-WebSocket-Version": "13", **headers})
    status, reply_headers = conn.head()
    accept = base64.b64encode(hashlib.sha1((key + WS_GUID).encode()).digest()).decode()
    return conn, status, reply_headers.get("sec-websocket-accept", [None])[0] == accept


def touch_board(pid, token):
    reply = api("POST", f"{engine(pid)}/nodes", token, {"name": f"probe-{uuid.uuid4().hex[:8]}", "type": "ctx", "config": {"markdown": "rehearsal"}})
    expect(reply.status == 201, f"touching the board: {reply.brief()}")


@check
def websocket():
    state = need("project", "session", "user_wht")
    pid = state["project"]
    path = f"{engine(pid)}/events"
    anonymous, anonymous_status, _ = ws_handshake(path, {})
    anonymous.close()
    expect(anonymous_status == 401, f"a handshake without a token answered {anonymous_status}")
    conn, status, accepted = ws_handshake(path, {"x-auth-token": state["user_wht"]})
    try:
        expect(status == 101 and accepted, f"handshake with a wht_ header answered {status} (accept ok: {accepted})")
        touch_board(pid, state["session"])
        event = json.loads(conn.ws_text(timeout=10))
    finally:
        conn.close()
    return f"wss://{HOST}{path}: 101 with x-auth-token: wht_…, then a {event.get('type')!r} event; 401 without a token"


@check
def web_sse():
    state = need("project", "session", "cookie")
    pid = state["project"]
    conn = Conn(timeout=20)
    try:
        started = time.monotonic()
        conn.send("GET", f"/api/wheel/projects/{pid}/events", {"cookie": state["cookie"], "accept": "text/event-stream"})
        status, headers = conn.head()
        content_type = ",".join(headers.get("content-type", []))
        expect(status == 200 and "text/event-stream" in content_type, f"events answered {status} {content_type}")
        conn.until(b"event: wheel-open", timeout=10)
        opened = time.monotonic() - started
        touch_board(pid, state["session"])
        touched = time.monotonic()
        conn.until(b"data: {", timeout=10)
        relayed = time.monotonic() - touched
    finally:
        conn.close()
    return f"text/event-stream through Caddy: wheel-open after {opened:.2f}s, a board event {relayed:.2f}s after the change, connection still open"


@check
def ingress():
    state = need("project", "session", "agent")
    pid, token, agent = state["project"], state["session"], state["agent"]
    enabled = api("PATCH", f"/v1/projects/{pid}", token, {"capabilities": {"http": True}})
    expect(enabled.status == 200, f"enabling ingress: {enabled.brief()}")
    hook = api("POST", f"{engine(pid)}/nodes", token, {"name": "rehearsal-hook", "type": "endpoint", "config": {"method": "POST", "path": "/hook", "response_mode": "ack"}})
    expect(hook.status == 201, f"creating the endpoint: {hook.brief()}")
    wired = api("POST", f"{engine(pid)}/wires", token, {"from": hook.json()["id"], "to": agent, "type": "send"})
    expect(wired.status in (200, 201, 204), f"wiring endpoint → agent: {wired.brief()}")
    nonce = uuid.uuid4().hex
    hit = request("POST", f"/p/{pid}/hook", {
        "content-type": "application/json",
        "x-forwarded-for": FORGED,
        "x-real-ip": FORGED,
        "forwarded": f"for={FORGED};proto=http;host=evil.example",
        "x-forwarded-proto": "http",
        "x-forwarded-host": "evil.example",
        "x-wheel-client-ip": FORGED,
    }, json.dumps({"nonce": nonce}).encode())
    expect(hit.status == 202, f"POST /p/{pid}/hook answered {hit.brief()}")
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        row = next((m for m in inbox(pid, agent, token) if (m.get("from") or {}).get("type") == "endpoint" and nonce in m["body"]), None)
        if row:
            save(ingress_headers=json.loads(row["body"])["headers"])
            return f"unauthenticated POST /p/{pid}/hook → 202, delivered to the agent as an endpoint message ({row['state']})"
        time.sleep(1)
    raise Failed("the hit was accepted but never reached the agent's inbox")


@check
def forwarded_headers_overwritten():
    headers = need("ingress_headers")["ingress_headers"]
    client = headers.get("x-wheel-client-ip")
    leaked = {k: v for k, v in headers.items() if FORGED in v or "evil.example" in v}
    expect(not leaked, f"client-forged values reached wheeld's engine: {leaked}")
    expect(client, "no x-wheel-client-ip: the API vouched for nobody")
    expect(headers.get("x-forwarded-for") == client, f"X-Forwarded-For {headers.get('x-forwarded-for')!r} is not exactly the client {client!r}")
    expect(headers.get("x-real-ip") == client, f"X-Real-IP {headers.get('x-real-ip')!r}")
    expect("forwarded" not in headers, f"Forwarded: {headers.get('forwarded')!r}")
    expect(headers.get("x-forwarded-proto") == "https", f"X-Forwarded-Proto {headers.get('x-forwarded-proto')!r}")
    expect(headers.get("x-forwarded-host") == HOST, f"X-Forwarded-Host {headers.get('x-forwarded-host')!r}")
    return f"the engine saw X-Forwarded-For = X-Real-IP = x-wheel-client-ip = {client}, proto https, host {HOST}, no Forwarded; {FORGED} and evil.example appear nowhere"


@check
def body_limits():
    state = need("project", "session", "cookie")
    pid, token = state["project"], state["session"]
    hook = f"/p/{pid}/hook"
    over = request("POST", hook, {"content-type": "application/octet-stream"}, b"x" * (300 * 1024))
    expect(over.status == 413 and EDGE_413 in over.body, f"ingress 300 KiB: {over.brief()}")
    under = request("POST", hook, {"content-type": "text/plain"}, b"y" * (200 * 1024))
    expect(under.status == 202, f"ingress 200 KiB: {under.brief()}")
    # A body this far past the edge's 5 MiB limit (@other) is refused, but not always with a clean
    # 413: Caddy's request_body limiter is enforced as the body is STREAMED to reverse_proxy, not
    # checked against Content-Length upfront. A near-limit overage (the 300 KiB case above, 44 KiB
    # over 256 KiB) is caught almost immediately, before reverse_proxy has dialed the backend, and
    # gets a clean 413. A megabyte-scale overage gives reverse_proxy time to start forwarding
    # before the cutoff lands, and aborting an in-flight proxy read surfaces as either a 502 or a
    # hard close with no response at all — measured directly, both reproduce on repeated identical
    # 6 MiB POSTs to /v1/projects. Either way nothing over the limit ever succeeds, which is the
    # property that matters; the exact client-visible failure mode is not, so a raised connection
    # error counts the same as a 502 here.
    answered_by = {}
    for label, path, headers in (
        ("/v1", "/v1/projects", {"x-auth-token": token, "content-type": "application/json"}),
        ("the web proxy", "/api/wheel/v1/projects", {"cookie": state["cookie"], "origin": ORIGIN, "content-type": "application/json"}),
    ):
        try:
            reply = request("POST", path, headers, b" " * (6 * MiB))
        except (ConnectionError, TimeoutError) as e:
            answered_by[label] = f"a hard connection close ({type(e).__name__}: {e}) — a proxy-level refusal, not a clean 413"
            continue
        expect(reply.status in (413, 502) or (reply.status >= 400 and EDGE_413 not in reply.body), f"6 MiB to {label} succeeded: {reply.brief()}")
        answered_by[label] = "a clean edge 413" if EDGE_413 in reply.body else f"a {reply.status} (proxy-level refusal, not a clean 413 — see the comment above)"
    blob = f"{engine(pid)}/chests/{uuid.uuid4()}/blob?key=rehearsal.bin"
    carve_out = request("PUT", blob, {"x-auth-token": token, "content-type": "application/octet-stream"}, b"z" * (6 * MiB))
    expect(carve_out.status and EDGE_413 not in carve_out.body, f"a 6 MiB chest blob, under its 50 MiB limit, was refused at the edge: {carve_out.brief()}")
    return (
        "the edge refused a 300 KiB webhook, where its limit is the binding one, and passed 200 KiB (202); "
        f"6 MiB was refused on /v1 by {answered_by['/v1']} and on the web proxy by {answered_by['the web proxy']}; "
        f"a 6 MiB chest blob passed the edge's 50 MiB carve-out and wheeld answered {carve_out.status} "
        "(no chest storage and a 5 MiB proxy cap, so the 50 MiB ceiling cannot be reached end to end yet)"
    )


@check
def ingress_rate_limit_ignores_xff():
    pid = need("project")["project"]
    statuses = []
    for i in range(70):
        forged = f"10.66.{i // 200}.{i % 200 + 1}"
        statuses.append(request("POST", f"/p/{pid}/hook", {"content-type": "application/json", "x-forwarded-for": forged}, b"{}").status)
    accepted, limited = statuses.count(202), statuses.count(429)
    summary = f"{accepted}×202, {limited}×429, others {sorted(set(statuses) - {202, 429})}"
    expect(limited > 0 and accepted <= 60, f"70 hits with 70 different forged X-Forwarded-For values: {summary}")
    return f"70 hits with 70 different forged X-Forwarded-For values: {summary}; the engine's per-caller limit (60/min) saw one caller"


def main():
    name = sys.argv[1] if len(sys.argv) > 1 else ""
    if name not in CHECKS:
        print(f"checks.py: unknown check {name!r}; one of {', '.join(CHECKS)}", file=sys.stderr)
        return 2
    try:
        detail = CHECKS[name]()
    except Skipped as e:
        print(f"SKIP  {name} — {e}")
        return 3
    except Failed as e:
        print(f"FAIL  {name} — {e}")
        return 1
    except Exception as e:
        print(f"FAIL  {name} — {type(e).__name__}: {e}")
        return 1
    print(f"PASS  {name} — {detail}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
