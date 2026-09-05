import Link from "next/link";
import "@/styles/landing.css";

/**
 * The landing page, in the "Modernist" system from the Claude Design project —
 * a printed poster, deliberately not the board's dark instrument panel.
 *
 * Two departures from the mockup, both on purpose:
 *  - the two image slots are drawn rather than photographed. The system prints in
 *    black, white and one red, so a screenshot would arrive as grey mush; the board
 *    and the run record are set in the system's own marks instead, which also means
 *    the page cannot go stale against a UI it is a picture of.
 *  - the invented customer testimonial is gone. Everything else on this page is a
 *    claim we can stand behind; a quote attributed to a person who does not exist
 *    is not, whatever the mockup's placeholder said.
 */

export const metadata = {
  title: "Wheel — a multi-agent harness",
  description:
    "Swarms of Claude and Codex agents, drawn as nodes and wires on a cloud board, building while you sleep.",
};

export default function Landing() {
  return (
    <div className="modernist">
      <nav className="nav">
        <span className="nav-brand">
          <WheelMark />
          Wheel
        </span>
        <a href="#product" aria-current="location">
          Product
        </a>
        <a href="#record">Record</a>
        <a href="#pricing">Pricing</a>
        <a href="#start">Start</a>
        <Link href="/app" className="btn btn-primary" data-testid="cta-nav">
          Open the board
        </Link>
      </nav>

      <div className="wrap">
        <section className="hero">
          <h1 className="display">
            <span className="line">Everyone told us not to reinvent the wheel.</span>{" "}
            <span className="line">We didn&rsquo;t listen.</span>
          </h1>
          <p className="sub">
            Wheel is a multi-agent harness: swarms of Claude and Codex agents, drawn as nodes and
            wires on a cloud board, building while you sleep. You hold the wheel; it does the
            turning.
          </p>
          <div className="row">
            <Link href="/app" className="btn btn-primary" data-testid="cta-app">
              Open the board
            </Link>
            <a className="btn btn-ghost" href="#product">
              See how it works
            </a>
          </div>
        </section>

        <section className="shot" aria-label="The Wheel board">
          <figure className="shot-frame">
            <BoardStill />
            <figcaption>
              <span>wheel.dev/app — the board</span>
              <span>fig. 01</span>
            </figcaption>
          </figure>
        </section>

        <hr className="rule2" />

        <section className="stats" aria-label="Wheel, by the numbers">
          <div className="grid">
            <div>
              <p className="stat-num">9</p>
              <p className="stat-label">Node types on the board</p>
            </div>
            <div>
              <p className="stat-num">2</p>
              <p className="stat-label">Harnesses — Claude &amp; Codex</p>
            </div>
            <div>
              <p className="stat-num">$0</p>
              <p className="stat-label">Cost of a parked agent</p>
            </div>
            <div>
              <p className="stat-num">1</p>
              <p className="stat-label">Wheel, reinvented</p>
            </div>
          </div>
        </section>

        <hr className="rule2" />

        <section className="features" id="product">
          <span className="kicker">What Wheel does</span>
          <div className="feature">
            <p className="f-num">01</p>
            <h2 className="f-title">The board</h2>
            <p className="f-copy">
              Swarms are built in the cloud webapp: agents, vaults, tables and endpoints as nodes,
              wires drawn between them. What you see is the whole system.
            </p>
          </div>
          <div className="feature">
            <p className="f-num">02</p>
            <h2 className="f-title">Wires are permissions</h2>
            <p className="f-copy">
              An agent touches only what it is wired to: read, write or send, checked on every call.
              Revoking access is deleting a line.
            </p>
          </div>
          <div className="feature">
            <p className="f-num">03</p>
            <h2 className="f-title">Parked, not paid</h2>
            <p className="f-copy">
              Idle agents park after five minutes and resume exactly where they stopped. You bring
              your own Claude and Codex plans; Wheel bills only the compute, network and disk your
              swarm uses inside the cluster.
            </p>
          </div>
        </section>

        <section className="split" id="how">
          <div className="split-copy">
            <span className="kicker">How it works</span>
            <h2 className="split-title">Nodes, wires, one board</h2>
            <p className="note">
              A swarm is agents plus the things they work with: vaults for secrets, tables for
              memory, endpoints for HTTP in, scripts and tools for the rest. Every arrow is a wire —
              the only permission system there is.
            </p>
          </div>
          <figure className="split-figure">
            <SwarmDiagram />
            <figcaption className="dg-key">
              Wire types — send: prompt it · read: access its data · write: mutate it
            </figcaption>
          </figure>
        </section>

        <section className="split" id="code">
          <div className="split-copy">
            <span className="kicker">The source</span>
            <h2 className="split-title">JSON underneath</h2>
            <p className="note">
              Every node on the board is a JSON document: exportable, diffable, reviewed like the
              code it is. Draw it in the webapp; commit it in the repository.
            </p>
          </div>
          <div className="codebox">
            <pre>
              {`{
  `}
              <span className="jk">&quot;name&quot;</span>
              {`: "reviewer",
  `}
              <span className="jk">&quot;type&quot;</span>
              {`: "agent",
  `}
              <span className="jk">&quot;wires&quot;</span>
              {`: [
    { `}
              <span className="jk">&quot;to&quot;</span>
              {`: "planner", `}
              <span className="jk">&quot;type&quot;</span>
              {`: "send" },
    { `}
              <span className="jk">&quot;to&quot;</span>
              {`: "notes",   `}
              <span className="jk">&quot;type&quot;</span>
              {`: "read" }
  ],
  `}
              <span className="jk">&quot;config&quot;</span>
              {`: {
    `}
              <span className="jk">&quot;harness&quot;</span>
              {`: "claude",
    `}
              <span className="jk">&quot;system_prompt&quot;</span>
              {`: "Review every diff. Be stricter than the humans are willing to be.",
    `}
              <span className="jk">&quot;run_on_startup&quot;</span>
              {`: true,
    `}
              <span className="jk">&quot;idle_timeout_secs&quot;</span>
              {`: 300,
    `}
              <span className="jk">&quot;budget&quot;</span>
              {`: { `}
              <span className="jk">&quot;max_usd&quot;</span>
              {`: 5.0 }
  }
}`}
            </pre>
          </div>
        </section>

        <section className="split" id="cli">
          <div className="split-copy">
            <span className="kicker">The CLI</span>
            <h2 className="split-title">The same wheel, from a shell</h2>
            <p className="note">
              Agents drive the board through the wheel CLI — and so can you. Every command is
              checked against the wires; a denial exits 3 and says exactly why.
            </p>
          </div>
          <div className="codebox">
            <pre>
              <span className="cli-p">$</span>
              {` wheel msg reviewer --file plan.md
`}
              <span className="cli-out">✓ queued · sha256 9c41…e2 · 1,204 bytes</span>
              {`
`}
              <span className="cli-p">$</span>
              {` wheel read notes/standup
`}
              <span className="cli-p">$</span>
              {` wheel secret get vault/deploy_key
`}
              <span className="cli-out">
                no wire from reviewer to vault (need: read) — exit 3
              </span>
            </pre>
          </div>
        </section>

        <section className="split" id="record">
          <div className="split-copy">
            <span className="kicker">The record</span>
            <h2 className="split-title">Every run on the record</h2>
            <p className="note">
              Prompts, diffs, costs, approvals — each run prints a complete trail, legible in black
              and white. When something goes wrong at 3 a.m., you read the record, not the tea
              leaves.
            </p>
          </div>
          <figure className="split-figure shot-frame">
            <RunRecord />
            <figcaption>
              <span>planner · session 4f2a</span>
              <span>fig. 02</span>
            </figcaption>
          </figure>
        </section>

        <section className="pricing" id="pricing">
          <span className="kicker">Pricing</span>
          <h2 className="split-title">Bring your own plan. Pay for the cluster.</h2>
          <p className="note">
            There are no seats and no token markup. Agents sign in with your own Claude and Codex
            plans, so the model does its thinking on the subscription you already pay for. Wheel
            bills only what your swarm actually uses inside the cluster — compute, network and
            disk — and a parked agent uses none of the three.
          </p>
          <div className="byo">
            <span className="tag tag-outline">Your Claude plan</span>
            <span className="tag tag-outline">Your Codex plan</span>
            <span className="tag tag-outline">No token markup</span>
          </div>
          <p className="note">Rates are not set yet. They will be published here before anyone is charged.</p>
        </section>
      </div>

      <section className="close" id="start">
        <div className="wrap">
          <h2>
            <span className="line">Reinvent the wheel.</span>
          </h2>
          <div className="row">
            <Link href="/app" className="btn btn-ghost" data-testid="cta-close">
              Open the board
            </Link>
          </div>
        </div>
      </section>

      <div className="wrap">
        <footer>
          <span>© 2026 Wheel — reinvented, on purpose.</span>
          <span>wheel.dev</span>
        </footer>
      </div>
    </div>
  );
}

/**
 * Spoke endpoints, rounded to three decimals.
 *
 * The raw trig gives values like 13.199999999999999, and React compares the server's string to
 * the client's when hydrating — any difference in how the two serialise the same float is a
 * hydration mismatch and a console error on every load. Fixing the precision makes the two
 * identical by construction rather than by luck.
 */
function spoke(degrees: number, radius: number) {
  const radians = (degrees * Math.PI) / 180;
  return {
    x: Number((12 + radius * Math.cos(radians)).toFixed(3)),
    y: Number((12 + radius * Math.sin(radians)).toFixed(3)),
  };
}

/** The mark: a hub with spokes ending in connection points. */
function WheelMark() {
  return (
    <svg width={20} height={20} viewBox="0 0 24 24" fill="none" aria-hidden>
      <circle cx="12" cy="12" r="9.25" stroke="currentColor" strokeWidth="1.6" />
      <circle cx="12" cy="12" r="2.4" stroke="currentColor" strokeWidth="1.6" />
      {[0, 60, 120, 180, 240, 300].map((a) => (
        <line
          key={a}
          x1={spoke(a, 2.4).x}
          y1={spoke(a, 2.4).y}
          x2={spoke(a, 9.25).x}
          y2={spoke(a, 9.25).y}
          stroke="currentColor"
          strokeWidth="1.2"
        />
      ))}
    </svg>
  );
}

/**
 * The board, drawn in the page's own marks. Not a screenshot: the system prints in
 * black, white and one red, and a photograph of a dark UI arrives as grey mush.
 */
function BoardStill() {
  return (
    <div className="board" role="img" aria-label="The Wheel board: a context node wired into a planner agent, which sends to a builder agent and writes to a findings table">
      <ul className="board-rail" aria-hidden>
        <li>Agent</li>
        <li>Context</li>
        <li>Table</li>
        <li>Endpoint</li>
        <li>Vault</li>
        <li>Tool</li>
      </ul>

      <div className="board-canvas" aria-hidden>
        <svg
          viewBox="0 0 520 300"
          preserveAspectRatio="none"
          style={{ position: "absolute", inset: 0, width: "100%", height: "100%" }}
        >
          <path d="M118 96 C 170 96, 170 132, 214 132" fill="none" stroke="#ec3013" strokeWidth="2" strokeDasharray="1 5" strokeLinecap="round" />
          <path d="M322 132 C 366 132, 366 212, 408 212" fill="none" stroke="#201e1d" strokeWidth="2" strokeDasharray="5 4" />
          <path d="M322 148 C 360 148, 360 66, 404 66" fill="none" stroke="#201e1d" strokeWidth="2.6" />
        </svg>

        <div className="plate is-data" style={{ left: "3%", top: "22%" }}>
          <b>house-style</b>
          <span>ctx · injected</span>
        </div>
        <div className="plate is-live" style={{ left: "40%", top: "36%" }}>
          <b>planner</b>
          <span>agent · claude · running</span>
        </div>
        <div className="plate is-data" style={{ left: "76%", top: "14%" }}>
          <b>findings</b>
          <span>table · 2 rows</span>
        </div>
        <div className="plate" style={{ left: "76%", top: "62%" }}>
          <b>builder</b>
          <span>agent · codex · parked</span>
        </div>
      </div>

      <dl className="board-inspector" aria-hidden>
        <dt>Node</dt>
        <dd>planner</dd>
        <dt>Harness</dt>
        <dd>Claude</dd>
        <dt>Status</dt>
        <dd>Running</dd>
        <dt>Wires</dt>
        <dd>send → builder</dd>
        <dd>write → findings</dd>
        <dt>Budget</dt>
        <dd>$5.00 / run</dd>
      </dl>
    </div>
  );
}

/** A run record, set as the record it is rather than photographed. */
function RunRecord() {
  const lines: { at: string; who: string; text: string; deny?: boolean }[] = [
    { at: "03:14:02", who: "engine", text: "session started · house-style injected (412 chars)" },
    { at: "03:14:02", who: "user", text: "ship the orbital-decay brief" },
    { at: "03:14:09", who: "planner", text: "wheel write findings/orbital-decay" },
    { at: "03:14:11", who: "planner", text: "wheel msg builder --file brief.md" },
    { at: "03:14:11", who: "engine", text: "queued · sha256 9c41…e2 · 1,204 bytes" },
    { at: "03:14:12", who: "planner", text: "no wire from planner to vault (need: read) — exit 3", deny: true },
    { at: "03:19:12", who: "engine", text: "idle 300s · parked, session kept" },
  ];

  return (
    <div className="record">
      {lines.map((l) => (
        <div key={`${l.at}-${l.text}`}>
          <time>{l.at}</time>
          <span className="who">{l.who}</span>
          <span className={l.deny ? "deny" : undefined}>{l.text}</span>
        </div>
      ))}
    </div>
  );
}

/** The swarm, exactly as the mockup drew it. */
function SwarmDiagram() {
  return (
    <svg
      className="dg"
      viewBox="0 0 680 470"
      role="img"
      aria-label="A Wheel swarm: a planner agent wired to reviewer and builder agents, a vault, a table, an endpoint, a script and an MCP server"
    >
      <line className="wire" x1="320" y1="93" x2="320" y2="205" />
      <line className="wire" x1="320" y1="265" x2="320" y2="392" />
      <line className="wire" x1="555" y1="145" x2="555" y2="207" />
      <line className="wire" x1="372" y1="217" x2="503" y2="133" />
      <line className="wire" x1="372" y1="253" x2="503" y2="337" />
      <line className="wire" x1="268" y1="217" x2="156" y2="133" />
      <line className="wire" x1="268" y1="253" x2="156" y2="337" />
      <text className="lbl" x="330" y="155">send</text>
      <text className="lbl" x="330" y="335">read · run</text>
      <text className="lbl" x="565" y="182">read</text>
      <text className="lbl" x="424" y="163">send</text>
      <text className="lbl" x="424" y="308">send</text>
      <text className="lbl" x="196" y="163">read</text>
      <text className="lbl" x="188" y="308">write</text>
      <rect className="nA" x="268" y="205" width="104" height="60" />
      <text x="320" y="233" textAnchor="middle">planner</text>
      <text className="cap" x="320" y="249" textAnchor="middle">agent · claude</text>
      <rect className="nA2" x="503" y="85" width="104" height="60" />
      <text x="555" y="113" textAnchor="middle">reviewer</text>
      <text className="cap" x="555" y="129" textAnchor="middle">agent · claude</text>
      <rect className="nA2" x="503" y="325" width="104" height="60" />
      <text x="555" y="353" textAnchor="middle">builder</text>
      <text className="cap" x="555" y="369" textAnchor="middle">agent · codex</text>
      <rect className="nD" x="64" y="87" width="92" height="56" />
      <text x="110" y="113" textAnchor="middle">vault</text>
      <text className="cap" x="110" y="129" textAnchor="middle">secrets</text>
      <rect className="nD" x="64" y="327" width="92" height="56" />
      <text x="110" y="353" textAnchor="middle">table</text>
      <text className="cap" x="110" y="369" textAnchor="middle">memory</text>
      <rect className="nD" x="274" y="37" width="92" height="56" />
      <text x="320" y="63" textAnchor="middle">endpoint</text>
      <text className="cap" x="320" y="79" textAnchor="middle">https in</text>
      <rect className="nD" x="274" y="392" width="92" height="56" />
      <text x="320" y="418" textAnchor="middle">script</text>
      <text className="cap" x="320" y="434" textAnchor="middle">python</text>
      <rect className="nD" x="509" y="207" width="92" height="56" />
      <text x="555" y="233" textAnchor="middle">mcp</text>
      <text className="cap" x="555" y="249" textAnchor="middle">tools</text>
    </svg>
  );
}
