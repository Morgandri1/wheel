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
import base64, json, os, re, subprocess, sys, time, uuid
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from wheel_client import Results, run_suite, free_port

SKIP = 77
R = Results()
# Unique per RUN, not just per suite. A fixed name means any second runner — another
# agent's session on this shared host, a stale container from a killed run — is removed by
# our own `docker rm -f` and removes ours by theirs. This run lost its engine mid-clone
# exactly that way. Suite-level uniqueness (qa/contract/suite_isolation.py) is not enough
# when the same suite can be running twice.
NAME = "qa-engine-wow-%s" % uuid.uuid4().hex[:8]
PORT = free_port(int(os.environ.get("WHEEL_ENGINE_WOW_PORT", "17426")))
BASE = "http://127.0.0.1:%d" % PORT
SECRET = "qa-wow-secret-at-least-16chars"
IMAGE = os.environ.get("WHEEL_ENGINE_IMAGE", "wheel-engine:test")
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


def http(method, path, body=None):
    import urllib.error, urllib.request
    r = urllib.request.Request(BASE + path, method=method)
    r.add_header("Authorization", "Bearer " + SECRET)
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        r.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(r, data, timeout=120) as resp:
            txt = resp.read().decode(errors="replace")
            return resp.status, (json.loads(txt) if txt.strip() else None)
    except urllib.error.HTTPError as e:
        txt = e.read().decode(errors="replace")
        try:
            return e.code, json.loads(txt)
        except Exception:
            return e.code, txt
    except urllib.error.URLError as e:
        # The engine went away mid-run. Report it as a value the caller can assert on; a
        # traceback here reads as "the test is broken" when the subject is what vanished.
        return 0, {"error": "engine unreachable: %s" % e.reason}


def node(name, typ, cfg, x=0):
    st, body = http("POST", "/v1/nodes", {"name": name, "type": typ,
                                          "position": {"x": x, "y": 0}, "config": cfg})
    return (body or {}).get("id"), st


def start_engine():
    sh("docker", "rm", "-f", NAME)
    key = sh("openssl", "rand", "-base64", "32").stdout.strip()
    p = sh("docker", "run", "-d", "--name", NAME,
           "-e", "WHEEL_PROJECT_ID=" + str(uuid.uuid4()),
           "-e", "WHEEL_ENGINE_SECRET=" + SECRET,
           "-e", "WHEEL_VAULT_KEY=" + key,
           "-e", "WHEEL_ROLE=engine",
           "-e", "WHEEL_LISTEN=tcp://0.0.0.0:7000",
           "-p", "%d:7000" % PORT, IMAGE)
    if p.returncode != 0:
        return "could not start %s: %s" % (IMAGE, p.stderr.strip()[:200])
    for _ in range(90):
        try:
            if http("GET", "/healthz")[0] == 200:
                return None
        except Exception:
            pass
        time.sleep(0.5)
    return "engine never became healthy"


def engine_postmortem():
    """Why the engine went away — asked while the container still exists.

    "Connection refused" has at least three causes here and they need different actions:
    the kernel OOM-killed it (this host runs six agents and a cargo build is not small),
    another session removed it, or it crashed. Guessing produced a bug report against the
    wrong person once already; the container knows, so ask it before cleanup does.
    """
    p = sh("docker", "inspect", "-f",
           "status={{.State.Status}} oom={{.State.OOMKilled}} exit={{.State.ExitCode}}", NAME)
    if p.returncode != 0:
        return "the container no longer exists — something outside this suite removed it"
    logs = sh("docker", "logs", "--tail", "5", NAME)
    return "%s; last log: %s" % (p.stdout.strip(),
                                 (logs.stdout + logs.stderr).strip()[-300:] or "(none)")


TURN_TIMEOUT = "<<TURN-DID-NOT-FINISH>>"


def log_cursor(agent):
    """The engine's `next` cursor, so a later read sees only what comes AFTER now."""
    _, log = http("GET", "/v1/agents/%s/log" % agent)
    return (log or {}).get("next", 0) if isinstance(log, dict) else 0


def turn(agent, command, wait=180):
    """Ask the agent to run a command, and return what THIS turn produced.

    The command travels as a directive inside a normal user message, so it goes through
    the real delivery path: queued, framed in an <AgentPrompt> envelope, written to the
    child's stdin by the single writer, executed by the child, and reported back through
    the engine's log. Nothing here reaches into the container.

    Two things this gets right that the first version did not:

    1. It reads from a cursor taken BEFORE the send. The old version searched the whole
       log for "exit=" -- which the PREVIOUS turn had already written. The build turn
       therefore returned instantly with the clone's output, and the suite reported
       "cargo test did not pass" while quoting the successful clone. A test that reads
       stale state does not fail; it reports the wrong thing confidently.

    2. A turn that does not finish in time returns TURN_TIMEOUT, not its partial output.
       "The turn never completed" and "the command failed" need different actions -- a
       longer timeout or a warm cache versus a code fix -- and returning partial output
       silently collapses them into the second.
    """
    b64 = base64.b64encode(command.encode()).decode()
    since = log_cursor(agent)
    http("POST", "/v1/agents/%s/send" % agent,
         {"body": "<<FAKE:SH_B64=%s>>" % b64})
    deadline = time.time() + wait
    seen = ""
    while time.time() < deadline:
        _, log = http("GET", "/v1/agents/%s/log?since=%s" % (agent, since))
        seen = json.dumps(log)
        if "engine unreachable" in seen:
            return seen
        # Only stdout carries a directive's result; the transcript stream echoes the
        # PROMPT, which contains the command and would match almost any marker.
        lines = (log or {}).get("lines") if isinstance(log, dict) else None
        for ln in lines or []:
            if ln.get("stream") == "stdout" and "exit=" in (ln.get("text") or ""):
                return json.dumps(lines)
        time.sleep(2)
    return TURN_TIMEOUT


def credential_scan(root, token):
    """What a credential looks like on disk under `root`, measured from OUTSIDE the agent.

    Three separate questions, because they fail independently and PM asked for all three:
    the literal token anywhere in the tree, a credential embedded in a remote URL, and
    what `git remote -v` actually prints. Run by `docker exec`, not by the agent: an agent
    asked to search for its own leaked secret is the wrong witness.
    """
    literal = sh("docker", "exec", NAME, "sh", "-c",
                 "grep -rlF -- '%s' %s 2>/dev/null | head -5" % (token, root)).stdout.strip()
    cfg = sh("docker", "exec", NAME, "sh", "-c",
             "cat %s/.git/config 2>/dev/null" % root).stdout
    remotes = sh("docker", "exec", NAME, "sh", "-c",
                 "cd %s 2>/dev/null && git remote -v" % root).stdout
    # `https://user:secret@host` in any of them. Matches the shape, not one token, so a
    # DIFFERENT credential than the one we planted is still caught.
    embedded = re.findall(r"https://[^/\s]*:[^/@\s]+@", cfg + remotes)
    return {"literal_files": literal, "config": cfg, "remotes": remotes,
            "embedded": embedded,
            "clean": not literal and not embedded and token not in cfg + remotes}



def main():
    if os.environ.get("WHEEL_WOW") != "1":
        print("wheel-on-wheel is opt-in: set WHEEL_WOW=1 (it clones and compiles; minutes, "
              "not seconds, so it is not in the default gate)")
        return SKIP
    if subprocess.run(["docker", "info"], capture_output=True).returncode != 0:
        print("docker not running")
        return SKIP
    image = IMAGE
    if subprocess.run(["docker", "image", "inspect", image], capture_output=True).returncode != 0:
        print("%s not built — run `make engine-image-test`" % image)
        return SKIP

    absent = missing_tools(image)
    if absent:
        # The whole point of the test is that a real agent can do real work in the sandbox.
        # Without the toolchain there is nothing to measure, and a green here would mean
        # only that we successfully did nothing.
        for tid in ("WOW-clone", "WOW-vault-token", "WOW-cargo-test", "WOW-no-token-in-log",
                    "WOW-token-not-on-disk", "WOW-commit", "WOW-commit-push",
                    "WOW-token-not-on-disk-after-push"):
            R.skip(tid, "sandbox image lacks %s — needs SDK's toolchain image" % ", ".join(absent))
        return R.report("wheel-on-wheel")

    print("toolchain present (%s) — running the real thing" % ", ".join(TOOLCHAIN))

    token = os.environ.get("WHEEL_WOW_GH_TOKEN") or sh("gh", "auth", "token").stdout.strip()
    if not token:
        for tid in ("WOW-vault-token", "WOW-clone", "WOW-cargo-test", "WOW-no-token-in-log",
                    "WOW-token-not-on-disk", "WOW-commit", "WOW-commit-push",
                    "WOW-token-not-on-disk-after-push"):
            R.skip(tid, "no GitHub token: set WHEEL_WOW_GH_TOKEN or run `gh auth login`")
        return R.report("wheel-on-wheel")

    err = start_engine()
    if err:
        print(err)
        return SKIP

    vault, _ = node("ghsecrets", "vault", {"keys": ["GH_TOKEN"]})
    agent, st = node("builder", "agent",
                     {"harness": "claude", "system_prompt": "You build Wheel.",
                      "run_on_startup": False, "ephemeral_context": False}, x=200)
    if not R.check("WOW/setup", agent and vault, "node creation -> %s" % st):
        return R.report("wheel-on-wheel")

    # The credential reaches the agent the way the product intends: a vault node, a
    # read wire, exported at spawn. No token is passed on a command line anywhere.
    http("POST", "/v1/wires", {"from": agent, "to": vault, "type": "read"})
    st, _ = http("PUT", "/v1/vault/%s/GH_TOKEN" % vault, {"value": token})
    if not R.check("WOW-vault-token", 200 <= st < 300, "vault write answered %s" % st):
        return R.report("wheel-on-wheel")

    http("POST", "/v1/agents/%s/start" % agent)
    time.sleep(3)

    # `git clone` reads GH_TOKEN out of the environment via the credential helper, so
    # the secret is never in argv (argv is world-readable across uids — contract §5b).
    clone = ("set -e; cd /data; rm -rf wow; "
             "git config --global credential.helper "
             "'!f(){ echo username=x-access-token; echo password=$GH_TOKEN; };f'; "
             "git clone --depth 1 %s wow 2>&1 | tail -3; "
             "test -f wow/Cargo.toml && echo CLONE_OK" % REPO)
    out = turn(agent, clone, wait=180)
    vanished = "engine unreachable" in out
    if vanished:
        # The engine died; that says nothing about whether an agent can clone.
        R.skip("WOW-clone", "the engine went away mid-run — %s" % engine_postmortem())
    elif out == TURN_TIMEOUT:
        R.skip("WOW-clone", "the clone turn did not finish within 180s — no verdict on "
                            "whether an agent can clone, only that it did not this time")
    else:
        R.check("WOW-clone", "CLONE_OK" in out,
                "the agent could not clone with its vault token: %s" % out[-400:])

    # ---- PM 15:55 (S1): a live GitHub PAT was found in plaintext in .git/config -------
    #
    # Found by the cloud adversary on the production volume and verified by PM. The clone
    # put the vault token in the REMOTE URL, so it landed in `.git/config`, which is
    # world-readable to every uid in the sandbox that can reach the workspace, survives the
    # process, and gets copied wherever the workspace is copied. The token in this suite
    # reaches git through a credential helper reading the environment instead, so this leg
    # is what proves that difference is real rather than intended.
    #
    # The control first: a detector that reports "no credential on disk" is only evidence
    # if it would have said otherwise. So a credential is PLANTED in a scratch repo, in
    # exactly the shape the production exposure had, and the same detector must flag it.
    # Without this, a typo in the grep, a wrong path or an empty token all read as green —
    # and "we looked and found nothing" is the single easiest false green to ship.
    planted = "ghp_" + "P1anted" * 5
    sh("docker", "exec", NAME, "sh", "-c",
       "rm -rf /data/detector && mkdir -p /data/detector && cd /data/detector && "
       "git init -q . && git remote add origin "
       "https://x-access-token:%s@github.com/example/repo.git" % planted)
    caught = credential_scan("/data/detector", planted)
    R.control("WOW/credential-detector-works", not caught["clean"] and bool(caught["embedded"]),
              "the detector did NOT flag a credential deliberately written into "
              ".git/config, so it cannot be trusted to report the absence of one. "
              "config=%r remotes=%r" % (caught["config"][-200:], caught["remotes"][-120:]))
    sh("docker", "exec", NAME, "sh", "-c", "rm -rf /data/detector")

    if "CLONE_OK" not in out:
        R.skip("WOW-token-not-on-disk", "nothing was cloned, so there is no workspace to "
                                        "search — an absence here would mean only that")
    else:
        found = credential_scan("/data/wow", token)
        R.gated("WOW-token-not-on-disk", "WOW/credential-detector-works", found["clean"],
                "the GitHub token is on disk in the workspace after clone. Files "
                "containing it: %s. Credentialed remote URLs: %s. This is a live PAT "
                "readable by every process that can reach the workspace, and it outlives "
                "the agent that used it: `git remote -v` -> %r"
                % (found["literal_files"] or "none", found["embedded"] or "none",
                   found["remotes"][-160:]))

    # ---- PM 15:48: the third leg — commit & push -------------------------------------
    #
    # Clone proves a vault token authenticates a READ. The operator's goal is Wheel
    # developing itself, which needs a WRITE, and until now nobody had ever run one: seven
    # PRs were opened by cloud agents today on a capability with no gate under it.
    #
    # Pushes go to a throwaway branch named for the run, never main and never a role
    # branch, and it is deleted in the same run whatever the outcome.
    #
    # THE CONTROL IS THE TEST. A push that succeeds because the RUNNER is logged in, or
    # because a credential is cached in the image, proves nothing about the sandbox — and
    # that is the exact shape that produced three false greens today. So the same push is
    # attempted FIRST with the token removed from the environment, and it must FAIL. If it
    # succeeds, some other credential is doing the work and the real result is worthless.
    branch = "wow/%s" % uuid.uuid4().hex[:12]
    if "CLONE_OK" not in out:
        for tid in ("WOW-commit", "WOW-commit-push", "WOW-push-needs-the-token"):
            R.skip(tid, "nothing was cloned, so there is no workspace to commit in")
    else:
        # A tracked file, not creds/ (that is A3), and content that is obviously test
        # residue if a branch ever escapes deletion.
        edit = ("set -e; cd /data/wow; "
                "git config user.email 'qa@wheel.test'; git config user.name 'Wheel QA'; "
                "echo 'wheel-on-wheel probe %s' >> README.md; "
                "git add README.md; "
                "git commit -q -m 'qa: wheel-on-wheel push probe (%s)' && echo COMMIT_OK; "
                "git rev-parse --short HEAD" % (branch, branch))
        cout = turn(agent, edit, wait=120)
        if cout == TURN_TIMEOUT or "engine unreachable" in cout:
            R.skip("WOW-commit", "the commit turn did not finish — %s" % cout[:120])
        else:
            R.check("WOW-commit", "COMMIT_OK" in cout,
                    "the agent could not commit in its own workspace. Git identity is the "
                    "usual cause and it must fail legibly, not silently: %s" % cout[-400:])

        # Control: the same push, with the token taken out of the environment.
        # `git push ... | tail -3; echo RC=$?` reports TAIL's exit code, which is always 0.
        # My first version did exactly that, and the control below caught it by "proving"
        # a push works with no credential at all. The exit code has to be taken from git
        # itself, before anything else runs.
        push_cmd = ("cd /data/wow && %s git push origin HEAD:%s > /tmp/push.out 2>&1; "
                    "rc=$?; tail -3 /tmp/push.out; echo RC=$rc")
        denied = turn(agent, push_cmd % ("env -u GH_TOKEN", branch), wait=120)
        pushed_without_token = "RC=0" in denied
        R.control("WOW-push-needs-the-token", not pushed_without_token,
                  "a push SUCCEEDED with the vault token removed. Some other credential is "
                  "authenticating — a cached helper, an ambient git config, or a token "
                  "baked into the image — so a green push leg would say nothing about "
                  "whether the vault token works. Output: %s" % denied[-300:])

        pout = turn(agent, push_cmd % ("", branch), wait=180)
        # "git push exited 0" and "the ref is on GitHub" are different claims, and PM was
        # explicit that only one of them is the operator's goal. Ask the REMOTE.
        lsr = turn(agent, "cd /data/wow && git ls-remote origin %s 2>&1 | tail -2" % branch,
                   wait=120)
        on_remote = branch in lsr

        if pout == TURN_TIMEOUT or "engine unreachable" in pout:
            R.skip("WOW-commit-push", "the push turn did not finish — no verdict")
        else:
            R.gated("WOW-commit-push", "WOW-push-needs-the-token",
                    "RC=0" in pout and on_remote,
                    "an agent in the sandbox could not push to %s with its vault token. "
                    "This is the third leg of the operator's goal and it is already "
                    "happening in production, so a failure here is a gap in the gate, not "
                    "in the capability. push -> %s; the remote's own view of the branch "
                    "-> %r" % (REPO, pout[-300:], lsr[-160:]))

        # PM's S1, applied after the WRITE as well: pushing must not leave a credential
        # behind either, and a push is where git is most tempted to persist one.
        after_push = credential_scan("/data/wow", token)
        R.gated("WOW-token-not-on-disk-after-push", "WOW/credential-detector-works",
                after_push["clean"],
                "the GitHub token is on disk after the push (it was not after the clone, "
                "so the push wrote it): files=%s remotes=%r"
                % (after_push["literal_files"] or "none", after_push["remotes"][-160:]))

        # Clean up whatever we managed to create, on every path out.
        turn(agent, "cd /data/wow && git push origin --delete %s 2>&1 | tail -2" % branch,
             wait=120)

    if os.environ.get("WHEEL_WOW_SKIP_BUILD") == "1":
        # The clone leg proves the interesting half (a vault secret reaching an agent
        # and authenticating a real remote); `cargo test` proves the sandbox is a build
        # environment. On a host with six agents resident the build gets OOM-killed, so
        # it is separable — but only ever by SKIPPING it, never by assuming it.
        R.skip("WOW-cargo-test", "WHEEL_WOW_SKIP_BUILD=1")
    elif not vanished and "CLONE_OK" in out:
        # Keep cargo's exit status without a pipe and without pipefail.
        #
        # `cargo test | tail` reports TAIL's status, so a failed build looked like exit=0 —
        # that is how the rustup gap first read as a successful command. My fix for it was
        # `set -o pipefail`, which is a BASH-ism: the fake harness runs the directive under
        # /bin/sh, which is dash in this image, and it answered "Illegal option -o pipefail"
        # with exit=2. I fixed an honesty bug with a portability bug.
        #
        # Redirect, then report, then exit with the status that was actually cargo's. POSIX,
        # no pipe, no shell-specific options.
        build = ("cd /data/wow && cargo test -p wheel-core > /tmp/wow-build.log 2>&1; "
                 "rc=$?; tail -15 /tmp/wow-build.log; exit $rc")
        out = turn(agent, build, wait=1800)
        if out == TURN_TIMEOUT:
            # A cold `cargo test` inside a fresh sandbox builds every dependency from
            # source. Not finishing in 30 minutes says the build is slow, not that the
            # code is broken, and reporting it as a failed test sends its owner to the
            # wrong place entirely.
            R.skip("WOW-cargo-test",
                   "the build turn did not finish within 1800s — a cold dependency build "
                   "in a fresh sandbox; raise the timeout or warm the cargo cache")
        else:
            R.check("WOW-cargo-test", "test result: ok" in out,
                    "cargo test -p wheel-core did not pass inside the sandbox: %s"
                    % out[-600:])
    else:
        R.skip("WOW-cargo-test", "nothing was cloned, so there is nothing to build")

    # ---- ADVERSARY 029 / PM ruling: WHERE the toolchain vars point, not just that they exist.
    #
    # Membership in the F015 allowlist is not safety. RUSTUP_HOME may be inherited ONLY because
    # it points at an immutable, read-only toolchain outside the data dir: a toolchain a child
    # can WRITE is a toolchain a child can replace, and every later project then builds with
    # whatever it left there. CARGO_HOME is the opposite case and must never be inherited —
    # what a tenant FETCHES is not immutable, and a shared one puts one project's downloaded
    # sources, and any registry credentials it configures, where the next project can read them.
    #
    # qa/contract/env_allowlist.py pins the RULING; this asserts the RUNNING system obeys it.
    dump = sh("docker", "exec", NAME, "sh", "-c",
              "cat /data/wheel-fake-env.jsonl 2>/dev/null || true").stdout
    recs = [json.loads(l) for l in dump.splitlines() if l.strip()]
    env_of = {}
    for r in recs:
        env_of.update(r.get("config") or {})
    child_names = set()
    for r in recs:
        child_names.update(r.get("env_names") or [])

    if not recs:
        for tid in ("WOW-toolchain-rustup-readonly", "WOW-toolchain-cargo-per-project"):
            R.skip(tid, "no child spawned, so there is no environment to inspect")
    else:
        rustup = env_of.get("RUSTUP_HOME")
        if rustup is None and "RUSTUP_HOME" not in child_names:
            R.skip("WOW-toolchain-rustup-readonly",
                   "RUSTUP_HOME not yet inherited (ADVERSARY 029, SDK lands it after ingress)")
        else:
            probe = sh("docker", "exec", NAME, "sh", "-c",
                       "d=%s; [ -d \"$d\" ] && stat -c '%%a %%u' \"$d\" || echo missing"
                       % (rustup or "/opt/rust/rustup"))
            info = probe.stdout.strip()
            mode = info.split()[0] if info and info != "missing" else ""
            R.check("WOW-toolchain-rustup-readonly",
                    (rustup or "").startswith("/opt/") and not (rustup or "").startswith("/data")
                    and bool(mode) and mode[-2:] in ("55", "44", "05", "00", "50", "40"),
                    "RUSTUP_HOME=%r stat=%r — it must sit in the image's read-only toolchain "
                    "dir, never under /data or a project dir, and never be writable by the "
                    "child uids" % (rustup, info))

        cargo = env_of.get("CARGO_HOME")
        if cargo is None:
            R.skip("WOW-toolchain-cargo-per-project",
                   "the fake harness did not record CARGO_HOME for this spawn")
        else:
            probe = sh("docker", "exec", NAME, "sh", "-c",
                       "[ -d \"%s\" ] && stat -c '%%a' \"%s\" || echo missing" % (cargo, cargo))
            mode = probe.stdout.strip()
            # Two separate claims, and only one of them is backend-independent.
            #
            # PER-PROJECT: 029 words it as "/data/projects/<id>/.cargo", which is the PROCESS
            # backend's layout. Under docker there is one engine per project with its own
            # volume, so the engine's own data dir is already project-scoped and /data/cargo
            # satisfies the requirement. Asserting the literal path would fail a correct
            # docker deployment, so what is checked is that it sits under the engine's data
            # dir rather than somewhere shared between engines.
            #
            # NOT GROUP/OTHER-ACCESSIBLE: this one holds everywhere and is the part that
            # matters, because §2 puts every agent, script and MCP child under its OWN uid
            # inside the sandbox. A 0755 cache is readable by every one of them, and what a
            # tenant fetches — sources, and any registry credentials it configures — is
            # exactly what must not be.
            under_data = cargo.startswith("/data")
            private = mode.endswith("00") and len(mode) == 3
            R.check("WOW-toolchain-cargo-per-project", under_data and private,
                    "CARGO_HOME=%r mode=%r — it must live under the project's own data dir "
                    "and be private to the project uid (0700). 755 is readable by every other "
                    "uid in the sandbox, and §2 gives each agent, script and MCP child its "
                    "own." % (cargo, mode))

    # ---- PM: one toolchain per project, and workspaces away from the secrets ------
    #
    # TOOLCHAIN SHARING (N agent nodes -> ONE toolchain, not N). Asserted on the paths the
    # children were actually given, and on bytes: the toolchain dir must be the same for
    # every child and must not have been copied per node. A per-node toolchain is ~1 GB
    # each and would not fail anything — it would just quietly fill the volume.
    homes = {}
    for r in recs:
        cfg = r.get("config") or {}
        if cfg.get("RUSTUP_HOME"):
            homes.setdefault(cfg["RUSTUP_HOME"], 0)
            homes[cfg["RUSTUP_HOME"]] += 1
    if not homes:
        R.skip("WOW-toolchain-shared", "no child recorded RUSTUP_HOME")
    else:
        du = sh("docker", "exec", NAME, "sh", "-c",
                "du -s -m /opt/rust 2>/dev/null | cut -f1; "
                "find /data -maxdepth 3 -name 'toolchains' -type d 2>/dev/null | wc -l")
        parts = du.stdout.split()
        copies_under_data = parts[1] if len(parts) > 1 else "?"
        R.check("WOW-toolchain-shared",
                len(homes) == 1 and copies_under_data == "0",
                "children were given %d distinct RUSTUP_HOME values (%s) and %s toolchain "
                "trees exist under /data — N agents on one project must share ONE toolchain. "
                "A per-node copy is about a gigabyte each and fails nothing; it just fills "
                "the volume." % (len(homes), sorted(homes), copies_under_data))

    # WORKSPACE vs CREDENTIALS. supervisor/mod.rs:422 sets cwd to the data dir ROOT, and
    # creds_dir() is data_dir/creds — so an agent's `git clone` lands in the PARENT of the
    # directory holding every node's credentials. PM's rule: a build artifact next to
    # secrets is its own finding, and the working copy belongs under ws/<name>.
    cwds = {r.get("cwd") for r in recs if r.get("cwd")}
    creds = sh("docker", "exec", NAME, "sh", "-c",
               "ls -d /data/creds 2>/dev/null || echo none").stdout.strip()
    if not cwds:
        R.skip("WOW-workspace-not-in-creds", "the harness did not record a working directory")
    else:
        bad = [c for c in cwds
               if creds != "none" and (creds.startswith(c.rstrip("/") + "/") or c == creds)]
        R.check("WOW-workspace-not-in-creds", not bad,
                "the agent's working directory %s contains the credentials directory %s — a "
                "clone or a build artifact lands in the same tree as every node's stored "
                "credentials. §3e puts a working copy under /data/projects/<id>/ws/<name>."
                % (sorted(bad), creds))

    # The token travelled through the engine; it must not have been written down.
    #
    # Gated on the log being READABLE. An unreachable engine returns an error body with
    # no token in it, and "the token is not in this error message" is not the claim.
    # This assertion reported green against a dead engine on its first run, which is
    # the exact shape of vacuous pass this suite exists to avoid.
    st, log = http("GET", "/v1/agents/%s/log" % agent)
    if st == 0:
        R.skip("WOW-no-token-in-log", "the agent log could not be read, so its absence "
                                      "from the log proves nothing")
    else:
        R.check("WOW-no-token-in-log", token not in json.dumps(log),
                "the GitHub token is in the agent log")
    # Teardown is run_suite()'s, not this function's.
    #
    # run_suite saves the engine's own log before removing the container, which only
    # works if nothing removed it first. With a teardown here too, the failure artifact
    # was written after the container was already gone: the one run anybody would ever
    # want to read it for produced 70 bytes of "No such container".

    return R.report("wheel-on-wheel")


def _cleanup():
    subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)


if __name__ == "__main__":
    sys.exit(run_suite(main, "wheel-on-wheel", _cleanup, container=NAME))
