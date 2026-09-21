// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Which tier each engine control-plane path requires. **Default DENY.**
//!
//! The authenticated proxy is one axum route (`/v1/projects/{id}/engine/{*rest}`) standing in
//! front of an entire second API. So "which tier may do this" cannot be expressed in a handler
//! signature the way it is for every other route — it has to be a table, and the table has to be
//! the kind that fails closed.
//!
//! Same idiom as the wire matrix: *anything not listed is rejected*. Adding a route to the engine
//! without adding a row here makes it unreachable through the API, which is the correct direction
//! for a mistake to fall. The alternative — an allow-by-default table with a deny list — means a
//! new engine route is exposed to every guest on the day it is written, by someone who was not
//! thinking about this file.
//!
//! Matching is on `(method, path)` only. **Never on the body.** A rule that inspected a request
//! body would be authorising against one thing while the engine acts on another, which is the
//! confusion `extractor.rs` already refuses for `x-project-id`; it is also how a tier acquires
//! conditional powers, which the operator's ruling forbids. Where that made a rule impossible to
//! express — `PATCH /v1/nodes/{id}` carries both ctx content and agent config — the answer was a
//! narrower engine route (`PUT /v1/nodes/{id}/content`), not a smarter rule here.

use super::extractor::Tier;
use axum::http::Method;

/// One row. `methods` is an explicit list; there is no wildcard, because "any method" is how a
/// `DELETE` ends up wherever a `GET` was meant.
struct Rule {
    methods: &'static [&'static str],
    /// Segments, where `{}` matches exactly one segment and `**` matches one or more remaining.
    path: &'static str,
    tier: Tier,
}

const GET: &[&str] = &["GET", "HEAD"];
const POST: &[&str] = &["POST"];
const PUT: &[&str] = &["PUT"];
const DELETE: &[&str] = &["DELETE"];
const WRITE: &[&str] = &["POST", "PATCH", "PUT", "DELETE"];

/// The table. Ordered: the first matching rule wins, so a specific path may precede a general one.
///
/// Reasoning for the rows that are doing real work lives in `docs/proposals/shared-projects.md`
/// §5.2; the short version is beside each group.
const RULES: &[Rule] = &[
    // ---- guest: view only -------------------------------------------------------------------
    Rule {
        methods: GET,
        path: "v1/engine",
        tier: Tier::Guest,
    },
    Rule {
        methods: GET,
        path: "v1/board",
        tier: Tier::Guest,
    },
    Rule {
        methods: GET,
        path: "v1/events",
        tier: Tier::Guest,
    },
    Rule {
        methods: GET,
        path: "v1/agents/{}/log",
        tier: Tier::Guest,
    },
    Rule {
        methods: GET,
        path: "v1/agents/{}/inbox",
        tier: Tier::Guest,
    },
    Rule {
        methods: GET,
        path: "v1/agents/{}/inbox/{}",
        tier: Tier::Guest,
    },
    Rule {
        methods: GET,
        path: "v1/tables/{}/rows",
        tier: Tier::Guest,
    },
    Rule {
        methods: GET,
        path: "v1/tools/{}/ops",
        tier: Tier::Guest,
    },
    // Whether an agent is authenticated, not with what. Clearing or attaching a credential is
    // admin, immediately below, and is a different method on the same path.
    Rule {
        methods: GET,
        path: "v1/agents/{}/auth",
        tier: Tier::Guest,
    },
    // ---- admin: credentials --------------------------------------------------------------------
    // Listed before the prompter block so the narrower `auth` rules win over nothing, and so the
    // credential surfaces read as one group. A prompter must never attach, clear or use the
    // creator's LLM accounts: per ADVERSARY 037 a vault value is readable by every agent in the
    // project, so sharing a project must not share the creator's bill.
    Rule {
        methods: DELETE,
        path: "v1/agents/{}/auth",
        tier: Tier::Admin,
    },
    Rule {
        methods: POST,
        path: "v1/agents/{}/auth/begin",
        tier: Tier::Admin,
    },
    Rule {
        methods: POST,
        path: "v1/agents/{}/auth/complete",
        tier: Tier::Admin,
    },
    // The vault is not a prompter surface at all. `GET /v1/vault/{id}` returns key *names* only,
    // but a map of where the secrets are is still the vault.
    Rule {
        methods: GET,
        path: "v1/vault/{}",
        tier: Tier::Admin,
    },
    Rule {
        methods: WRITE,
        path: "v1/vault/{}/{}",
        tier: Tier::Admin,
    },
    // Invoking a tool spends vault-filled credentials. That is credential *use*, which is the
    // vault boundary wearing a different hat.
    Rule {
        methods: POST,
        path: "v1/tools/{}/call",
        tier: Tier::Admin,
    },
    // ---- admin: board structure -----------------------------------------------------------------
    Rule {
        methods: WRITE,
        path: "v1/nodes",
        tier: Tier::Admin,
    },
    Rule {
        methods: WRITE,
        path: "v1/wires",
        tier: Tier::Admin,
    },
    Rule {
        methods: POST,
        path: "v1/tools/import",
        tier: Tier::Admin,
    },
    Rule {
        methods: POST,
        path: "v1/tools/{}/import",
        tier: Tier::Admin,
    },
    // The builder's own LLM account: whoever saves it is handing the project a credential to
    // spend, so it sits with `agents/{}/auth` (admin), for reads too -- whether one is configured
    // and of what kind is not a guest's business. Missing entirely until now, which made saving a
    // setup-token fail for every tier, admin included.
    Rule {
        methods: GET,
        path: "v1/builder/credential",
        tier: Tier::Admin,
    },
    Rule {
        methods: &["PUT", "DELETE"],
        path: "v1/builder/credential",
        tier: Tier::Admin,
    },
    // ---- prompter: context and prompting ---------------------------------------------------------
    // The narrow content door. `PATCH /v1/nodes/{id}` stays admin below, because it also carries
    // agent config and a tier may not have conditional powers.
    Rule {
        methods: PUT,
        path: "v1/nodes/{}/content",
        tier: Tier::Prompter,
    },
    Rule {
        methods: POST,
        path: "v1/agents/{}/send",
        tier: Tier::Prompter,
    },
    Rule {
        methods: POST,
        path: "v1/agents/{}/start",
        tier: Tier::Prompter,
    },
    Rule {
        methods: POST,
        path: "v1/agents/{}/stop",
        tier: Tier::Prompter,
    },
    // The composition of start and stop. Allowing both halves and refusing the whole would be a
    // rule with no content.
    Rule {
        methods: POST,
        path: "v1/agents/{}/restart",
        tier: Tier::Prompter,
    },
    // Resetting an agent's session is context management, and a prompter can already stop it.
    Rule {
        methods: POST,
        path: "v1/agents/{}/clear",
        tier: Tier::Prompter,
    },
    // Interrupting a turn is the same lifecycle class as stop/restart above -- a prompter who may
    // already stop the agent outright may also ask its current turn to yield. Added in #75;
    // missing from this table until now meant the route was unreachable at ANY tier, admin
    // included (default-deny caught it, but that made it a dead route, not a working one).
    Rule {
        methods: POST,
        path: "v1/agents/{}/interrupt",
        tier: Tier::Prompter,
    },
    // Read-only SQL, but expressed as SQL, behind an authorizer whose function arm was
    // allow-by-default as recently as ADVERSARY 044. `GET .../rows` gives a guest the same data
    // through a door with no SQL in it, so the lowest-trust tier need not stand on that.
    Rule {
        methods: POST,
        path: "v1/tables/{}/query",
        tier: Tier::Prompter,
    },
    // `script_routes::run`'s own doc comment: "no wire to check, the same as
    // POST /v1/tables/:id/query needing none for the project's own owner" -- the same tier as
    // that route, for the same reason. Also missing a row entirely until now (unreachable at any
    // tier, same class of gap as interrupt above).
    Rule {
        methods: POST,
        path: "v1/scripts/{}/run",
        tier: Tier::Prompter,
    },
    // ---- admin: the general node patch ------------------------------------------------------
    // Last of the `v1/nodes` rules so `v1/nodes/{}/content` above wins for a prompter.
    Rule {
        methods: WRITE,
        path: "v1/nodes/{}",
        tier: Tier::Admin,
    },
];

/// The minimum tier for an engine path, or `None` when no tier may reach it.
///
/// `None` covers two distinct things on purpose, because the answer is the same: a path with no
/// rule, and `v1/cli/**`, which is denied to every tier *including admin*. The CLI realm is the
/// node-token plane, deliberately nested outside the engine-secret layer. ADVERSARY 002 and the M0
/// API plan review both flagged that the authenticated proxy forwards it with the host bearer.
/// Nothing in the product calls it through the API, so refusing costs nothing — and it is the
/// multiplayer design's rule made mechanical: the actor is ignored entirely on the agent-token
/// plane, and the cleanest way to ignore it is to refuse to carry it there.
/// `segments` must be the **same decoded segments the upstream URL is built from**
/// (`wheel_core::proxy_path::proxy_segments`), never the raw suffix. Matching one spelling of a
/// path and forwarding another is how an authorisation check comes to be about a different request
/// than the one that happens — the same confusion `extractor.rs` refuses for `x-project-id`.
pub fn engine_tier(method: &Method, segments: &[&str]) -> Option<Tier> {
    let segments: Vec<&str> = segments.iter().copied().filter(|s| !s.is_empty()).collect();
    let segments = segments.as_slice();
    if segments.is_empty() {
        return None;
    }
    if segments[0] == "v1" && segments.get(1) == Some(&"cli") {
        return None;
    }
    let m = method.as_str();
    RULES
        .iter()
        .find(|r| r.methods.contains(&m) && matches_pattern(r.path, segments))
        .map(|r| r.tier)
}

/// `{}` matches one segment, `**` matches one or more trailing segments, anything else is literal.
fn matches_pattern(pattern: &str, segments: &[&str]) -> bool {
    let pats: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    for (i, p) in pats.iter().enumerate() {
        if *p == "**" {
            return segments.len() > i;
        }
        match segments.get(i) {
            None => return false,
            Some(s) => {
                if *p != "{}" && p != s {
                    return false;
                }
            }
        }
    }
    pats.len() == segments.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives the real entry point through the same decoder the proxy uses, so a test can be
    /// written as a path while still exercising the segment contract.
    fn tier(m: &str, p: &str) -> Option<Tier> {
        let segments = wheel_core::proxy_path::proxy_segments(p).expect("test path is forwardable");
        engine_tier(&Method::from_bytes(m.as_bytes()).unwrap(), &segments)
    }

    const AGENT: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";

    /// Engine `/v1` routes that are deliberately NOT reachable through the generic proxy, each
    /// with the API route that serves it instead. The test below holds every one of them to
    /// staying denied here, so this list cannot quietly become a way to skip a tier decision.
    const NOT_PROXIED: &[(&str, &str)] = &[
        // `routes::builder::turns`: a dedicated route (the shared client's timeout would cut a
        // turn short), gated by `AdminScope` in its own signature.
        ("POST", "v1/builder/turns"),
    ];

    /// One engine route, as registered: the method set and the path with every `{name}` reduced
    /// to `{}`, matching the spelling this file's rules use.
    struct EngineRoute {
        path: String,
        methods: Vec<&'static str>,
    }

    /// True when `text` calls `name(` as its own identifier -- `get(` but not `budget(`.
    fn calls(text: &str, name: &str) -> bool {
        text.match_indices(&format!("{name}(")).any(|(i, _)| {
            !text[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    }

    /// `text` with `//` and `/* */` comments removed, leaving string literals alone. A line-based
    /// filter misses `/* .route(..) */` on one line and a `.route(` after code on the same line.
    fn without_comments(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    out.push(c);
                    while let Some(d) = chars.next() {
                        out.push(d);
                        if d == '\\' {
                            out.extend(chars.next());
                        } else if d == '"' {
                            break;
                        }
                    }
                }
                '/' if chars.peek() == Some(&'/') => {
                    for d in chars.by_ref() {
                        if d == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                }
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    let mut prev = ' ';
                    for d in chars.by_ref() {
                        if prev == '*' && d == '/' {
                            break;
                        }
                        prev = d;
                    }
                    out.push(' ');
                }
                _ => out.push(c),
            }
        }
        out
    }

    /// Refuse any way of registering a route this scan cannot read. By IDENTIFIER, not a fixed list
    /// of spellings: anything called `*_service`, anything called `fallback*`, and the method-router
    /// constructors that take a filter or a service. A list of known-bad names is what missed
    /// `fallback_service`, `put_service` and a top-level `route_service`. `nest` is only allowed
    /// where the caller says so (the outermost router nests `/v1`, `/v1/cli` and `/ingress`).
    fn refuse_unreadable_shapes(text: &str, allow_nest: bool, what: &str) {
        for (i, _) in text.match_indices('(') {
            let ident: String = text[..i]
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let banned = ident.ends_with("_service")
                || ident.starts_with("fallback")
                || matches!(
                    ident.as_str(),
                    "merge"
                        | "on"
                        | "any"
                        | "head"
                        | "options"
                        | "trace"
                        | "connect"
                        | "nest_service"
                        | "route_service"
                )
                || (ident == "nest" && !allow_nest);
            assert!(
                !banned,
                "the {what} router uses `{ident}(`, which this scan does not understand -- teach it, \
                 or register the route in a shape it reads"
            );
        }
    }

    /// The engine's `router()` function body, comments removed.
    fn engine_router_source() -> String {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../wheel-engine/src/api/mod.rs"
        ))
        .expect("the engine's route table is readable from this workspace");
        let start = src.find("pub fn router(").expect("the engine router");
        let end = start + src[start..].find("\n}\n").expect("the end of router()");
        let body = without_comments(&src[start..end]);
        refuse_unreadable_shapes(&body, true, "engine");
        // The build may differ from the text: a gated registration is scanned here whether or not it
        // is compiled in, and an attribute on a route call is not something this scan reads.
        assert!(
            !body.contains("#[cfg") && !body.contains("cfg!("),
            "router() uses cfg: this scan reads text, not what the build compiles -- register the \
             route unconditionally, or teach the scan"
        );
        body
    }

    /// The text of one `let <name> = Router::new()` builder chain, up to its `.route_layer(`.
    fn chain<'a>(router: &'a str, name: &str) -> &'a str {
        let start = router
            .find(&format!("let {name} = Router::new()"))
            .unwrap_or_else(|| panic!("the {name} router"));
        let end = start
            + router[start..]
                .find(".route_layer(")
                .unwrap_or_else(|| panic!("the end of the {name} router"));
        &router[start..end]
    }

    /// Every `.route(path, handlers)` in one builder chain, under `prefix`.
    ///
    /// Reads source because an axum `Router` cannot be enumerated, and the alternative -- a second,
    /// hand-kept list of engine routes -- is exactly the drift this test exists to catch. Because
    /// it reads source, it must REFUSE what it cannot read rather than skip it: a registration
    /// shape it does not understand fails the test loudly instead of contributing no rows.
    fn scan(window: &str, prefix: &str) -> Vec<EngineRoute> {
        refuse_unreadable_shapes(window, false, prefix);
        window
            .split(".route(")
            .skip(1)
            .map(|chunk| {
                assert!(
                    chunk.trim_start().starts_with('"'),
                    "a route path that is not a plain string literal: {}",
                    &chunk[..chunk.len().min(60)]
                );
                let open = chunk.find('"').unwrap() + 1;
                let close = open + chunk[open..].find('"').expect("an unterminated path literal");
                let raw = &chunk[open..close];
                let handlers = &chunk[close + 1..];
                assert!(
                    !handlers.contains('|') && !handlers.contains('{'),
                    "a closure or block inside the handlers of {raw:?}: the scan would misread it"
                );
                assert!(
                    !handlers.contains('"'),
                    "a string literal inside the handlers of {raw:?}: the scan would misread it"
                );
                let path = std::iter::once(prefix.to_string())
                    .chain(raw.split('/').filter(|s| !s.is_empty()).map(|s| {
                        if s.starts_with('{') {
                            "{}".to_string()
                        } else {
                            s.to_string()
                        }
                    }))
                    .collect::<Vec<_>>()
                    .join("/");
                let methods: Vec<&'static str> = [
                    ("get", "GET"),
                    ("post", "POST"),
                    ("put", "PUT"),
                    ("patch", "PATCH"),
                    ("delete", "DELETE"),
                ]
                .into_iter()
                .filter(|(name, _)| calls(handlers, name))
                .map(|(_, m)| m)
                .collect();
                assert!(
                    !methods.is_empty(),
                    "{raw:?} was registered with no method this scan recognises -- it would need no \
                     policy row, which is the failure this test exists to prevent"
                );
                EngineRoute { path, methods }
            })
            .collect()
    }

    fn engine_v1_routes() -> Vec<EngineRoute> {
        scan(chain(&engine_router_source(), "v1"), "v1")
    }

    /// The scan reads only the `/v1` builder. Everything else in `router()` has to be accounted for
    /// independently, so a route added anywhere it does not read cannot slip past by being unseen.
    #[test]
    fn nothing_in_the_engine_router_is_registered_where_the_scan_cannot_see_it() {
        let router = engine_router_source();
        let v1 = scan(chain(&router, "v1"), "v1").len();
        let cli = scan(chain(&router, "cli"), "v1/cli").len();
        let all = router.matches(".route(").count();
        // The one top-level route is `/healthz`.
        assert_eq!(
            all,
            v1 + cli + 1,
            "router() registers {all} routes; the scan accounts for {v1} in /v1, {cli} in /v1/cli \
             and /healthz -- one is somewhere it does not read"
        );
        assert!(
            !calls(&router, "merge"),
            "router() merges a router the scan cannot read"
        );
        let nests: Vec<&str> = router
            .match_indices(".nest(")
            .map(|(i, _)| {
                let rest = &router[i + ".nest(".len()..];
                let rest = rest
                    .trim_start()
                    .strip_prefix('"')
                    .expect("a literal nest path");
                &rest[..rest.find('"').unwrap()]
            })
            .collect();
        assert_eq!(
            nests,
            ["/v1", "/v1/cli", "/ingress"],
            "router() nests a tree the scan does not read (ingress is public and never proxied)"
        );
    }

    fn concrete(path: &str) -> String {
        path.split('/')
            .map(|s| if s == "{}" { AGENT } else { s })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// The gate this table exists for, and the second time its absence bit: a route shipped in
    /// the engine with no row here is refused for every tier, admin included, and the first
    /// anyone learns of it is a user staring at "not permitted" (`interrupt`, `scripts/run`, then
    /// `builder/credential`). Default-deny made each failure safe; it did not make it visible.
    #[test]
    fn every_engine_route_has_a_tier_decision() {
        let routes = engine_v1_routes();
        assert!(
            routes.len() >= 30 && routes.iter().any(|r| r.path == "v1/builder/credential"),
            "the route scan looks wrong ({} routes) -- did the engine's router move?",
            routes.len()
        );

        let mut missing = Vec::new();
        for route in &routes {
            for method in &route.methods {
                let decided = tier(method, &concrete(&route.path));
                let exempt = NOT_PROXIED.contains(&(*method, route.path.as_str()));
                match (decided, exempt) {
                    (None, false) => missing.push(format!("{method} /{}", route.path)),
                    (Some(_), true) => panic!(
                        "{method} /{} is listed as not proxied but has a proxy row",
                        route.path
                    ),
                    _ => {}
                }
            }
        }
        assert!(
            missing.is_empty(),
            "engine routes with no tier row in auth/policy.rs (unreachable for EVERY tier): {missing:#?}"
        );
    }

    /// The other direction: a row for a route the engine no longer has is a tier decision about
    /// nothing, and it quietly stops meaning what its comment says.
    #[test]
    fn every_policy_row_names_a_real_engine_route() {
        let routes = engine_v1_routes();
        let stale: Vec<_> = RULES
            .iter()
            .filter(|rule| {
                !routes.iter().any(|route| {
                    route.path == rule.path
                        && route.methods.iter().any(|m| rule.methods.contains(m))
                })
            })
            .map(|rule| format!("{:?} /{}", rule.methods, rule.path))
            .collect();
        assert!(
            stale.is_empty(),
            "policy rows with no engine route: {stale:#?}"
        );
    }

    #[test]
    fn the_builders_credential_is_admin_only_for_every_method() {
        let path = "v1/builder/credential";
        for method in ["GET", "PUT", "DELETE"] {
            assert_eq!(tier(method, path), Some(Tier::Admin), "{method}");
        }
        assert_eq!(tier("POST", path), None);
    }

    #[test]
    fn a_guest_may_read_the_board_and_the_transcripts() {
        assert_eq!(tier("GET", "v1/board"), Some(Tier::Guest));
        assert_eq!(tier("GET", "v1/engine"), Some(Tier::Guest));
        assert_eq!(tier("GET", "v1/events"), Some(Tier::Guest));
        assert_eq!(
            tier("GET", &format!("v1/agents/{AGENT}/log")),
            Some(Tier::Guest)
        );
        assert_eq!(
            tier("GET", &format!("v1/agents/{AGENT}/inbox")),
            Some(Tier::Guest)
        );
        assert_eq!(
            tier("GET", &format!("v1/agents/{AGENT}/inbox/{AGENT}")),
            Some(Tier::Guest)
        );
        assert_eq!(
            tier("GET", &format!("v1/tables/{AGENT}/rows")),
            Some(Tier::Guest)
        );
        assert_eq!(
            tier("GET", &format!("v1/tools/{AGENT}/ops")),
            Some(Tier::Guest)
        );
    }

    #[test]
    fn prompting_and_context_are_prompter() {
        assert_eq!(
            tier("POST", &format!("v1/agents/{AGENT}/send")),
            Some(Tier::Prompter)
        );
        assert_eq!(
            tier("POST", &format!("v1/agents/{AGENT}/start")),
            Some(Tier::Prompter)
        );
        assert_eq!(
            tier("POST", &format!("v1/agents/{AGENT}/stop")),
            Some(Tier::Prompter)
        );
        assert_eq!(
            tier("POST", &format!("v1/agents/{AGENT}/restart")),
            Some(Tier::Prompter)
        );
        assert_eq!(
            tier("POST", &format!("v1/agents/{AGENT}/clear")),
            Some(Tier::Prompter)
        );
        assert_eq!(
            tier("POST", &format!("v1/agents/{AGENT}/interrupt")),
            Some(Tier::Prompter)
        );
        assert_eq!(
            tier("PUT", &format!("v1/nodes/{AGENT}/content")),
            Some(Tier::Prompter)
        );
        assert_eq!(
            tier("POST", &format!("v1/tables/{AGENT}/query")),
            Some(Tier::Prompter)
        );
        assert_eq!(
            tier("POST", &format!("v1/scripts/{AGENT}/run")),
            Some(Tier::Prompter)
        );
    }

    /// The narrow content door must win over the general node patch, or a prompter cannot write
    /// context at all — and the general patch must stay admin, or a prompter can rewire the board.
    #[test]
    fn the_content_door_is_prompter_and_the_general_node_patch_is_admin() {
        assert_eq!(
            tier("PUT", &format!("v1/nodes/{AGENT}/content")),
            Some(Tier::Prompter)
        );
        assert_eq!(
            tier("PATCH", &format!("v1/nodes/{AGENT}")),
            Some(Tier::Admin)
        );
        assert_eq!(
            tier("DELETE", &format!("v1/nodes/{AGENT}")),
            Some(Tier::Admin)
        );
        assert_eq!(tier("POST", "v1/nodes"), Some(Tier::Admin));
    }

    #[test]
    fn structure_and_credentials_are_admin() {
        assert_eq!(tier("POST", "v1/wires"), Some(Tier::Admin));
        assert_eq!(tier("DELETE", "v1/wires"), Some(Tier::Admin));
        assert_eq!(tier("GET", &format!("v1/vault/{AGENT}")), Some(Tier::Admin));
        assert_eq!(
            tier("PUT", &format!("v1/vault/{AGENT}/KEY")),
            Some(Tier::Admin)
        );
        assert_eq!(
            tier("DELETE", &format!("v1/vault/{AGENT}/KEY")),
            Some(Tier::Admin)
        );
        assert_eq!(
            tier("POST", &format!("v1/agents/{AGENT}/auth/begin")),
            Some(Tier::Admin)
        );
        assert_eq!(
            tier("POST", &format!("v1/agents/{AGENT}/auth/complete")),
            Some(Tier::Admin)
        );
        assert_eq!(
            tier("DELETE", &format!("v1/agents/{AGENT}/auth")),
            Some(Tier::Admin)
        );
        assert_eq!(
            tier("POST", &format!("v1/tools/{AGENT}/call")),
            Some(Tier::Admin)
        );
        assert_eq!(tier("POST", "v1/tools/import"), Some(Tier::Admin));
        assert_eq!(
            tier("POST", &format!("v1/tools/{AGENT}/import")),
            Some(Tier::Admin)
        );
    }

    /// Reading whether an agent is authenticated is a guest read; clearing that credential is not.
    /// Same path, different method, different tier — so the table must discriminate on both.
    #[test]
    fn method_decides_where_a_path_is_shared() {
        assert_eq!(
            tier("GET", &format!("v1/agents/{AGENT}/auth")),
            Some(Tier::Guest)
        );
        assert_eq!(
            tier("DELETE", &format!("v1/agents/{AGENT}/auth")),
            Some(Tier::Admin)
        );
    }

    #[test]
    fn the_cli_realm_is_denied_to_everyone_including_admin() {
        for p in [
            "v1/cli/whoami",
            "v1/cli/msg",
            "v1/cli/secret",
            "v1/cli/query",
            "v1/cli",
        ] {
            assert_eq!(tier("GET", p), None, "{p} must be denied");
            assert_eq!(tier("POST", p), None, "{p} must be denied");
        }
    }

    #[test]
    fn anything_not_listed_is_denied() {
        for p in [
            "",
            "v1",
            "v1/board/extra",
            "v1/unknown",
            "v2/board",
            "healthz",
            "ingress/hook",
            "v1/nodes/a/b/c",
        ] {
            assert_eq!(tier("GET", p), None, "{p} must be denied");
        }
        // A method nobody granted on a path somebody did.
        assert_eq!(tier("DELETE", "v1/board"), None);
        assert_eq!(tier("POST", "v1/board"), None);
        assert_eq!(tier("PATCH", "v1/events"), None);
    }

    /// Paths a proxy would refuse outright never reach the table — the decoder rejects them
    /// first, which is why the table may assume its segments are literal.
    #[test]
    fn an_unforwardable_path_is_refused_before_the_table_sees_it() {
        for p in [
            "/",
            "v1//board",
            "v1/../board",
            "v1/%2e%2e/board",
            "v1/bo\\ard",
        ] {
            assert!(
                wheel_core::proxy_path::proxy_segments(p).is_err(),
                "{p} must be refused by the decoder"
            );
        }
    }

    /// A path that merely *starts* with an allowed one is a different path.
    #[test]
    fn a_prefix_is_not_a_match() {
        assert_eq!(tier("GET", "v1/boardroom"), None);
        assert_eq!(
            tier("GET", "v1/board/"),
            Some(Tier::Guest),
            "a trailing slash is the same path"
        );
        assert_eq!(tier("PUT", &format!("v1/nodes/{AGENT}/contents")), None);
    }

    #[test]
    fn head_is_treated_as_a_read_wherever_get_is() {
        assert_eq!(tier("HEAD", "v1/board"), Some(Tier::Guest));
        assert_eq!(
            tier("HEAD", &format!("v1/vault/{AGENT}")),
            Some(Tier::Admin)
        );
    }

    #[test]
    fn segment_matching_does_not_let_a_wildcard_swallow_a_path() {
        assert!(matches_pattern(
            "v1/agents/{}/send",
            &["v1", "agents", "x", "send"]
        ));
        assert!(!matches_pattern(
            "v1/agents/{}/send",
            &["v1", "agents", "x", "y", "send"]
        ));
        assert!(!matches_pattern(
            "v1/agents/{}/send",
            &["v1", "agents", "send"]
        ));
        assert!(matches_pattern("v1/**", &["v1", "anything", "deep"]));
        assert!(
            !matches_pattern("v1/**", &["v1"]),
            "** needs at least one segment"
        );
    }
}
