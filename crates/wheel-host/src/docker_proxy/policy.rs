// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! What the sandbox host may ask the docker daemon for. **Default DENY.**
//!
//! Access to the docker socket is root on the machine: a `create` with `Privileged`, or with a bind
//! of `/`, is a container escape in one request. The host is the only thing that holds it, and the
//! host proxies tenant traffic, so "the host has a bug" is the risk this closes. The host makes
//! exactly seven calls (`DockerSandbox`), all on names derived from a uuid the API generated, so
//! the allowlist is short enough to state in full and to refuse everything else.
//!
//! Two rules make it hold:
//!
//! * Bodies are checked against an ALLOWLIST OF KEYS, not a denylist of dangerous ones. Docker's
//!   create body grows with every release; "not on the list" is the only fail-closed shape.
//! * Every name, and every id that appears in a body, must agree with the id in the URL, so a
//!   request cannot be admitted for one project and act on another.
//!
//! Pure: no I/O, no clock. The proxy in `super` owns the socket.

use serde_json::Value;
use uuid::Uuid;

const PROJECT_LABEL: &str = "wheel.project";

/// What the operator configured; the proxy admits exactly this and nothing else.
#[derive(Debug, Clone)]
pub struct Policy {
    /// The one image a tenant container may run.
    pub image: String,
    /// The one network a tenant container may join.
    pub network: String,
    /// Ceilings, not exact values: the host's own settings must fit under them.
    pub max_memory: i64,
    pub max_nano_cpus: i64,
    pub max_pids: i64,
    pub engine_port: u16,
}

/// What to do to a response before it leaves the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admitted {
    /// Forward as is.
    Plain,
    /// Container inspection: the environment holds the engine secret and vault key, and the host
    /// only ever reads `State` from it, so the environment is removed.
    StripEnv,
}

/// Why a request was refused. Safe to log and to return: it never quotes a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied(pub String);

fn deny<T>(why: impl Into<String>) -> Result<T, Denied> {
    Err(Denied(why.into()))
}

type Verdict = Result<Admitted, Denied>;

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

/// Path segments after an optional `/v1.NN` API-version prefix. Refuses anything that could be read
/// two ways: percent-encoding, backslashes, empty or dot segments, non-ASCII.
fn segments(path: &str) -> Result<Vec<&str>, Denied> {
    if !path.starts_with('/')
        || !path.is_ascii()
        || path.contains('%')
        || path.contains('\\')
        || path.contains("//")
    {
        return deny("path is not in canonical form");
    }
    let mut segs: Vec<&str> = path[1..].split('/').collect();
    if segs.first().is_some_and(|s| {
        s.strip_prefix('v')
            .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.'))
    }) {
        segs.remove(0);
    }
    if segs.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return deny("path is not in canonical form");
    }
    Ok(segs)
}

/// `k=v&k=v`, refusing encoding, repeats and valueless keys.
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

impl Policy {
    /// Decide one request. `target` is the request target as received (path and optional query).
    pub fn decide(&self, method: &str, target: &str, body: &[u8]) -> Verdict {
        let (path, q) = match target.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (target, None),
        };
        let segs = segments(path)?;
        let q = query(q)?;
        let bool_word = |v: &str| matches!(v, "true" | "false");

        match (method, segs.as_slice()) {
            ("GET", ["containers", name, "json"]) => {
                container_id(name)?;
                only_params(&q, &["size"], |_, v| matches!(v, "false" | "0"))?;
                Ok(Admitted::StripEnv)
            }
            ("POST", ["containers", "create"]) => {
                // bollard also sends an empty `platform=`; a non-empty one would pick an image
                // variant, which the pinned image already decides.
                only_params(&q, &["name", "platform"], |k, v| {
                    k == "name" || v.is_empty()
                })?;
                let Some((_, name)) = q.iter().find(|(k, _)| *k == "name") else {
                    return deny("a container must be created with a name");
                };
                let id = container_id(name)?;
                self.container_body(&id, body)?;
                Ok(Admitted::Plain)
            }
            ("POST", ["containers", name, "start"]) => {
                container_id(name)?;
                only_params(&q, &[], |_, _| false)?;
                Ok(Admitted::Plain)
            }
            ("POST", ["containers", name, "stop"]) => {
                container_id(name)?;
                only_params(&q, &["t"], |_, v| {
                    !v.is_empty() && v.len() <= 4 && v.chars().all(|c| c.is_ascii_digit())
                })?;
                Ok(Admitted::Plain)
            }
            ("DELETE", ["containers", name]) => {
                container_id(name)?;
                // `v=true` would also remove anonymous volumes and `link=true` removes a link between
                // containers, so both are admitted only as `false` (which is what bollard sends).
                only_params(&q, &["force", "v", "link"], |k, v| match k {
                    "force" => bool_word(v),
                    _ => v == "false",
                })?;
                Ok(Admitted::Plain)
            }
            ("POST", ["volumes", "create"]) => {
                only_params(&q, &[], |_, _| false)?;
                self.volume_body(body)?;
                Ok(Admitted::Plain)
            }
            ("DELETE", ["volumes", name]) => {
                volume_id(name)?;
                only_params(&q, &["force"], |_, v| bool_word(v))?;
                Ok(Admitted::Plain)
            }
            _ => deny("this docker API call is not one the sandbox host makes"),
        }
    }

    fn volume_body(&self, body: &[u8]) -> Result<(), Denied> {
        let v = json_object(body)?;
        let obj = v.as_object().expect("json_object returns an object");
        only_keys(obj, &["Name", "Labels", "Driver"], "volume")?;
        let id = volume_id(str_field(obj, "Name")?)?;
        // Not on the list, so a `local` driver with `type=none,o=bind,device=/` — a bind mount of
        // any host path through the volume API — cannot be spelled at all.
        if let Some(driver) = obj.get("Driver").filter(|d| !d.is_null()) {
            if driver.as_str() != Some("local") {
                return deny("volume driver must be local");
            }
        }
        labels_are_only_the_project(obj.get("Labels"), &id)
    }

    fn container_body(&self, id: &Uuid, body: &[u8]) -> Result<(), Denied> {
        let v = json_object(body)?;
        let obj = v.as_object().expect("json_object returns an object");
        only_keys(obj, &["Image", "Env", "Labels", "HostConfig"], "container")?;

        if str_field(obj, "Image")? != self.image {
            return deny("image is not the configured engine image");
        }
        labels_are_only_the_project(obj.get("Labels"), id)?;
        self.env(id, obj.get("Env"))?;
        self.host_config(id, obj.get("HostConfig"))
    }

    fn env(&self, id: &Uuid, env: Option<&Value>) -> Result<(), Denied> {
        let Some(list) = env.and_then(Value::as_array) else {
            return deny("Env is required");
        };
        let listen = format!("tcp://0.0.0.0:{}", self.engine_port);
        let mut seen: Vec<&str> = Vec::new();
        for entry in list {
            let Some((k, val)) = entry.as_str().and_then(|e| e.split_once('=')) else {
                return deny("Env entries must be KEY=VALUE strings");
            };
            if seen.contains(&k) {
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
            seen.push(k);
        }
        for required in [
            "WHEEL_PROJECT_ID",
            "WHEEL_ENGINE_SECRET",
            "WHEEL_VAULT_KEY",
            "WHEEL_HARNESS_AUTH",
            "WHEEL_LISTEN",
            "WHEEL_DATA_DIR",
            "WHEEL_LOG",
            "WHEEL_ROLE",
        ] {
            if !seen.contains(&required) {
                return deny(format!("Env is missing {required}"));
            }
        }
        Ok(())
    }

    fn host_config(&self, id: &Uuid, hc: Option<&Value>) -> Result<(), Denied> {
        let Some(hc) = hc.and_then(Value::as_object) else {
            return deny("HostConfig is required");
        };
        only_keys(
            hc,
            &[
                "CapDrop",
                "SecurityOpt",
                "Memory",
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
        let expected_bind = format!("{}:/data", volume_name(id));
        if strings("Binds")? != [expected_bind.as_str()] {
            return deny("HostConfig.Binds must be exactly this project's data volume");
        }
        if str_field(hc, "NetworkMode")? != self.network {
            return deny("HostConfig.NetworkMode is not the tenant network");
        }
        for (key, ceiling) in [
            ("Memory", self.max_memory),
            ("NanoCpus", self.max_nano_cpus),
            ("PidsLimit", self.max_pids),
        ] {
            match hc.get(key).and_then(Value::as_i64) {
                Some(n) if n > 0 && n <= ceiling => {}
                _ => {
                    return deny(format!(
                        "HostConfig.{key} must be set, positive and within the ceiling"
                    ))
                }
            }
        }
        let restart = hc.get("RestartPolicy").and_then(Value::as_object);
        match restart {
            Some(r) => {
                only_keys(r, &["Name"], "RestartPolicy")?;
                if r.get("Name").and_then(Value::as_str) != Some("unless-stopped") {
                    return deny("RestartPolicy must be unless-stopped");
                }
            }
            None => return deny("RestartPolicy is required"),
        }
        Ok(())
    }
}

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

fn labels_are_only_the_project(labels: Option<&Value>, id: &Uuid) -> Result<(), Denied> {
    let Some(labels) = labels.filter(|l| !l.is_null()) else {
        return Ok(());
    };
    let Some(map) = labels.as_object() else {
        return deny("Labels must be an object");
    };
    only_keys(map, &[PROJECT_LABEL], "Labels")?;
    match map.get(PROJECT_LABEL).map(|v| v.as_str()) {
        None => Ok(()),
        Some(Some(v)) if v == id.to_string() => Ok(()),
        _ => deny("the project label does not match the name"),
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
            max_memory: 1 << 30,
            max_nano_cpus: 2_000_000_000,
            max_pids: 512,
            engine_port: 7000,
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
        assert_eq!(create(&policy(), &golden()), Ok(Admitted::Plain));
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
        assert_eq!(
            p.decide("POST", "/volumes/create", ok.to_string().as_bytes()),
            Ok(Admitted::Plain)
        );

        for (what, body) in [
            (
                "a bind mount through the volume API",
                json!({"Name": name, "DriverOpts": {"type":"none","o":"bind","device":"/"}}),
            ),
            ("another driver", json!({"Name": name, "Driver": "nfs"})),
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
            p.decide("GET", &format!("/containers/{c}/json"), b""),
            Ok(Admitted::StripEnv),
            "inspection must have its environment removed"
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
            ("DELETE", format!("/containers/{c}?v=true")),
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

    #[test]
    fn a_body_that_is_not_an_object_is_refused() {
        let p = policy();
        let t = format!("/containers/create?name=wheel-p-{}", id());
        for b in [&b"[]"[..], b"null", b"\"x\"", b"", b"{"] {
            assert!(p.decide("POST", &t, b).is_err());
        }
    }
}
