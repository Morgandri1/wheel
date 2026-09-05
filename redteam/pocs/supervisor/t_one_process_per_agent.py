#!/usr/bin/env python3
"""§3c#13 — exactly one harness process per agent. Owner: SDK. → THREAT-MODEL TB5.

The defect this guards: on YOKE each delivered message launched another `claude --continue`, so N quick
messages = N processes of one agent editing one worktree at once. Wheel's invariant: a message NEVER
spawns; it enqueues, and the single supervised process consumes it when idle. Start is idempotent (a
second start while running returns the existing session). The contract's own test: 10 messages within
100 ms produce ONE process and 10 sequential turns.

Attack: blast 10 sends within 100 ms at a parked/stopped agent, and concurrently call start twice.
Secure outcome (from board state + QA's transcript):
  - the agent reports exactly ONE session_id, stable across the burst (no second session appears);
  - the transcript shows 10 turns delivered SERIALLY in that one session, not 10 first-turns;
  - concurrent starts return the same session_id (idempotent spawn under the per-agent mutex).
"""
import json, os, sys, threading, time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "lib"))
import harness as h

def run(_):
    tok, pid, aid = (os.environ.get(k) for k in ("WHEEL_TOKEN_A", "WHEEL_PROJECT_A", "WHEEL_AGENT_A"))
    if not (tok and pid and aid):
        return None  # staged: needs wheel-engine:test with a parked agent + fake harness

    base = f"/v1/projects/{pid}"
    sessions = []
    def start_once():
        st, _, body = h.req("POST", f"{base}/agents/{aid}/start", token=tok, project=pid)
        try: sessions.append(json.loads(body).get("session_id"))
        except Exception: pass
    # two concurrent starts (idempotency under the spawn mutex)
    ts = [threading.Thread(target=start_once) for _ in range(2)]
    for t in ts: t.start()
    for t in ts: t.join()

    # 10 sends within ~100 ms
    def send(i):
        h.req("POST", f"{base}/agents/{aid}/send", token=tok, project=pid,
              headers={"content-type": "application/json"}, body=json.dumps({"body": f"burst-{i}"}))
    burst = [threading.Thread(target=send, args=(i,)) for i in range(10)]
    t0 = time.time()
    for t in burst: t.start()
    for t in burst: t.join()
    elapsed_ms = (time.time() - t0) * 1000

    # read back the single session_id from board state
    st, _, body = h.req("GET", f"{base}/engine/v1/board", token=tok, project=pid)
    if st != 200:
        return None
    node = next((n for n in json.loads(body).get("nodes", []) if n.get("id") == aid), None)
    sid = ((node or {}).get("state") or {}).get("session_id")

    distinct_starts = set(s for s in sessions if s)
    if len(distinct_starts) > 1:
        return f"concurrent starts produced {len(distinct_starts)} sessions {distinct_starts} — spawn not idempotent (§3c#13)"
    # TODO(stack): assert via /v1/agents/{aid}/log that exactly 10 turns ran in session `sid`
    #   (10 `result` events, one session), not 10 process starts. Transcript corroborates order.
    if sid and distinct_starts and sid not in distinct_starts:
        return f"post-burst session_id {sid} differs from start session {distinct_starts} — a message spawned a new process"
    return None

if __name__ == "__main__":
    h.finish(run)
