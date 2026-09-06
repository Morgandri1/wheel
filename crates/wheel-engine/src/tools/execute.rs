//! Calling an imported operation (§3d).
//!
//! Split deliberately in two. [`build_request`] decides WHAT to send and is
//! pure — it is where an agent's arguments meet the operator's fills, which is
//! the part that must never get it wrong, and it is unit-testable without a
//! network. [`send`] does the I/O and the SSRF checks.
//!
//! Two rules run through all of it (§3d rules 1 and 2). An agent sees only
//! `agent`-mode fields, and `static`/`vault` values are authoritative: an
//! agent cannot override, read back, or provoke the engine into echoing one.

use std::{collections::HashMap, net::IpAddr, time::Duration};

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use wheel_core::{FillMode, ParamLocation, ToolConfig, ToolOperation};

/// Ceiling on a response body. A tool is a thing an agent reads, and an agent
/// that reads five megabytes has already lost the plot.
pub const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

/// One outbound call may not take longer than this.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How many redirects to follow. Each hop is re-validated; the limit exists so
/// a redirect loop cannot spend the whole timeout.
pub const MAX_REDIRECTS: usize = 3;

/// A request that has been fully decided but not yet sent.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub cookies: Vec<(String, String)>,
    pub body: Option<Value>,
    /// Values that came from a vault or a static fill. Never rendered into a
    /// curl string, never returned, never logged — the list exists so they can
    /// be masked rather than hoped about.
    pub secrets: Vec<String>,
}

/// What a call produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub status: u16,
    pub headers: Map<String, Value>,
    pub body: Value,
    pub duration_ms: u64,
    pub bytes: usize,
}

/// Decide the request for `op` from the agent's arguments and the node's fills.
///
/// `vault_values` maps a `<vault>/<key>` ref to its value; the caller resolves
/// those, so this stays pure and the wire check lives with the wires.
pub fn build_request(
    cfg: &ToolConfig,
    op: &ToolOperation,
    args: &Value,
    vault_values: &HashMap<String, String>,
) -> Result<Prepared> {
    if !op.enabled {
        bail!("operation {} is disabled", op.id);
    }
    let supplied = match args {
        Value::Null => Map::new(),
        Value::Object(m) => m.clone(),
        _ => bail!("arguments must be a JSON object"),
    };

    // An argument for a field the agent does not own is refused, not ignored
    // (§3d rule 1). Ignoring it would let an agent believe it had set an
    // authorization header that the operator actually controls.
    for name in supplied.keys() {
        let known = op.params.iter().find(|p| &p.name == name);
        match known {
            Some(p) if p.fill.is_agent_visible() => {}
            Some(p) => bail!(
                "{name:?} is set by the board ({}), not by the caller",
                match p.fill.mode {
                    FillMode::Static => "a fixed value",
                    FillMode::Vault => "a vault",
                    FillMode::Hidden => "omitted",
                    FillMode::Agent => unreachable!(),
                }
            ),
            None => bail!("{name:?} is not a field of operation {}", op.id),
        }
    }

    let mut path = op.path.clone();
    let mut query: Vec<(String, String)> = Vec::new();
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut cookies: Vec<(String, String)> = Vec::new();
    let mut body = Map::new();
    let mut secrets = Vec::new();

    for p in &op.params {
        let value: Option<String> = match p.fill.mode {
            FillMode::Hidden => None,
            FillMode::Static => {
                let v = p.fill.value.clone().unwrap_or_default();
                if !v.is_empty() {
                    secrets.push(v.clone());
                }
                Some(v)
            }
            FillMode::Vault => {
                let r = p.fill.vault_ref.as_deref().with_context(|| {
                    format!("field {:?} is vault-filled but names no ref", p.name)
                })?;
                let v = vault_values.get(r).with_context(|| {
                    format!("field {:?} needs {r}, which this tool cannot read", p.name)
                })?;
                secrets.push(v.clone());
                Some(v.clone())
            }
            FillMode::Agent => match supplied.get(&p.name) {
                Some(Value::Null) | None if p.required => {
                    bail!("field {:?} is required", p.name)
                }
                Some(Value::Null) | None => None,
                Some(v) => Some(scalar(v)),
            },
        };

        let Some(value) = value else { continue };
        match p.location {
            // Percent-encoded: a path value containing `/` or `?` would
            // otherwise address a different resource entirely.
            ParamLocation::Path => {
                path = path.replace(&format!("{{{}}}", p.name), &encode(&value));
            }
            ParamLocation::Query => query.push((p.name.clone(), value)),
            ParamLocation::Header => headers.push((p.name.clone(), value)),
            // Encoded like path and query (ADVERSARY 022/2). A cookie value
            // is data, and `x; admin=true; role=root` is two more cookies the
            // caller never granted. `;` is legal in a header value, so
            // reqwest does not reject it the way it rejects CRLF — this is
            // the only thing standing between an agent and a forged session.
            ParamLocation::Cookie => cookies.push((p.name.clone(), encode(&value))),
            ParamLocation::Body => {
                body.insert(
                    p.name.clone(),
                    original_or_string(&supplied, &p.name, value),
                );
            }
        }
    }

    // A placeholder nobody filled would be sent literally, and `/users/{id}`
    // is a real path that returns something confusing rather than an error.
    if let Some(unfilled) = first_placeholder(&path) {
        bail!("path parameter {unfilled:?} was not supplied");
    }

    let mut url = format!("{}{}", cfg.base_url.trim_end_matches('/'), path);
    if !query.is_empty() {
        let q: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect();
        url.push('?');
        url.push_str(&q.join("&"));
    }

    Ok(Prepared {
        method: op.method.as_str().to_string(),
        url,
        headers,
        cookies,
        body: (!body.is_empty()).then_some(Value::Object(body)),
        secrets,
    })
}

/// Body fields keep the agent's own JSON type; everything else is a string,
/// because a header or a query parameter is text on the wire regardless.
fn original_or_string(supplied: &Map<String, Value>, name: &str, fallback: String) -> Value {
    match supplied.get(name) {
        Some(v) if !v.is_null() => v.clone(),
        _ => Value::String(fallback),
    }
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn first_placeholder(path: &str) -> Option<String> {
    let start = path.find('{')?;
    let end = path[start..].find('}')?;
    Some(path[start + 1..start + end].to_string())
}

/// Percent-encode everything that is not unreserved. Deliberately strict: this
/// runs on values an agent chose.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The equivalent `curl`, with every static and vault value masked.
///
/// The point of this is that an operator can reproduce a call by hand — and
/// the point of the masking is that they can paste it somewhere without
/// handing over a credential the agent was never allowed to see.
pub fn curl_for(p: &Prepared) -> String {
    // Both spellings (ADVERSARY 022/1). A secret in a query or path fill is
    // stored PERCENT-ENCODED in the url, so searching for the raw string
    // missed it entirely whenever the value contained anything outside the
    // unreserved set -- which every base64 credential does. The header
    // placement was masked and the query placement was not, which is the
    // worst kind of gap: it looks like it works.
    let mask = |s: &str| -> String {
        let mut out = s.to_string();
        for secret in &p.secrets {
            if secret.is_empty() {
                continue;
            }
            out = out.replace(secret.as_str(), "<redacted>");
            let encoded = encode(secret);
            if encoded != *secret {
                out = out.replace(&encoded, "<redacted>");
            }
        }
        out
    };

    let mut parts = vec!["curl".to_string(), "-X".into(), p.method.clone()];
    for (k, v) in &p.headers {
        parts.push("-H".into());
        parts.push(shell_quote(&format!("{k}: {}", mask(v))));
    }
    if !p.cookies.is_empty() {
        let jar: Vec<String> = p
            .cookies
            .iter()
            .map(|(k, v)| format!("{k}={}", mask(v)))
            .collect();
        parts.push("-b".into());
        parts.push(shell_quote(&jar.join("; ")));
    }
    if let Some(body) = &p.body {
        parts.push("-H".into());
        parts.push(shell_quote("content-type: application/json"));
        parts.push("-d".into());
        parts.push(shell_quote(&mask(&body.to_string())));
    }
    parts.push(shell_quote(&mask(&p.url)));
    parts.join(" ")
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// --- the network half -------------------------------------------------------

/// Send a prepared request, following redirects by hand so every hop is
/// checked.
///
/// The SSRF policy (§3d rule 4) is layered, because each layer alone has a
/// hole. The host is resolved ONCE and the connection is pinned to the address
/// that was validated, so a name that answers differently a moment later
/// cannot become a different destination (DNS rebinding). Redirects are
/// followed manually because a redirect is a fresh destination the caller
/// never named, and the most useful thing an attacker can do with an allowed
/// host is have it point them somewhere else.
pub async fn send(p: &Prepared) -> Result<Outcome> {
    let started = std::time::Instant::now();
    let mut url = p.url.clone();
    let mut hops = 0usize;

    loop {
        let (host, addr) = resolve_and_check(&url).await?;

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(CALL_TIMEOUT)
            .resolve(&host, addr)
            .build()
            .context("building the http client")?;

        let mut req = client.request(p.method.parse()?, &url);
        for (k, v) in &p.headers {
            req = req.header(k, v);
        }
        if !p.cookies.is_empty() {
            let jar: Vec<String> = p.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect();
            req = req.header("cookie", jar.join("; "));
        }
        // Only on the first hop: a redirect to another origin must not carry
        // the body — or the credentials in it — anywhere the caller did not
        // name.
        if hops == 0 {
            if let Some(body) = &p.body {
                req = req.json(body);
            }
        }

        let resp = req.send().await.context("calling the tool")?;
        let status = resp.status().as_u16();

        if (300..400).contains(&status) {
            let Some(next) = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
            else {
                // A redirect with nowhere to go is the answer.
                return finish(resp, status, started).await;
            };
            hops += 1;
            if hops > MAX_REDIRECTS {
                bail!("too many redirects (limit {MAX_REDIRECTS})");
            }
            url = join_redirect(&url, next)?;
            continue;
        }

        return finish(resp, status, started).await;
    }
}

async fn finish(
    resp: reqwest::Response,
    status: u16,
    started: std::time::Instant,
) -> Result<Outcome> {
    let mut headers = Map::new();
    for (k, v) in resp.headers() {
        if let Ok(s) = v.to_str() {
            headers.insert(k.as_str().to_string(), Value::String(s.to_string()));
        }
    }

    // Read with a ceiling rather than trusting content-length, which is a
    // claim the other end makes about itself.
    let mut resp = resp;
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.context("reading the response")? {
        buf.extend_from_slice(&chunk);
        if buf.len() > MAX_RESPONSE_BYTES {
            bail!(
                "response exceeded {} MiB",
                MAX_RESPONSE_BYTES / (1024 * 1024)
            );
        }
    }

    let text = String::from_utf8_lossy(&buf).into_owned();
    let body = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
    Ok(Outcome {
        status,
        headers,
        bytes: buf.len(),
        body,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// Resolve a URL's host and refuse anything that is not a public address.
///
/// Returns the socket address the connection must be pinned to, so the name is
/// looked up exactly once and the address that was checked is the address that
/// is used.
async fn resolve_and_check(url: &str) -> Result<(String, std::net::SocketAddr)> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("bad url {url:?}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => bail!("{other} is not an http(s) url"),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("{url:?} has no host"))?
        .to_string();
    // Cheap literal and suffix checks first: they need no DNS and they catch
    // the obvious attempts before we ask the resolver anything.
    if wheel_core::host_is_denied(&host) {
        bail!("{host} is not a reachable destination: private, loopback or internal");
    }
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("no port for {url:?}"))?;

    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.clone(), port))
        .await
        .with_context(|| format!("resolving {host}"))?
        .collect();
    if addrs.is_empty() {
        bail!("{host} does not resolve");
    }
    // EVERY answer must be public, not just the one we pick: a name that
    // resolves to both a public and a private address is a rebinding attempt
    // wearing a disguise, and picking the public one would be luck.
    for a in &addrs {
        if wheel_core::ip_is_denied(a.ip()) {
            bail!("{host} resolves to {}, which is not reachable", a.ip());
        }
    }
    Ok((host, addrs[0]))
}

/// Resolve a `Location` against the URL it came from, so a relative redirect
/// works and an absolute one is taken at face value (and re-checked).
fn join_redirect(from: &str, location: &str) -> Result<String> {
    let base = reqwest::Url::parse(from)?;
    let next = base
        .join(location)
        .with_context(|| format!("bad redirect target {location:?}"))?;
    Ok(next.to_string())
}

/// Public for tests and for the SSRF check on `mcp.url`.
pub fn ip_allowed(ip: IpAddr) -> bool {
    !wheel_core::ip_is_denied(ip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wheel_core::{Fill, ToolFormat, ToolKind, ToolMethod, ToolParam, ToolSource};

    fn cfg(ops: Vec<ToolOperation>) -> ToolConfig {
        ToolConfig {
            kind: ToolKind::Http,
            source: ToolSource {
                format: ToolFormat::Manual,
                raw: String::new(),
                imported_at: wheel_core::Timestamp::now(),
            },
            base_url: "https://api.example.com".into(),
            operations: ops,
        }
    }

    fn p(name: &str, location: ParamLocation, fill: Fill) -> ToolParam {
        ToolParam {
            name: name.into(),
            location,
            required: false,
            description: None,
            schema: None,
            fill,
        }
    }

    fn vault(r: &str) -> Fill {
        Fill {
            mode: FillMode::Vault,
            value: None,
            vault_ref: Some(r.into()),
        }
    }

    fn fixed(v: &str) -> Fill {
        Fill {
            mode: FillMode::Static,
            value: Some(v.into()),
            vault_ref: None,
        }
    }

    fn op(params: Vec<ToolParam>) -> ToolOperation {
        ToolOperation {
            id: "send".into(),
            method: ToolMethod::Post,
            path: "/messages/{room}".into(),
            summary: None,
            enabled: true,
            params,
        }
    }

    fn secrets() -> HashMap<String, String> {
        HashMap::from([("creds/API_KEY".to_string(), "sk-SUPER-SECRET".to_string())])
    }

    /// The ordinary case, so the refusals below are not just "everything is
    /// refused".
    #[test]
    fn a_call_assembles_path_query_header_and_body() {
        let o = op(vec![
            p("room", ParamLocation::Path, Fill::agent()),
            p("verbose", ParamLocation::Query, Fill::agent()),
            p("X-Trace", ParamLocation::Header, Fill::agent()),
            p("text", ParamLocation::Body, Fill::agent()),
        ]);
        let got = build_request(
            &cfg(vec![o.clone()]),
            &o,
            &serde_json::json!({"room": "general", "verbose": "1", "X-Trace": "abc", "text": "hi"}),
            &secrets(),
        )
        .unwrap();

        assert_eq!(got.method, "POST");
        assert_eq!(
            got.url,
            "https://api.example.com/messages/general?verbose=1"
        );
        assert_eq!(got.headers, vec![("X-Trace".into(), "abc".into())]);
        assert_eq!(got.body.unwrap()["text"], "hi");
    }

    /// §3d rule 2: static and vault values are authoritative. An agent naming
    /// one must be refused, not quietly ignored — ignoring it would let an
    /// agent believe it had set an authorization header the operator owns.
    #[test]
    fn an_agent_cannot_set_a_field_the_board_owns() {
        let o = op(vec![
            p("room", ParamLocation::Path, Fill::agent()),
            p(
                "Authorization",
                ParamLocation::Header,
                vault("creds/API_KEY"),
            ),
            p("X-Env", ParamLocation::Header, fixed("prod")),
            p(
                "X-Gone",
                ParamLocation::Header,
                Fill {
                    mode: FillMode::Hidden,
                    value: None,
                    vault_ref: None,
                },
            ),
        ]);
        for attempt in [
            serde_json::json!({"room": "g", "Authorization": "Bearer mine"}),
            serde_json::json!({"room": "g", "X-Env": "dev"}),
            serde_json::json!({"room": "g", "X-Gone": "back"}),
        ] {
            let err = build_request(&cfg(vec![o.clone()]), &o, &attempt, &secrets())
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("set by the board"),
                "must be refused, got: {err}"
            );
        }
    }

    #[test]
    fn an_invented_field_is_refused_rather_than_ignored() {
        let o = op(vec![p("room", ParamLocation::Path, Fill::agent())]);
        let err = build_request(
            &cfg(vec![o.clone()]),
            &o,
            &serde_json::json!({"room": "g", "admin": true}),
            &secrets(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not a field"), "{err}");
    }

    /// The vault value must reach the wire and must not come from the agent.
    #[test]
    fn a_vault_field_is_resolved_and_tracked_as_a_secret() {
        let o = op(vec![
            p("room", ParamLocation::Path, Fill::agent()),
            p(
                "Authorization",
                ParamLocation::Header,
                vault("creds/API_KEY"),
            ),
        ]);
        let got = build_request(
            &cfg(vec![o.clone()]),
            &o,
            &serde_json::json!({"room": "g"}),
            &secrets(),
        )
        .unwrap();
        assert_eq!(
            got.headers,
            vec![("Authorization".into(), "sk-SUPER-SECRET".into())]
        );
        assert!(got.secrets.contains(&"sk-SUPER-SECRET".to_string()));
    }

    /// A tool without the vault wire has no value to resolve, and guessing or
    /// sending an empty credential would produce a confusing 401 instead of a
    /// clear misconfiguration.
    #[test]
    fn a_vault_field_with_no_reachable_value_is_an_error() {
        let o = op(vec![
            p("room", ParamLocation::Path, Fill::agent()),
            p("Authorization", ParamLocation::Header, vault("other/KEY")),
        ]);
        let err = build_request(
            &cfg(vec![o.clone()]),
            &o,
            &serde_json::json!({"room": "g"}),
            &secrets(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("other/KEY"), "{err}");
        assert!(err.contains("cannot read"), "{err}");
    }

    /// A path value is data, not structure. Without encoding, `..%2f` and a
    /// literal `/` address a different resource than the operation names.
    #[test]
    fn a_path_value_cannot_change_which_resource_is_addressed() {
        let o = op(vec![p("room", ParamLocation::Path, Fill::agent())]);
        let got = build_request(
            &cfg(vec![o.clone()]),
            &o,
            &serde_json::json!({"room": "../../admin/keys"}),
            &secrets(),
        )
        .unwrap();
        assert_eq!(
            got.url,
            "https://api.example.com/messages/..%2F..%2Fadmin%2Fkeys"
        );
        assert!(!got.url.contains("/admin/keys"));
    }

    /// A query value must not be able to add another parameter.
    #[test]
    fn a_query_value_cannot_smuggle_another_parameter() {
        let o = ToolOperation {
            path: "/search".into(),
            params: vec![p("q", ParamLocation::Query, Fill::agent())],
            ..op(vec![])
        };
        let got = build_request(
            &cfg(vec![o.clone()]),
            &o,
            &serde_json::json!({"q": "x&admin=true"}),
            &secrets(),
        )
        .unwrap();
        assert!(got.url.ends_with("?q=x%26admin%3Dtrue"), "{}", got.url);
    }

    #[test]
    fn a_required_field_that_is_missing_says_which_one() {
        let mut param = p("room", ParamLocation::Path, Fill::agent());
        param.required = true;
        let o = op(vec![param]);
        let err = build_request(
            &cfg(vec![o.clone()]),
            &o,
            &serde_json::json!({}),
            &secrets(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("room"), "{err}");
        assert!(err.contains("required"), "{err}");
    }

    /// An unfilled placeholder would be sent literally, and `/messages/{room}`
    /// is a real path that answers something confusing rather than erroring.
    #[test]
    fn an_unfilled_path_placeholder_is_refused_before_sending() {
        let o = op(vec![p("room", ParamLocation::Path, Fill::agent())]);
        let err = build_request(
            &cfg(vec![o.clone()]),
            &o,
            &serde_json::json!({}),
            &secrets(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("room"), "{err}");
    }

    #[test]
    fn a_disabled_operation_cannot_be_called() {
        let mut o = op(vec![]);
        o.enabled = false;
        let err = build_request(
            &cfg(vec![o.clone()]),
            &o,
            &serde_json::json!({}),
            &secrets(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("disabled"), "{err}");
    }

    /// The whole point of "copy as curl": an operator can reproduce the call,
    /// and can paste it somewhere without handing over a credential the agent
    /// itself was never allowed to see.
    #[test]
    fn the_curl_rendering_masks_every_static_and_vault_value() {
        let o = op(vec![
            p("room", ParamLocation::Path, Fill::agent()),
            p(
                "Authorization",
                ParamLocation::Header,
                vault("creds/API_KEY"),
            ),
            p("X-Env", ParamLocation::Header, fixed("prod-secret-name")),
            p("text", ParamLocation::Body, Fill::agent()),
        ]);
        let prepared = build_request(
            &cfg(vec![o.clone()]),
            &o,
            &serde_json::json!({"room": "g", "text": "hi"}),
            &secrets(),
        )
        .unwrap();
        let curl = curl_for(&prepared);

        assert!(
            !curl.contains("sk-SUPER-SECRET"),
            "vault value leaked: {curl}"
        );
        assert!(
            !curl.contains("prod-secret-name"),
            "static value leaked: {curl}"
        );
        assert_eq!(curl.matches("<redacted>").count(), 2, "{curl}");
        // ...and it is still a usable command with the agent's own data in it.
        assert!(curl.starts_with("curl -X POST"), "{curl}");
        assert!(
            curl.contains("https://api.example.com/messages/g"),
            "{curl}"
        );
        assert!(
            curl.contains("hi"),
            "the agent's own body is not secret: {curl}"
        );
    }

    /// A secret that appears in the URL (a query-mode vault fill) must be
    /// masked there too, not only in headers.
    #[test]
    fn a_secret_in_the_url_is_masked_as_well() {
        let o = ToolOperation {
            path: "/search".into(),
            params: vec![p("token", ParamLocation::Query, vault("creds/API_KEY"))],
            ..op(vec![])
        };
        let prepared = build_request(
            &cfg(vec![o.clone()]),
            &o,
            &serde_json::json!({}),
            &secrets(),
        )
        .unwrap();
        let curl = curl_for(&prepared);
        assert!(!curl.contains("sk-SUPER-SECRET"), "{curl}");
        assert!(curl.contains("<redacted>"), "{curl}");
    }

    /// ADVERSARY 022/1. A secret is stored PERCENT-ENCODED in the url, so a
    /// mask that searched for the raw string missed every credential
    /// containing a character outside the unreserved set — which is every
    /// base64 credential. The header placement was masked and the query
    /// placement was not, which is the worst kind of gap: it looks like it
    /// works.
    #[test]
    fn a_secret_is_masked_in_the_url_even_though_it_is_encoded_there() {
        let raw = "sk/live+abc==";
        let creds = HashMap::from([("creds/API_KEY".to_string(), raw.to_string())]);

        // Query placement.
        let o = ToolOperation {
            path: "/search".into(),
            params: vec![p("key", ParamLocation::Query, vault("creds/API_KEY"))],
            ..op(vec![])
        };
        let curl = curl_for(
            &build_request(&cfg(vec![o.clone()]), &o, &serde_json::json!({}), &creds).unwrap(),
        );
        assert!(!curl.contains(raw), "raw secret in curl: {curl}");
        assert!(
            !curl.contains("sk%2Flive%2Babc%3D%3D"),
            "ENCODED secret survived in the url: {curl}"
        );
        assert!(curl.contains("<redacted>"), "{curl}");

        // Path placement, same encoding, same requirement.
        let o = ToolOperation {
            path: "/keys/{key}".into(),
            params: vec![p("key", ParamLocation::Path, vault("creds/API_KEY"))],
            ..op(vec![])
        };
        let curl = curl_for(
            &build_request(&cfg(vec![o.clone()]), &o, &serde_json::json!({}), &creds).unwrap(),
        );
        assert!(!curl.contains(raw), "{curl}");
        assert!(!curl.contains("sk%2Flive%2Babc%3D%3D"), "{curl}");

        // And a header, which already worked — so the fix did not trade one
        // placement for another.
        let o = ToolOperation {
            path: "/x".into(),
            params: vec![p(
                "Authorization",
                ParamLocation::Header,
                vault("creds/API_KEY"),
            )],
            ..op(vec![])
        };
        let curl = curl_for(
            &build_request(&cfg(vec![o.clone()]), &o, &serde_json::json!({}), &creds).unwrap(),
        );
        assert!(!curl.contains(raw), "{curl}");
    }

    /// ADVERSARY 022/2. `;` is legal in a header value, so reqwest does not
    /// reject it the way it rejects CRLF — encoding is the only thing between
    /// an agent and a forged session cookie.
    #[test]
    fn a_cookie_value_cannot_inject_more_cookies() {
        let o = ToolOperation {
            path: "/x".into(),
            params: vec![p("sid", ParamLocation::Cookie, Fill::agent())],
            ..op(vec![])
        };
        let got = build_request(
            &cfg(vec![o.clone()]),
            &o,
            &serde_json::json!({"sid": "x; admin=true; role=root"}),
            &secrets(),
        )
        .unwrap();

        let (_, value) = &got.cookies[0];
        assert!(!value.contains(';'), "cookie separator survived: {value}");
        assert!(!value.contains(' '), "{value}");
        assert!(
            !value.contains('='),
            "an `=` would still split a pair: {value}"
        );

        // ...and it is still gone once the jar is joined for the wire.
        let curl = curl_for(&got);
        assert!(
            !curl.contains("admin=true"),
            "injected cookie in curl: {curl}"
        );
        assert!(!curl.contains("role=root"), "{curl}");
    }

    /// A vault-filled cookie must be masked too, in its encoded form.
    #[test]
    fn a_secret_cookie_is_masked_in_the_curl() {
        let creds = HashMap::from([("creds/API_KEY".to_string(), "sk/live+abc==".to_string())]);
        let o = ToolOperation {
            path: "/x".into(),
            params: vec![p("session", ParamLocation::Cookie, vault("creds/API_KEY"))],
            ..op(vec![])
        };
        let curl = curl_for(
            &build_request(&cfg(vec![o.clone()]), &o, &serde_json::json!({}), &creds).unwrap(),
        );
        assert!(!curl.contains("sk/live+abc=="), "{curl}");
        assert!(!curl.contains("sk%2Flive%2Babc%3D%3D"), "{curl}");
        assert!(curl.contains("<redacted>"), "{curl}");
    }

    #[test]
    fn shell_metacharacters_in_a_curl_cannot_escape_their_quotes() {
        let o = ToolOperation {
            path: "/search".into(),
            params: vec![p("q", ParamLocation::Query, Fill::agent())],
            ..op(vec![])
        };
        let prepared = build_request(
            &cfg(vec![o.clone()]),
            &o,
            &serde_json::json!({"q": "'; rm -rf /; echo '"}),
            &secrets(),
        )
        .unwrap();
        let curl = curl_for(&prepared);
        // The value is percent-encoded into the URL and the whole argument is
        // single-quoted, so nothing reaches a shell as syntax.
        assert!(!curl.contains("; rm -rf"), "{curl}");
    }

    /// §3d rule 4. These are the addresses an imported spec should never be
    /// able to send the engine to, whatever the document said.
    #[test]
    fn internal_addresses_are_not_reachable_destinations() {
        for host in [
            "localhost",
            "127.0.0.1",
            "0.0.0.0",
            "10.0.0.5",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254",
            "[::1]",
            "[fe80::1]",
            "[fc00::1]",
            "postgres.railway.internal",
            "host.internal",
            "printer.local",
            "100.64.0.1",
        ] {
            assert!(
                wheel_core::host_is_denied(host),
                "{host} must not be reachable"
            );
        }
        for host in ["api.example.com", "1.1.1.1", "example.co.uk"] {
            assert!(!wheel_core::host_is_denied(host), "{host} must be allowed");
        }
    }

    /// The cloud metadata endpoint by any spelling. This is the single most
    /// valuable target for an SSRF on a hosted machine.
    #[test]
    fn the_metadata_endpoint_is_denied_however_it_is_written() {
        for ip in [
            "169.254.169.254",
            "169.254.170.2",
            // IPv4-mapped IPv6, which is the same address wearing a hat.
            "::ffff:169.254.169.254",
        ] {
            let parsed: IpAddr = ip.parse().unwrap();
            assert!(!ip_allowed(parsed), "{ip} must be denied");
        }
    }

    #[tokio::test]
    async fn a_url_that_is_not_http_is_refused_before_any_lookup() {
        for url in [
            "file:///etc/passwd",
            "gopher://example.com/",
            "ftp://example.com/",
        ] {
            let err = resolve_and_check(url).await.unwrap_err().to_string();
            assert!(err.contains("not an http(s) url"), "{url}: {err}");
        }
    }

    #[tokio::test]
    async fn a_private_host_is_refused_without_being_resolved() {
        for url in [
            "http://127.0.0.1:7000/v1/board",
            "http://localhost/",
            "http://postgres.railway.internal:5432/",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            let err = resolve_and_check(url).await.unwrap_err().to_string();
            assert!(err.contains("not a reachable destination"), "{url}: {err}");
        }
    }

    /// A relative redirect must resolve against where it came from, and an
    /// absolute one must be taken as given — both then get re-checked.
    #[test]
    fn a_redirect_target_resolves_the_way_a_browser_would() {
        assert_eq!(
            join_redirect("https://a.example/x/y", "/z").unwrap(),
            "https://a.example/z"
        );
        assert_eq!(
            join_redirect("https://a.example/x/y", "z").unwrap(),
            "https://a.example/x/z"
        );
        assert_eq!(
            join_redirect("https://a.example/x", "http://127.0.0.1/").unwrap(),
            "http://127.0.0.1/"
        );
    }
}
