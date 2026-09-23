// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! What the sandbox host may ask the docker daemon for. **Default DENY.**
//!
//! Access to the docker socket is root on the machine: a `create` with `Privileged`, or with a bind
//! of `/`, is a container escape in one request. The host is the only thing that holds it, and the
//! host proxies tenant traffic, so "the host has a bug" is the risk this closes. The host makes a
//! handful of calls (`DockerSandbox`), all on names derived from a uuid the API generated, so the
//! allowlist is short enough to state in full and to refuse everything else.
//!
//! Three rules make it hold:
//!
//! * **The daemon never receives the caller's bytes.** A request is validated and then a FRESH body
//!   and target are built from the checked values. Go's JSON decoding is case-insensitive (with
//!   special folds) and ignores unknown keys; serde is neither. Forwarding what was checked would
//!   let a key serde calls "unknown" be one the daemon applies. Here an exact-case allowlist refuses
//!   every variant, and what is forwarded is rebuilt, so the two decoders cannot disagree.
//! * **Values must EQUAL what the operator configured**, not merely be present: `Memory: 0` is
//!   "unlimited", and "present" is not a check.
//! * Every name, and every id in a body, must agree with the id in the URL, so a request cannot be
//!   admitted for one project and act on another.
//!
//! Pure: no I/O, no clock. The proxy in `super` owns the socket.

use serde_json::{json, Value};
use uuid::Uuid;

pub const PROJECT_LABEL: &str = "wheel.project";
/// Hash of the full create configuration; the host recreates a container whose label is stale.
pub const SPEC_LABEL: &str = "wheel.spec";

/// The list query the host uses to find its own containers, exactly as a client percent-encodes it.
const LIST_FILTERS_ENCODED: &str = "%7B%22label%22%3A%5B%22wheel.project%22%5D%7D";

/// What the operator configured; the proxy admits exactly this and nothing else.
#[derive(Debug, Clone)]
pub struct Policy {
    /// The one image a tenant container may run, compared as a string.
    pub image: String,
    /// The one network a tenant container may join.
    pub network: String,
    /// Exact values. The host and the proxy are given the same `CONTAINER_*` settings.
    pub memory: i64,
    pub nano_cpus: i64,
    pub pids_limit: i64,
    pub engine_port: u16,
    /// The engine's channel. `None`: it listens on TCP inside the shared network (`M1`). `Some(root)`:
    /// it listens on a unix socket in `<root>/<project uuid>/`, bind-mounted at `/run/wheel`, and the
    /// container needs no reachable address at all. Exactly one form is admitted, never both.
    pub run_root: Option<RunRoot>,
    /// The most project containers (and volumes) this proxy will let exist; 0 means no ceiling. A
    /// compromised host could otherwise create containers up to the per-container limits until the
    /// machine runs out of memory or disk.
    pub max_projects: usize,
}

/// A host directory that holds nothing but per-project socket directories.
///
/// Its own type because it is the one HOST path the proxy ever lets into a container, so it is
/// checked once, here, rather than trusted wherever it is used: absolute, canonical, and free of
/// anything that changes what a bind string means (`:` separates fields, `..` and `//` re-route it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRoot(String);

impl RunRoot {
    pub fn new(path: &str) -> Result<Self, String> {
        let ok = path.starts_with('/')
            && path.len() > 1
            && !path.ends_with('/')
            && path.is_ascii()
            && !path.contains("//")
            && !path.contains(':')
            && !path
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == ',')
            && path.split('/').all(|seg| seg != "." && seg != "..");
        if ok && path != "/" {
            Ok(Self(path.to_string()))
        } else {
            Err(format!(
                "{path:?} is not a canonical absolute directory path"
            ))
        }
    }

    fn bind_for(&self, id: &Uuid) -> String {
        format!("{}/{id}:/run/wheel", self.0)
    }
}

/// Where the engine listens when it has a unix socket channel.
pub const ENGINE_SOCKET: &str = "unix:///run/wheel/engine.sock";

/// What the proxy does with the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// The daemon's status with an empty body on success; a fixed body on failure (error messages
    /// can echo input).
    Fixed,
    /// A container create: reduced to its id.
    Create,
    /// A volume create: reduced to its name and driver.
    Volume,
    /// A container inspection, reduced to the few non-secret fields the host reads.
    Inspect,
    /// A container list, reduced to name, project label, spec label and state.
    List,
    /// A volume list, reduced to name and the project label.
    VolumeList,
}

/// An admitted request, ready to send: nothing in it came from the caller unchecked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    pub method: &'static str,
    /// Path and query, rebuilt from parsed components.
    pub target: String,
    pub body: Option<Vec<u8>>,
    pub reply: Reply,
}

/// Why a request was refused. Safe to log and to return: it never quotes a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied(pub String);

fn deny<T>(why: impl Into<String>) -> Result<T, Denied> {
    Err(Denied(why.into()))
}

type Verdict = Result<Admission, Denied>;

/// The request the proxy sends itself, internally, to enforce the project ceiling. Not reachable
/// by a caller through `decide` — nothing about it comes from outside this process.
pub fn list_admission() -> Admission {
    Admission {
        method: "GET",
        target: format!("/containers/json?all=true&filters={LIST_FILTERS_ENCODED}"),
        body: None,
        reply: Reply::List,
    }
}

/// As [`list_admission`], for volumes — the ceiling has to count these too: `POST
/// /volumes/create` is one of the two admission kinds it gates, and a volume can exist with no
/// matching container (the host creates the volume first), so counting containers alone leaves
/// volume creation completely uncapped.
pub fn volume_list_admission() -> Admission {
    Admission {
        method: "GET",
        target: format!("/volumes?filters={LIST_FILTERS_ENCODED}"),
        body: None,
        reply: Reply::VolumeList,
    }
}

pub fn container_name(id: &Uuid) -> String {
    format!("wheel-p-{id}")
}

pub fn volume_name(id: &Uuid) -> String {
    format!("wheel-p-{id}-data")
}

/// The id in `wheel-p-<uuid>` — only the canonical lowercase hyphenated form, because `Uuid`'s own
/// parser also accepts braced, urn and simple spellings that would be a second name for one project.
fn id_of(name: &str, suffix: &str) -> Option<Uuid> {
    let rest = name.strip_prefix("wheel-p-")?.strip_suffix(suffix)?;
    let id = Uuid::parse_str(rest).ok()?;
    (id.hyphenated().to_string() == rest).then_some(id)
}

fn container_id(name: &str) -> Result<Uuid, Denied> {
    id_of(name, "").ok_or_else(|| Denied("not a project container name".into()))
}

fn volume_id(name: &str) -> Result<Uuid, Denied> {
    id_of(name, "-data").ok_or_else(|| Denied("not a project volume name".into()))
}

/// The API-version prefix (if any) and the path segments after it. Refuses anything that could be
/// read two ways: percent-encoding, backslashes, empty or dot segments, non-ASCII.
fn segments(path: &str) -> Result<(Option<&str>, Vec<&str>), Denied> {
    if !path.starts_with('/')
        || !path.is_ascii()
        || path.contains('%')
        || path.contains('\\')
        || path.contains("//")
    {
        return deny("path is not in canonical form");
    }
    let mut segs: Vec<&str> = path[1..].split('/').collect();
    let mut version = None;
    if segs.first().is_some_and(|s| {
        s.strip_prefix('v')
            .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.'))
    }) {
        version = Some(segs.remove(0));
    }
    if segs.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return deny("path is not in canonical form");
    }
    Ok((version, segs))
}

/// `k=v&k=v`, refusing repeats and valueless keys. Percent-encoding is refused here; the one query
/// that legitimately carries it (`filters`) is matched whole before this runs.
fn query(q: Option<&str>) -> Result<Vec<(&str, &str)>, Denied> {
    let Some(q) = q.filter(|q| !q.is_empty()) else {
        return Ok(Vec::new());
    };
    if q.contains('%') || !q.is_ascii() {
        return deny("query is not in canonical form");
    }
    let mut out: Vec<(&str, &str)> = Vec::new();
    for pair in q.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            return deny("query is not in canonical form");
        };
        if out.iter().any(|(seen, _)| *seen == k) {
            return deny("repeated query parameter");
        }
        out.push((k, v));
    }
    Ok(out)
}

/// Each parameter must be on `allowed`, with a value `ok` accepts.
fn only_params(
    q: &[(&str, &str)],
    allowed: &[&str],
    ok: impl Fn(&str, &str) -> bool,
) -> Result<(), Denied> {
    for (k, v) in q {
        if !allowed.contains(k) || !ok(k, v) {
            return deny(format!("query parameter {k:?} is not permitted here"));
        }
    }
    Ok(())
}

fn rebuild(version: Option<&str>, segs: &[&str], q: &[(&str, &str)]) -> String {
    let mut target = String::new();
    if let Some(v) = version {
        target.push('/');
        target.push_str(v);
    }
    for s in segs {
        target.push('/');
        target.push_str(s);
    }
    for (i, (k, v)) in q.iter().enumerate() {
        target.push(if i == 0 { '?' } else { '&' });
        target.push_str(k);
        target.push('=');
        target.push_str(v);
    }
    target
}

impl Policy {
    /// Decide one request. `target` is the request target as received (path and optional query).
    pub fn decide(&self, method: &str, target: &str, body: &[u8]) -> Verdict {
        let (path, raw_query) = match target.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (target, None),
        };
        let (version, segs) = segments(path)?;

        // The one query that needs percent-encoding: matched whole, then never decoded. Anchored
        // on the start of the string or on a preceding `&` — never a bare substring match — so a
        // parameter whose VALUE happens to contain the text `filters=` cannot be mistaken for the
        // real one.
        let is_list = method == "GET" && (segs == ["containers", "json"] || segs == ["volumes"]);
        let q = if is_list {
            let raw_query = raw_query.unwrap_or("");
            let head_and_rest = raw_query
                .strip_prefix("filters=")
                .map(|rest| ("", rest))
                .or_else(|| raw_query.split_once("&filters="));
            match head_and_rest {
                Some((head, LIST_FILTERS_ENCODED)) => query(Some(head))?,
                _ => return deny("a list may only be filtered to project objects"),
            }
        } else {
            query(raw_query)?
        };
        let bool_word = |v: &str| matches!(v, "true" | "false");

        let needs_body = matches!(
            (method, segs.as_slice()),
            ("POST", ["containers", "create"]) | ("POST", ["volumes", "create"])
        );
        if !needs_body && !body.is_empty() {
            return deny("this call takes no body");
        }

        match (method, segs.as_slice()) {
            ("GET", ["containers", "json"]) => {
                only_params(&q, &["all", "size"], |k, v| match k {
                    "all" => v == "true",
                    _ => v == "false",
                })?;
                let mut target = rebuild(version, &segs, &q);
                target.push(if q.is_empty() { '?' } else { '&' });
                target.push_str("filters=");
                target.push_str(LIST_FILTERS_ENCODED);
                Ok(Admission {
                    method: "GET",
                    target,
                    body: None,
                    reply: Reply::List,
                })
            }
            ("GET", ["volumes"]) => {
                only_params(&q, &[], |_, _| false)?;
                let mut target = rebuild(version, &segs, &q);
                target.push(if q.is_empty() { '?' } else { '&' });
                target.push_str("filters=");
                target.push_str(LIST_FILTERS_ENCODED);
                Ok(Admission {
                    method: "GET",
                    target,
                    body: None,
                    reply: Reply::VolumeList,
                })
            }
            ("GET", ["containers", name, "json"]) => {
                container_id(name)?;
                only_params(&q, &["size"], |_, v| matches!(v, "false" | "0"))?;
                Ok(Admission {
                    method: "GET",
                    target: rebuild(version, &segs, &q),
                    body: None,
                    reply: Reply::Inspect,
                })
            }
            ("POST", ["containers", "create"]) => {
                only_params(&q, &["name", "platform"], |k, v| {
                    k == "name" || v.is_empty()
                })?;
                let Some((_, name)) = q.iter().find(|(k, _)| *k == "name") else {
                    return deny("a container must be created with a name");
                };
                let id = container_id(name)?;
                let body = self.container_body(&id, body)?;
                // Rebuilt: only the name, in its canonical form.
                let target = rebuild(version, &segs, &[("name", name)]);
                Ok(Admission {
                    method: "POST",
                    target,
                    body: Some(body),
                    reply: Reply::Create,
                })
            }
            ("POST", ["containers", name, "start"]) => {
                container_id(name)?;
                only_params(&q, &[], |_, _| false)?;
                Ok(plain("POST", rebuild(version, &segs, &q)))
            }
            ("POST", ["containers", name, "stop"]) => {
                container_id(name)?;
                only_params(&q, &["t"], |_, v| {
                    !v.is_empty() && v.len() <= 4 && v.chars().all(|c| c.is_ascii_digit())
                })?;
                Ok(plain("POST", rebuild(version, &segs, &q)))
            }
            ("DELETE", ["containers", name]) => {
                container_id(name)?;
                // `link=true` removes a link between containers, so only `false` is admitted; `v`
                // removes anonymous volumes with the container, which is what a delete must do.
                only_params(&q, &["force", "v", "link"], |k, v| match k {
                    "link" => v == "false",
                    _ => bool_word(v),
                })?;
                Ok(plain("DELETE", rebuild(version, &segs, &q)))
            }
            ("POST", ["volumes", "create"]) => {
                only_params(&q, &[], |_, _| false)?;
                let body = self.volume_body(body)?;
                Ok(Admission {
                    method: "POST",
                    target: rebuild(version, &segs, &q),
                    body: Some(body),
                    reply: Reply::Volume,
                })
            }
            ("DELETE", ["volumes", name]) => {
                volume_id(name)?;
                only_params(&q, &["force"], |_, v| bool_word(v))?;
                Ok(plain("DELETE", rebuild(version, &segs, &q)))
            }
            _ => deny("this docker API call is not one the sandbox host makes"),
        }
    }

    fn volume_body(&self, body: &[u8]) -> Result<Vec<u8>, Denied> {
        let v = json_object(body)?;
        let obj = v.as_object().expect("json_object returns an object");
        // `DriverOpts` is not on the list, so a `local` volume with `type=none,o=bind,device=/` — a
        // bind mount of any host path through the volume API — cannot be spelled at all.
        only_keys(obj, &["Name", "Labels", "Driver"], "volume")?;
        let id = volume_id(str_field(obj, "Name")?)?;
        if let Some(driver) = obj.get("Driver").filter(|d| !d.is_null()) {
            if driver.as_str() != Some("local") {
                return deny("volume driver must be local");
            }
        }
        let labels = labels(obj.get("Labels"), &id, false)?;
        Ok(json!({ "Name": volume_name(&id), "Labels": labels })
            .to_string()
            .into_bytes())
    }

    fn container_body(&self, id: &Uuid, body: &[u8]) -> Result<Vec<u8>, Denied> {
        let v = json_object(body)?;
        let obj = v.as_object().expect("json_object returns an object");
        only_keys(obj, &["Image", "Env", "Labels", "HostConfig"], "container")?;

        if str_field(obj, "Image")? != self.image {
            return deny("image is not the configured engine image");
        }
        let labels = labels(obj.get("Labels"), id, true)?;
        let env = self.env(id, obj.get("Env"))?;
        let host_config = self.host_config(id, obj.get("HostConfig"))?;
        Ok(json!({
            "Image": self.image,
            "Env": env,
            "Labels": labels,
            "HostConfig": host_config,
        })
        .to_string()
        .into_bytes())
    }

    /// The validated environment, in a fixed order.
    fn env(&self, id: &Uuid, env: Option<&Value>) -> Result<Vec<String>, Denied> {
        let Some(list) = env.and_then(Value::as_array) else {
            return deny("Env is required");
        };
        let listen = match self.run_root {
            Some(_) => ENGINE_SOCKET.to_string(),
            None => format!("tcp://0.0.0.0:{}", self.engine_port),
        };
        let mut found: Vec<(&str, &str)> = Vec::new();
        for entry in list {
            let Some((k, val)) = entry.as_str().and_then(|e| e.split_once('=')) else {
                return deny("Env entries must be KEY=VALUE strings");
            };
            if found.iter().any(|(seen, _)| *seen == k) {
                return deny("Env repeats a key");
            }
            let fits = match k {
                "WHEEL_PROJECT_ID" => val == id.to_string(),
                "WHEEL_ENGINE_SECRET" | "WHEEL_VAULT_KEY" => {
                    !val.is_empty() && val.len() <= 512 && val.chars().all(|c| c.is_ascii_graphic())
                }
                "WHEEL_HARNESS_AUTH" => matches!(val, "api-key-only" | "oauth-token"),
                "WHEEL_LISTEN" => val == listen,
                "WHEEL_DATA_DIR" => val == "/data",
                "WHEEL_LOG" => val == "json",
                "WHEEL_ROLE" => val == "engine",
                _ => return deny(format!("Env key {k:?} is not permitted")),
            };
            if !fits {
                return deny(format!("Env value for {k:?} is not permitted"));
            }
            found.push((k, val));
        }
        const ORDER: [&str; 8] = [
            "WHEEL_PROJECT_ID",
            "WHEEL_ENGINE_SECRET",
            "WHEEL_VAULT_KEY",
            "WHEEL_HARNESS_AUTH",
            "WHEEL_LISTEN",
            "WHEEL_DATA_DIR",
            "WHEEL_LOG",
            "WHEEL_ROLE",
        ];
        ORDER
            .iter()
            .map(|key| {
                found
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(k, v)| format!("{k}={v}"))
                    .ok_or_else(|| Denied(format!("Env is missing {key}")))
            })
            .collect()
    }

    fn host_config(&self, id: &Uuid, hc: Option<&Value>) -> Result<Value, Denied> {
        let Some(hc) = hc.and_then(Value::as_object) else {
            return deny("HostConfig is required");
        };
        only_keys(
            hc,
            &[
                "CapDrop",
                "SecurityOpt",
                "Memory",
                "MemorySwap",
                "NanoCpus",
                "PidsLimit",
                "NetworkMode",
                "Binds",
                "RestartPolicy",
            ],
            "HostConfig",
        )?;
        let strings = |key: &str| -> Result<Vec<&str>, Denied> {
            hc.get(key)
                .and_then(Value::as_array)
                .and_then(|a| a.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
                .ok_or_else(|| Denied(format!("HostConfig.{key} is required")))
        };
        if strings("CapDrop")? != ["ALL"] {
            return deny("HostConfig.CapDrop must be exactly [\"ALL\"]");
        }
        if strings("SecurityOpt")? != ["no-new-privileges"] {
            return deny("HostConfig.SecurityOpt must be exactly [\"no-new-privileges\"]");
        }
        let mut binds = vec![format!("{}:/data", volume_name(id))];
        if let Some(root) = &self.run_root {
            binds.push(root.bind_for(id));
        }
        if strings("Binds")? != binds.iter().map(String::as_str).collect::<Vec<_>>() {
            return deny(
                "HostConfig.Binds must be exactly this project's data volume (and, with a run \
                 root, its own socket directory)",
            );
        }
        if str_field(hc, "NetworkMode")? != self.network {
            return deny("HostConfig.NetworkMode is not the tenant network");
        }
        for (key, exact) in [
            ("Memory", self.memory),
            // Swap equal to memory means no swap at all; left unset a container gets twice its
            // memory limit in swap.
            ("MemorySwap", self.memory),
            ("NanoCpus", self.nano_cpus),
            ("PidsLimit", self.pids_limit),
        ] {
            if hc.get(key).and_then(Value::as_i64) != Some(exact) {
                return deny(format!("HostConfig.{key} must equal the configured limit"));
            }
        }
        let restart = hc.get("RestartPolicy").and_then(Value::as_object);
        let Some(r) = restart else {
            return deny("RestartPolicy is required");
        };
        only_keys(r, &["Name"], "RestartPolicy")?;
        if r.get("Name").and_then(Value::as_str) != Some("unless-stopped") {
            return deny("RestartPolicy must be unless-stopped");
        }
        Ok(json!({
            "CapDrop": ["ALL"],
            "SecurityOpt": ["no-new-privileges"],
            "Memory": self.memory,
            "MemorySwap": self.memory,
            "NanoCpus": self.nano_cpus,
            "PidsLimit": self.pids_limit,
            "NetworkMode": self.network,
            "Binds": binds,
            "RestartPolicy": { "Name": "unless-stopped" },
        }))
    }
}

fn plain(method: &'static str, target: String) -> Admission {
    Admission {
        method,
        target,
        body: None,
        reply: Reply::Fixed,
    }
}

/// A body must be one JSON object and nothing after it: the daemon reads the first value and would
/// ignore trailing bytes that serde refuses, so refusing them keeps the two in step.
fn json_object(body: &[u8]) -> Result<Value, Denied> {
    match serde_json::from_slice::<Value>(body) {
        Ok(v) if v.is_object() => Ok(v),
        _ => deny("body must be a JSON object"),
    }
}

fn only_keys(
    obj: &serde_json::Map<String, Value>,
    allowed: &[&str],
    what: &str,
) -> Result<(), Denied> {
    match obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        // The key is named, its value never is: it may be a secret.
        Some(k) => deny(format!("{what} field {k:?} is not permitted")),
        None => Ok(()),
    }
}

fn str_field<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str, Denied> {
    obj.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Denied(format!("{key} is required")))
}

/// The project label (required) and optionally the spec hash; nothing else. Rebuilt for the daemon.
fn labels(labels: Option<&Value>, id: &Uuid, allow_spec: bool) -> Result<Value, Denied> {
    let Some(map) = labels.and_then(Value::as_object) else {
        return deny("Labels must carry the project label");
    };
    only_keys(
        map,
        if allow_spec {
            &[PROJECT_LABEL, SPEC_LABEL]
        } else {
            &[PROJECT_LABEL]
        },
        "Labels",
    )?;
    match map.get(PROJECT_LABEL).and_then(Value::as_str) {
        Some(v) if v == id.to_string() => {}
        _ => return deny("the project label does not match the name"),
    }
    let mut out = serde_json::Map::new();
    out.insert(PROJECT_LABEL.into(), json!(id.to_string()));
    if let Some(spec) = map.get(SPEC_LABEL) {
        match spec.as_str() {
            Some(h)
                if h.len() == 64
                    && h.chars()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) =>
            {
                out.insert(SPEC_LABEL.into(), spec.clone());
            }
            _ => return deny("the spec label must be a lowercase sha256"),
        }
    }
    Ok(Value::Object(out))
}

/// Reduce a container inspection to what the host reads and nothing a future docker adds.
pub fn project_inspect(raw: &[u8]) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_slice(raw).ok()?;
    let status = v.pointer("/State/Status")?.as_str()?;
    let mut state = serde_json::Map::new();
    state.insert("Status".into(), json!(status));
    if let Some(h) = v.pointer("/State/Health/Status").and_then(Value::as_str) {
        state.insert("Health".into(), json!({ "Status": h }));
    }
    let mut labels = serde_json::Map::new();
    for key in [PROJECT_LABEL, SPEC_LABEL] {
        if let Some(l) = v
            .pointer("/Config/Labels")
            .and_then(|l| l.get(key))
            .and_then(Value::as_str)
        {
            labels.insert(key.into(), json!(l));
        }
    }
    serde_json::to_vec(&json!({ "State": state, "Config": { "Labels": labels } })).ok()
}

/// Reduce a container list to project containers, by name.
pub fn project_list(raw: &[u8]) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_slice(raw).ok()?;
    let mut out = Vec::new();
    for c in v.as_array()? {
        let name = c
            .get("Names")
            .and_then(Value::as_array)
            .and_then(|n| {
                n.iter()
                    .filter_map(Value::as_str)
                    .find_map(|n| n.strip_prefix('/'))
            })
            .filter(|n| container_id(n).is_ok());
        let Some(name) = name else { continue };
        let mut labels = serde_json::Map::new();
        for key in [PROJECT_LABEL, SPEC_LABEL] {
            if let Some(l) = c
                .pointer("/Labels")
                .and_then(|l| l.get(key))
                .and_then(Value::as_str)
            {
                labels.insert(key.into(), json!(l));
            }
        }
        out.push(json!({
            "Names": [format!("/{name}")],
            "Labels": labels,
            "State": c.get("State").and_then(Value::as_str).unwrap_or(""),
        }));
    }
    serde_json::to_vec(&out).ok()
}

/// A volume list, reduced the same way [`project_list`] reduces a container list — but kept
/// inside `{"Volumes": [...]}`, NOT a bare array like the container list: `GET /volumes` and `GET
/// /containers/json` disagree on their own envelope shape (Docker's real API does too — this is
/// not a simplification), and a real bollard client (`DockerSandbox`, reading this same reduced
/// response back through the proxy) fails to deserialise a bare array as a `VolumeListResponse`.
pub fn project_volume_list(raw: &[u8]) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_slice(raw).ok()?;
    let mut out = Vec::new();
    for vol in v.get("Volumes")?.as_array()? {
        let name = vol
            .get("Name")
            .and_then(Value::as_str)
            .filter(|n| volume_id(n).is_ok());
        let Some(name) = name else { continue };
        let project = vol
            .pointer("/Labels")
            .and_then(|l| l.get(PROJECT_LABEL))
            .and_then(Value::as_str);
        // `Driver`/`Mountpoint` are required by the real `Volume` model a bollard client
        // deserialises this into (`DockerSandbox` reads this reduced response back through the
        // proxy) — present so parsing succeeds, `Mountpoint` deliberately empty rather than a real
        // host path, which this reduction exists to keep out of a response at all.
        out.push(json!({
            "Name": name,
            "Driver": "local",
            "Mountpoint": "",
            "Scope": "local",
            "Options": {},
            "Labels": { PROJECT_LABEL: project },
        }));
    }
    serde_json::to_vec(&json!({ "Volumes": out })).ok()
}

/// A container create's answer, reduced to its id.
pub fn project_create(raw: &[u8]) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_slice(raw).ok()?;
    let id = v.get("Id")?.as_str()?;
    serde_json::to_vec(&json!({ "Id": id, "Warnings": [] })).ok()
}

/// A volume create's answer, reduced to what a client needs to believe it worked. The mountpoint is
/// a path on the host and nothing the sandbox host has any use for.
pub fn project_volume(raw: &[u8]) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_slice(raw).ok()?;
    let name = v.get("Name")?.as_str()?;
    serde_json::to_vec(&json!({
        "Name": name, "Driver": "local", "Mountpoint": "", "Labels": {}, "Scope": "local", "Options": {}
    }))
    .ok()
}

/// The fixed answer to a refused-or-failed daemon reply: error messages can echo the request.
pub fn fixed_error(status: u16) -> &'static str {
    match status {
        404 => "not found",
        409 => "conflict",
        304 => "not modified",
        _ => "the docker daemon could not do that",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy() -> Policy {
        Policy {
            image: "wheel-engine:test".into(),
            network: "wheel-tenants".into(),
            memory: 1 << 30,
            nano_cpus: 1_000_000_000,
            pids_limit: 512,
            engine_port: 7000,
            run_root: None,
            max_projects: 0,
        }
    }

    fn id() -> Uuid {
        Uuid::parse_str("3f2504e0-4f89-11d3-9a0c-0305e82c3301").unwrap()
    }

    /// The container `DockerSandbox::create` asks for. `tests/docker_proxy.rs` proves the real
    /// backend emits exactly this shape, so this is not a copy that can drift unnoticed.
    fn golden() -> Value {
        json!({
            "Image": "wheel-engine:test",
            "Env": [
                format!("WHEEL_PROJECT_ID={}", id()),
                "WHEEL_ENGINE_SECRET=engine-secret",
                "WHEEL_VAULT_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                "WHEEL_HARNESS_AUTH=api-key-only",
                "WHEEL_LISTEN=tcp://0.0.0.0:7000",
                "WHEEL_DATA_DIR=/data",
                "WHEEL_LOG=json",
                "WHEEL_ROLE=engine",
            ],
            "Labels": { "wheel.project": id().to_string() },
            "HostConfig": {
                "CapDrop": ["ALL"],
                "SecurityOpt": ["no-new-privileges"],
                "Memory": 1 << 30,
                "MemorySwap": 1 << 30,
                "NanoCpus": 1_000_000_000i64,
                "PidsLimit": 512,
                "NetworkMode": "wheel-tenants",
                "Binds": [format!("wheel-p-{}-data:/data", id())],
                "RestartPolicy": { "Name": "unless-stopped" },
            },
        })
    }

    fn create(p: &Policy, body: &Value) -> Verdict {
        let target = format!("/v1.49/containers/create?name=wheel-p-{}", id());
        p.decide("POST", &target, body.to_string().as_bytes())
    }

    fn refused(verdict: Verdict) -> String {
        verdict.expect_err("this request must be refused").0
    }

    #[test]
    fn the_hardened_container_the_host_asks_for_is_admitted() {
        let admitted = create(&policy(), &golden()).expect("the golden create is admitted");
        assert_eq!(admitted.reply, Reply::Create);
        assert_eq!(admitted.method, "POST");
    }

    /// Each row is a create that is a container escape, a tenant crossing, or an unpinned resource,
    /// and each must be refused for the reason named — not merely refused by accident of an earlier
    /// check. Removing any one guard turns exactly its row red.
    #[test]
    fn every_way_to_ask_for_a_less_hardened_container_is_refused() {
        type Edit = fn(&mut Value);
        let cases: Vec<(&str, Edit, &str)> = vec![
            (
                "privileged",
                |b| b["HostConfig"]["Privileged"] = json!(true),
                "\"Privileged\"",
            ),
            (
                "added capability",
                |b| b["HostConfig"]["CapAdd"] = json!(["SYS_ADMIN"]),
                "\"CapAdd\"",
            ),
            (
                "host pid namespace",
                |b| b["HostConfig"]["PidMode"] = json!("host"),
                "\"PidMode\"",
            ),
            (
                "host network mode",
                |b| b["HostConfig"]["NetworkMode"] = json!("host"),
                "NetworkMode",
            ),
            (
                "another network",
                |b| b["HostConfig"]["NetworkMode"] = json!("wheel"),
                "NetworkMode",
            ),
            (
                "a device",
                |b| b["HostConfig"]["Devices"] = json!([{"PathOnHost": "/dev/sda"}]),
                "\"Devices\"",
            ),
            (
                "a mount",
                |b| b["HostConfig"]["Mounts"] = json!([{"Type":"bind","Source":"/","Target":"/h"}]),
                "\"Mounts\"",
            ),
            (
                "the host root as a bind",
                |b| b["HostConfig"]["Binds"] = json!(["/:/host"]),
                "Binds",
            ),
            (
                "the docker socket as a bind",
                |b| b["HostConfig"]["Binds"] = json!(["/var/run/docker.sock:/var/run/docker.sock"]),
                "Binds",
            ),
            (
                "an extra bind",
                |b| {
                    let v = b["HostConfig"]["Binds"][0].clone();
                    b["HostConfig"]["Binds"] = json!([v, "/etc:/etc"]);
                },
                "Binds",
            ),
            (
                "another project's volume",
                |b| {
                    b["HostConfig"]["Binds"] =
                        json!([format!("wheel-p-{}-data:/data", Uuid::nil())]);
                },
                "Binds",
            ),
            (
                "no capability drop",
                |b| b["HostConfig"]["CapDrop"] = json!([]),
                "CapDrop",
            ),
            (
                "a partial capability drop",
                |b| b["HostConfig"]["CapDrop"] = json!(["NET_RAW"]),
                "CapDrop",
            ),
            (
                "privilege gain allowed",
                |b| b["HostConfig"]["SecurityOpt"] = json!(["seccomp=unconfined"]),
                "SecurityOpt",
            ),
            (
                "no security options",
                |b| {
                    b["HostConfig"]
                        .as_object_mut()
                        .unwrap()
                        .remove("SecurityOpt");
                },
                "SecurityOpt",
            ),
            (
                "an uncapped memory",
                |b| b["HostConfig"]["Memory"] = json!(0),
                "Memory",
            ),
            (
                "memory over the ceiling",
                |b| b["HostConfig"]["Memory"] = json!(1i64 << 40),
                "Memory",
            ),
            (
                "cpu over the ceiling",
                |b| b["HostConfig"]["NanoCpus"] = json!(64_000_000_000i64),
                "NanoCpus",
            ),
            (
                "no pids limit",
                |b| {
                    b["HostConfig"].as_object_mut().unwrap().remove("PidsLimit");
                },
                "PidsLimit",
            ),
            (
                "the user namespace of the host",
                |b| b["HostConfig"]["UsernsMode"] = json!("host"),
                "\"UsernsMode\"",
            ),
            (
                "a runtime",
                |b| b["HostConfig"]["Runtime"] = json!("runc"),
                "\"Runtime\"",
            ),
            (
                "published ports",
                |b| b["HostConfig"]["PortBindings"] = json!({"7000/tcp":[{"HostPort":"7000"}]}),
                "\"PortBindings\"",
            ),
            (
                "volumes from another container",
                |b| b["HostConfig"]["VolumesFrom"] = json!(["wheel-host"]),
                "\"VolumesFrom\"",
            ),
            (
                "a cgroup parent",
                |b| b["HostConfig"]["CgroupParent"] = json!("/"),
                "\"CgroupParent\"",
            ),
            (
                "a restart policy that is not unless-stopped",
                |b| b["HostConfig"]["RestartPolicy"] = json!({"Name":"always"}),
                "RestartPolicy",
            ),
            (
                "a network attachment with aliases",
                |b| {
                    b["NetworkingConfig"] =
                        json!({"EndpointsConfig":{"wheel-tenants":{"Aliases":["wheeld"]}}})
                },
                "\"NetworkingConfig\"",
            ),
            (
                "another container's ipc namespace",
                |b| b["HostConfig"]["IpcMode"] = json!("container:victim"),
                "\"IpcMode\"",
            ),
            (
                "another container's uts namespace",
                |b| b["HostConfig"]["UTSMode"] = json!("host"),
                "\"UTSMode\"",
            ),
            (
                "unmasked proc paths",
                |b| b["HostConfig"]["MaskedPaths"] = json!([]),
                "\"MaskedPaths\"",
            ),
            (
                "a negative oom score",
                |b| b["HostConfig"]["OomScoreAdj"] = json!(-1000),
                "\"OomScoreAdj\"",
            ),
            (
                "a log driver that dials out",
                |b| {
                    b["HostConfig"]["LogConfig"] =
                        json!({"Type":"syslog","Config":{"syslog-address":"tcp://10.0.0.1:514"}})
                },
                "\"LogConfig\"",
            ),
            (
                "sysctls",
                |b| b["HostConfig"]["Sysctls"] = json!({"net.ipv4.ip_forward":"1"}),
                "\"Sysctls\"",
            ),
            (
                "a tmpfs",
                |b| b["HostConfig"]["Tmpfs"] = json!({"/x":""}),
                "\"Tmpfs\"",
            ),
            (
                "extra hosts",
                |b| b["HostConfig"]["ExtraHosts"] = json!(["wheeld:10.0.0.1"]),
                "\"ExtraHosts\"",
            ),
            (
                "dns",
                |b| b["HostConfig"]["Dns"] = json!(["10.0.0.1"]),
                "\"Dns\"",
            ),
            (
                "a healthcheck",
                |b| b["Healthcheck"] = json!({"Test":["NONE"]}),
                "\"Healthcheck\"",
            ),
            (
                "exposed ports",
                |b| b["ExposedPorts"] = json!({"7000/tcp":{}}),
                "\"ExposedPorts\"",
            ),
            (
                "volumes in the config",
                |b| b["Volumes"] = json!({"/host":{}}),
                "\"Volumes\"",
            ),
            (
                "no project label",
                |b| {
                    b["Labels"].as_object_mut().unwrap().remove("wheel.project");
                },
                "label",
            ),
            (
                "a spec label that is not a hash",
                |b| b["Labels"]["wheel.spec"] = json!("latest"),
                "spec",
            ),
            ("another image", |b| b["Image"] = json!("alpine"), "image"),
            ("a command", |b| b["Cmd"] = json!(["sh"]), "\"Cmd\""),
            (
                "an entrypoint",
                |b| b["Entrypoint"] = json!(["sh"]),
                "\"Entrypoint\"",
            ),
            ("a user", |b| b["User"] = json!("root"), "\"User\""),
            (
                "a working directory",
                |b| b["WorkingDir"] = json!("/"),
                "\"WorkingDir\"",
            ),
            (
                "the host role",
                |b| {
                    let env = b["Env"].as_array_mut().unwrap();
                    env.retain(|e| !e.as_str().unwrap().starts_with("WHEEL_ROLE="));
                    env.push(json!("WHEEL_ROLE=host"));
                },
                "WHEEL_ROLE",
            ),
            (
                "another project's id in the environment",
                |b| {
                    let env = b["Env"].as_array_mut().unwrap();
                    env.retain(|e| !e.as_str().unwrap().starts_with("WHEEL_PROJECT_ID="));
                    env.push(json!(format!("WHEEL_PROJECT_ID={}", Uuid::nil())));
                },
                "WHEEL_PROJECT_ID",
            ),
            (
                "an injected variable",
                |b| {
                    b["Env"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!("LD_PRELOAD=/x.so"))
                },
                "LD_PRELOAD",
            ),
            (
                "a repeated variable",
                |b| {
                    b["Env"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!("WHEEL_LOG=json"))
                },
                "repeats",
            ),
            (
                "a missing variable",
                |b| {
                    b["Env"].as_array_mut().unwrap().pop();
                },
                "missing",
            ),
            (
                "a foreign label",
                |b| b["Labels"]["evil"] = json!("1"),
                "\"evil\"",
            ),
            (
                "another project's label",
                |b| b["Labels"]["wheel.project"] = json!(Uuid::nil().to_string()),
                "label",
            ),
        ];
        for (what, edit, reason) in cases {
            let mut body = golden();
            edit(&mut body);
            let why = refused(create(&policy(), &body));
            assert!(
                why.contains(reason),
                "{what}: refused for {why:?}, expected it to mention {reason:?}"
            );
        }
    }

    #[test]
    fn a_create_must_be_named_for_the_project_it_carries() {
        let p = policy();
        let body = golden().to_string();
        for target in [
            "/containers/create".to_string(),
            format!("/containers/create?name=wheel-p-{}", Uuid::nil()),
            "/containers/create?name=evil".to_string(),
            format!("/containers/create?name=wheel-p-{}&platform=x", id()),
            format!("/containers/create?name=WHEEL-P-{}", id()),
        ] {
            assert!(
                p.decide("POST", &target, body.as_bytes()).is_err(),
                "{target}"
            );
        }
    }

    #[test]
    fn a_volume_is_only_ever_a_plain_local_one_for_a_project() {
        let p = policy();
        let name = volume_name(&id());
        let ok = json!({"Name": name, "Labels": {"wheel.project": id().to_string()}});
        assert!(p
            .decide("POST", "/volumes/create", ok.to_string().as_bytes())
            .is_ok());

        // Every row below carries valid labels, so it can only be refused for the defect it names —
        // a row that also lacked them would keep passing after its own guard was removed.
        let labels = json!({"wheel.project": id().to_string()});
        for (what, body) in [
            (
                "a bind mount through the volume API",
                json!({"Name": name, "Labels": labels, "DriverOpts": {"type":"none","o":"bind","device":"/"}}),
            ),
            (
                "the same with the driver spelled out",
                json!({"Name": name, "Labels": labels, "Driver": "local", "DriverOpts": {"type":"none","o":"bind","device":"/"}}),
            ),
            (
                "a cluster volume",
                json!({"Name": name, "Labels": labels, "ClusterVolumeSpec": {}}),
            ),
            (
                "another driver",
                json!({"Name": name, "Labels": labels, "Driver": "nfs"}),
            ),
            ("no labels at all", json!({"Name": name})),
            (
                "another project's name",
                json!({"Name": volume_name(&Uuid::nil()), "Labels": {"wheel.project": id().to_string()}}),
            ),
            ("a bare name", json!({"Name": "data"})),
            ("no name", json!({})),
        ] {
            assert!(
                p.decide("POST", "/volumes/create", body.to_string().as_bytes())
                    .is_err(),
                "{what} was admitted"
            );
        }
    }

    #[test]
    fn the_lifecycle_calls_are_admitted_on_project_names_only() {
        let p = policy();
        let c = container_name(&id());
        let v = volume_name(&id());
        for (m, t) in [
            ("GET", format!("/v1.49/containers/{c}/json")),
            ("GET", format!("/containers/{c}/json?size=false")),
            ("POST", format!("/containers/{c}/start")),
            ("POST", format!("/containers/{c}/stop?t=30")),
            (
                "DELETE",
                format!("/containers/{c}?force=true&v=false&link=false"),
            ),
            ("DELETE", format!("/volumes/{v}?force=true")),
        ] {
            assert!(p.decide(m, &t, b"").is_ok(), "{m} {t}");
        }
        assert_eq!(
            p.decide("GET", &format!("/containers/{c}/json"), b"")
                .unwrap()
                .reply,
            Reply::Inspect,
            "inspection must be reduced to the fields the host reads"
        );
    }

    #[test]
    fn everything_else_the_daemon_can_do_is_refused() {
        let p = policy();
        let c = container_name(&id());
        for (m, t) in [
            ("GET", "/containers/json".to_string()),
            ("GET", "/version".to_string()),
            ("GET", "/_ping".to_string()),
            ("GET", "/info".to_string()),
            ("GET", "/images/json".to_string()),
            ("POST", "/build".to_string()),
            ("POST", "/images/create?fromImage=alpine".to_string()),
            ("POST", format!("/containers/{c}/exec")),
            ("POST", format!("/containers/{c}/attach")),
            ("GET", format!("/containers/{c}/archive?path=/data")),
            ("PUT", format!("/containers/{c}/archive?path=/")),
            ("GET", format!("/containers/{c}/logs")),
            ("POST", format!("/containers/{c}/kill")),
            ("POST", format!("/containers/{c}/update")),
            ("POST", format!("/containers/{c}/rename?name=x")),
            ("DELETE", format!("/containers/{c}?link=true")),
            ("POST", format!("/containers/{c}/stop?signal=SIGKILL")),
            ("GET", format!("/containers/{c}/json?size=true")),
            ("GET", "/containers/deadbeef/json".to_string()),
            ("GET", "/containers/wheel-host/json".to_string()),
            ("GET", format!("/containers/{}/json", c.to_uppercase())),
            ("DELETE", format!("/volumes/{c}")),
            ("DELETE", "/volumes/wheel-p-x-data".to_string()),
            ("POST", "/volumes/prune".to_string()),
            ("POST", "/networks/create".to_string()),
            ("POST", "/swarm/init".to_string()),
            ("GET", "/events".to_string()),
            // Two readings of one path.
            ("GET", format!("/containers/%77heel-p-{}/json", id())),
            ("GET", format!("/containers/{c}/../{c}/json")),
            ("GET", format!("//containers/{c}/json")),
            ("GET", format!("/containers/{c}/json/")),
            ("GET", format!("/containers\\{c}/json")),
            ("HEAD", format!("/containers/{c}/json")),
            ("post", format!("/containers/{c}/start")),
        ] {
            assert!(p.decide(m, &t, b"").is_err(), "{m} {t} was admitted");
        }
    }

    #[test]
    fn a_refusal_never_quotes_a_secret_from_the_body() {
        let mut body = golden();
        body["HostConfig"]["Privileged"] = json!(true);
        body["Env"]
            .as_array_mut()
            .unwrap()
            .push(json!("EVIL=hunter2-secret"));
        let why = refused(create(&policy(), &body));
        assert!(
            !why.contains("hunter2") && !why.contains("engine-secret"),
            "{why}"
        );
    }

    /// Go's JSON decoder matches keys case-insensitively (with special folds) and ignores unknown
    /// ones; serde does neither. A proxy that checked with one and forwarded to the other could be
    /// shown a key it calls unknown that the daemon applies. Every variant is therefore REFUSED, not
    /// ignored.
    #[test]
    fn case_variant_and_unicode_folded_keys_are_refused_not_ignored() {
        type Edit = fn(&mut Value);
        let cases: [(&str, Edit); 6] = [
            ("lower-case privileged", |b| {
                b["HostConfig"]["privileged"] = json!(true)
            }),
            ("lower-case hostconfig", |b| {
                b["hostconfig"] = json!({"Privileged": true})
            }),
            ("upper-case image", |b| b["IMAGE"] = json!("alpine")),
            ("a long-s fold of SecurityOpt", |b| {
                b["HostConfig"]["\u{17f}ecurityOpt"] = json!([])
            }),
            ("a kelvin-sign fold", |b| {
                b["HostConfig"]["Binds\u{212a}"] = json!(["/:/x"])
            }),
            ("an escaped spelling of a denied key", |b| {
                // serde decodes the escape before comparing, so this is `Privileged` and is refused.
                *b = serde_json::from_str(&golden().to_string().replace(
                    "\"HostConfig\":{",
                    "\"HostConfig\":{\"Priv\\u0069leged\":true,",
                ))
                .unwrap();
            }),
        ];
        for (what, edit) in cases {
            let mut body = golden();
            edit(&mut body);
            refused(create(&policy(), &body));
            let _ = what;
        }
    }

    /// The daemon must never receive the caller's bytes: what is sent is built from the checked
    /// values, so odd whitespace, key order and duplicate keys cannot reach it.
    #[test]
    fn what_is_forwarded_is_rebuilt_from_checked_values_not_the_callers_bytes() {
        let p = policy();
        let g = golden().to_string();
        // A duplicate key, extra whitespace, and a reordered document.
        let messy = g
            .replacen("{\"", "{  \"Image\":\"evil\",\n\"", 1)
            .replace(",\"HostConfig\"", " ,  \"HostConfig\"");
        let target = format!("/containers/create?name=wheel-p-{}", id());
        let sent = p
            .decide("POST", &target, messy.as_bytes())
            .unwrap()
            .body
            .unwrap();
        // Bytes, not parsed values: a parser hides the very things a forwarded copy would carry.
        let text = String::from_utf8(sent.clone()).unwrap();
        assert_eq!(
            text.matches("\"Image\"").count(),
            1,
            "a duplicate key reached the daemon: {text}"
        );
        assert!(
            !text.contains("evil") && !text.contains("  ") && !text.contains('\n'),
            "{text}"
        );
        let sent: Value = serde_json::from_slice(&sent).unwrap();
        assert_eq!(
            sent["Image"], "wheel-engine:test",
            "the duplicate key was not collapsed"
        );
        assert_eq!(sent, {
            let mut expected = golden();
            expected["Env"] = sent["Env"].clone();
            expected
        });
        // Canonical key order for the environment, whatever order arrived.
        let env: Vec<&str> = sent["Env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        assert!(env[0].starts_with("WHEEL_PROJECT_ID=") && env[7].starts_with("WHEEL_ROLE="));
    }

    #[test]
    fn trailing_bytes_after_the_object_are_refused() {
        let p = policy();
        let target = format!("/containers/create?name=wheel-p-{}", id());
        let body = format!("{}{}", golden(), r#"{"HostConfig":{"Privileged":true}}"#);
        assert!(p.decide("POST", &target, body.as_bytes()).is_err());
    }

    #[test]
    fn calls_that_take_no_body_refuse_one() {
        let p = policy();
        let c = container_name(&id());
        assert!(p
            .decide("POST", &format!("/containers/{c}/start"), b"{}")
            .is_err());
        assert!(p
            .decide("GET", &format!("/containers/{c}/json"), b"x")
            .is_err());
    }

    #[test]
    fn the_target_sent_on_is_rebuilt_from_parsed_components() {
        let p = policy();
        let c = container_name(&id());
        let a = p
            .decide("POST", &format!("/v1.49/containers/{c}/stop?t=30"), b"")
            .unwrap();
        assert_eq!(a.target, format!("/v1.49/containers/{c}/stop?t=30"));
        let a = p
            .decide(
                "DELETE",
                &format!("/containers/{c}?force=true&v=true&link=false"),
                b"",
            )
            .unwrap();
        assert_eq!(
            a.target,
            format!("/containers/{c}?force=true&v=true&link=false")
        );
    }

    #[test]
    fn only_the_project_container_list_is_admitted() {
        let p = policy();
        let ok =
            "/v1.49/containers/json?all=true&filters=%7B%22label%22%3A%5B%22wheel.project%22%5D%7D";
        // What bollard really sends for a list: `size=false` rides along with `all`.
        let real = "/v1.49/containers/json?all=true&size=false&filters=%7B%22label%22%3A%5B%22wheel.project%22%5D%7D";
        assert_eq!(p.decide("GET", real, b"").unwrap().reply, Reply::List);
        let a = p.decide("GET", ok, b"").unwrap();
        assert_eq!(a.reply, Reply::List);
        for bad in [
            "/containers/json",
            "/containers/json?all=true",
            "/containers/json?all=true&size=true&filters=%7B%22label%22%3A%5B%22wheel.project%22%5D%7D",
            "/containers/json?all=true&filters=%7B%22label%22%3A%5B%22traefik.enable%22%5D%7D",
            "/containers/json?all=true&filters=%7B%7D",
            "/containers/json?size=true&all=true&filters=%7B%22label%22%3A%5B%22wheel.project%22%5D%7D",
        ] {
            assert!(p.decide("GET", bad, b"").is_err(), "{bad}");
        }
    }

    #[test]
    fn only_the_project_volume_list_is_admitted() {
        let p = policy();
        let ok = "/v1.49/volumes?filters=%7B%22label%22%3A%5B%22wheel.project%22%5D%7D";
        assert_eq!(p.decide("GET", ok, b"").unwrap().reply, Reply::VolumeList);
        for bad in [
            "/volumes",
            "/volumes?filters=%7B%22label%22%3A%5B%22traefik.enable%22%5D%7D",
            "/volumes?filters=%7B%7D",
            "/volumes?dangling=true&filters=%7B%22label%22%3A%5B%22wheel.project%22%5D%7D",
        ] {
            assert!(p.decide("GET", bad, b"").is_err(), "{bad}");
        }
    }

    #[test]
    fn a_value_that_merely_contains_the_text_filters_equals_is_not_the_filters_param() {
        // The anchoring (start-of-string or after `&`) matters: a value ending in the literal
        // text "filters=..." must not be mistaken for the real, well-formed parameter.
        let p = policy();
        let sneaky = format!("/containers/json?all=true&evil=x%26filters={LIST_FILTERS_ENCODED}");
        assert!(p.decide("GET", &sneaky, b"").is_err(), "{sneaky}");
    }

    #[test]
    fn a_volume_list_is_reduced_to_name_and_the_project_label() {
        let raw = br#"{"Volumes":[
            {"Name":"wheel-p-3f2504e0-4f89-11d3-9a0c-0305e82c3301-data","Labels":{"wheel.project":"3f2504e0-4f89-11d3-9a0c-0305e82c3301","evil":"1"},"Mountpoint":"/var/lib/docker/volumes/x/_data"},
            {"Name":"some-other-volume","Labels":{}}
        ],"Warnings":[]}"#;
        let v: Value = serde_json::from_slice(&project_volume_list(raw).unwrap()).unwrap();
        // {"Volumes": [...]}, not a bare array — matches Docker's own real `GET /volumes` shape,
        // which the real bollard client reading this back through the proxy depends on.
        let out = v["Volumes"].as_array().unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(
            out[0]["Name"],
            "wheel-p-3f2504e0-4f89-11d3-9a0c-0305e82c3301-data"
        );
        assert_eq!(
            out[0]["Labels"]["wheel.project"],
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301"
        );
        assert!(out[0]["Labels"].get("evil").is_none(), "{out:?}");
    }

    #[test]
    fn the_volume_list_admission_is_fixed_and_internal() {
        let a = volume_list_admission();
        assert_eq!(a.method, "GET");
        assert!(a.target.starts_with("/volumes?filters="));
        assert_eq!(a.reply, Reply::VolumeList);
    }

    #[test]
    fn an_inspection_is_reduced_to_state_and_the_two_wheel_labels() {
        let raw = br#"{"Id":"x","State":{"Status":"running","Health":{"Status":"unhealthy","Log":[{"Output":"secret"}]}},
            "Config":{"Env":["WHEEL_ENGINE_SECRET=s3cret"],"Cmd":["x"],"Labels":{"wheel.spec":"abc","traefik.x":"y","wheel.project":"p"}},
            "HostConfig":{"Binds":["/:/h"]},"NetworkSettings":{"IPAddress":"10.0.0.2"},"Mounts":[{"Source":"/var/lib/docker"}]}"#;
        let out = String::from_utf8(project_inspect(raw).unwrap()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["State"]["Status"], "running");
        assert_eq!(v["State"]["Health"]["Status"], "unhealthy");
        assert_eq!(v["Config"]["Labels"]["wheel.spec"], "abc");
        for leak in [
            "s3cret",
            "traefik",
            "10.0.0.2",
            "/var/lib/docker",
            "Cmd",
            "Output",
        ] {
            assert!(!out.contains(leak), "{leak} leaked: {out}");
        }
        assert!(project_inspect(b"not json").is_none());
    }

    #[test]
    fn a_list_keeps_only_project_containers_and_only_their_wheel_labels() {
        let raw = br#"[
          {"Names":["/wheel-p-3f2504e0-4f89-11d3-9a0c-0305e82c3301"],"State":"running","Labels":{"wheel.project":"3f2504e0-4f89-11d3-9a0c-0305e82c3301","evil":"1"},"HostConfig":{"NetworkMode":"x"}},
          {"Names":["/caddy"],"State":"running","Labels":{}},
          {"Names":["/wheel-host"],"State":"running","Labels":{}}
        ]"#;
        let v: Value = serde_json::from_slice(&project_list(raw).unwrap()).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1, "{v}");
        assert!(
            v[0]["Labels"].get("evil").is_none() && v[0].get("HostConfig").is_none(),
            "{v}"
        );
    }

    #[test]
    fn daemon_answers_are_reduced_and_errors_are_fixed_strings() {
        let c: Value = serde_json::from_slice(
            &project_create(br#"{"Id":"deadbeef","Warnings":["your env FOO=bar"]}"#).unwrap(),
        )
        .unwrap();
        assert_eq!(c, json!({"Id":"deadbeef","Warnings":[]}));
        let v = String::from_utf8(
            project_volume(
                br#"{"Name":"n","Driver":"local","Mountpoint":"/var/lib/docker/volumes/n/_data"}"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert!(!v.contains("/var/lib/docker"), "{v}");
        for status in [400u16, 403, 409, 500] {
            assert!(!fixed_error(status).contains("wheel-p-"), "{status}");
        }
    }

    #[test]
    fn a_body_that_is_not_an_object_is_refused() {
        let p = policy();
        let t = format!("/containers/create?name=wheel-p-{}", id());
        for b in [&b"[]"[..], b"null", b"\"x\"", b"", b"{"] {
            assert!(p.decide("POST", &t, b).is_err());
        }
    }

    // --------------------------------------------------------------------- run root (M3a)

    fn run_root_policy() -> Policy {
        Policy {
            run_root: Some(RunRoot::new("/run/wheel-docker").unwrap()),
            ..policy()
        }
    }

    fn golden_with_run_root() -> Value {
        let mut b = golden();
        b["Env"] = json!(b["Env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                let e = e.as_str().unwrap();
                if e.starts_with("WHEEL_LISTEN=") {
                    format!("WHEEL_LISTEN={ENGINE_SOCKET}")
                } else {
                    e.to_string()
                }
            })
            .collect::<Vec<_>>());
        b["HostConfig"]["Binds"] = json!([
            format!("wheel-p-{}-data:/data", id()),
            format!("/run/wheel-docker/{}:/run/wheel", id()),
        ]);
        b
    }

    #[test]
    fn a_run_root_path_must_be_canonical_and_absolute() {
        assert!(RunRoot::new("/run/wheel-docker").is_ok());
        assert!(RunRoot::new("/a").is_ok());
        for bad in [
            "",
            "/",
            "relative/path",
            "/has/trailing/",
            "/has//double",
            "/has/../dotdot",
            "/has/./dot",
            "/has:colon",
            "/has,comma",
            "/has space",
            "/has	tab",
            "/has
newline",
        ] {
            assert!(RunRoot::new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn with_a_run_root_the_golden_create_needs_the_socket_bind_and_a_unix_listen() {
        let p = run_root_policy();
        let admitted = create(&p, &golden_with_run_root());
        assert!(admitted.is_ok(), "{admitted:?}");
    }

    #[test]
    fn without_a_run_root_the_socket_bind_is_refused() {
        // The right WHEEL_LISTEN, but the Binds list is missing the socket mount (the plain,
        // TCP-channel Binds) -- refused for Binds specifically, not silently admitted without it.
        let mut b = golden_with_run_root();
        b["HostConfig"]["Binds"] = json!([format!("wheel-p-{}-data:/data", id())]);
        let why = refused(create(&run_root_policy(), &b));
        assert!(why.contains("Binds"), "{why}");
    }

    #[test]
    fn a_run_root_policy_refuses_the_plain_tcp_golden_create_too() {
        // Belt and braces: the unmodified TCP-channel golden create (wrong WHEEL_LISTEN AND
        // missing the socket bind) must not be admitted just because some other check fires first.
        assert!(create(&run_root_policy(), &golden()).is_err());
    }

    #[test]
    fn a_run_root_bind_is_refused_every_way_but_the_one_exact_form() {
        let p = run_root_policy();
        let other_project = Uuid::new_v4();
        let cases: [(&str, &str); 6] = [
            (
                "another project's socket dir",
                "/run/wheel-docker/00000000-0000-0000-0000-000000000000:/run/wheel",
            ),
            (
                "a different run root",
                "/somewhere/else/3f2504e0-4f89-11d3-9a0c-0305e82c3301:/run/wheel",
            ),
            (
                "the wrong target",
                "/run/wheel-docker/3f2504e0-4f89-11d3-9a0c-0305e82c3301:/somewhere",
            ),
            (
                "read-only",
                "/run/wheel-docker/3f2504e0-4f89-11d3-9a0c-0305e82c3301:/run/wheel:ro",
            ),
            (
                "the run root itself, no per-project subdir",
                "/run/wheel-docker:/run/wheel",
            ),
            ("the host root", "/:/run/wheel"),
        ];
        let _ = other_project;
        for (what, extra) in cases {
            let mut b = golden_with_run_root();
            b["HostConfig"]["Binds"] = json!([format!("wheel-p-{}-data:/data", id()), extra]);
            let why = refused(create(&p, &b));
            assert!(why.contains("Binds"), "{what}: {why}");
        }
        // A third bind, or the socket bind alone without the data volume, is refused too.
        let mut extra_bind = golden_with_run_root();
        let mut binds = extra_bind["HostConfig"]["Binds"]
            .as_array()
            .unwrap()
            .clone();
        binds.push(json!("/etc:/etc"));
        extra_bind["HostConfig"]["Binds"] = json!(binds);
        assert!(refused(create(&p, &extra_bind)).contains("Binds"));

        let mut socket_only = golden_with_run_root();
        socket_only["HostConfig"]["Binds"] =
            json!([format!("/run/wheel-docker/{}:/run/wheel", id())]);
        assert!(refused(create(&p, &socket_only)).contains("Binds"));
    }

    #[test]
    fn the_ceiling_admission_is_fixed_and_internal() {
        let a = list_admission();
        assert_eq!(a.method, "GET");
        assert!(a.target.contains("wheel.project"));
        assert_eq!(a.reply, Reply::List);
    }
}
