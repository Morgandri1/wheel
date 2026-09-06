//! Turning somebody's API document into callable operations (§3d).
//!
//! Four formats, one normalized shape. The engine is the ONLY parser — Web
//! sends the raw document and renders what comes back, so a spec that imports
//! differently in the preview than in the node would be a bug nobody could see.
//!
//! These documents come from the wild, so parsing is deliberately forgiving:
//! anything unrecognised is skipped rather than fatal, because a spec with one
//! malformed operation out of forty should import thirty-nine, not nothing.
//! What is NOT forgiving is the output — every operation gets a unique,
//! charset-safe id, because that id becomes an MCP tool name.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use wheel_core::{Fill, ParamLocation, ToolFormat, ToolMethod, ToolOperation, ToolParam};

/// What an import produced, before it becomes a node.
#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    pub format: ToolFormat,
    /// Empty when the document did not say; the user must supply one.
    pub base_url: String,
    pub operations: Vec<ToolOperation>,
}

/// Parse a document, detecting the format when the caller did not say.
pub fn import(raw: &str, format: Option<ToolFormat>) -> Result<Imported> {
    let doc = parse_document(raw)?;
    let format = match format {
        Some(f) => f,
        None => detect(&doc).context(
            "could not tell what kind of document this is; \
             expected OpenAPI 3, Swagger 2, a Postman collection or an Insomnia export",
        )?,
    };
    let (base_url, mut operations) = match format {
        ToolFormat::Openapi => openapi3(&doc),
        ToolFormat::Swagger2 => swagger2(&doc),
        ToolFormat::Postman => postman(&doc),
        ToolFormat::Insomnia => insomnia(&doc),
        ToolFormat::Manual => (String::new(), Vec::new()),
    };
    if operations.is_empty() {
        bail!("no operations found in this document");
    }
    dedupe_ids(&mut operations);
    Ok(Imported {
        format,
        base_url,
        operations,
    })
}

/// JSON or YAML. Real OpenAPI documents are usually YAML, and refusing them
/// would exclude most of the specs anyone actually has.
fn parse_document(raw: &str) -> Result<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(raw) {
        return Ok(v);
    }
    serde_yaml::from_str::<Value>(raw).context("this is neither valid JSON nor valid YAML")
}

fn detect(doc: &Value) -> Result<ToolFormat> {
    if doc.get("openapi").and_then(Value::as_str).is_some() {
        return Ok(ToolFormat::Openapi);
    }
    if doc.get("swagger").and_then(Value::as_str).is_some() {
        return Ok(ToolFormat::Swagger2);
    }
    // Postman keeps its marker inside `info`, and the schema URL is the only
    // thing guaranteed to be there across exports.
    if doc.pointer("/info/_postman_id").is_some()
        || doc
            .pointer("/info/schema")
            .and_then(Value::as_str)
            .is_some_and(|s| s.contains("getpostman.com"))
    {
        return Ok(ToolFormat::Postman);
    }
    if doc.get("__export_format").is_some()
        || doc.get("_type").and_then(Value::as_str) == Some("export")
    {
        return Ok(ToolFormat::Insomnia);
    }
    bail!("unrecognised document")
}

// --- OpenAPI 3 --------------------------------------------------------------

fn openapi3(doc: &Value) -> (String, Vec<ToolOperation>) {
    let base = doc
        .pointer("/servers/0/url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string();

    let mut ops = Vec::new();
    let Some(paths) = doc.get("paths").and_then(Value::as_object) else {
        return (base, ops);
    };
    for (path, item) in paths {
        // Parameters declared on the path apply to every method under it.
        let shared = params_from(item.get("parameters"), doc);
        let Some(methods) = item.as_object() else {
            continue;
        };
        for (verb, op) in methods {
            let Some(method) = method_of(verb) else {
                continue;
            };
            let mut params = shared.clone();
            params.extend(params_from(op.get("parameters"), doc));
            params.extend(openapi_body(op.get("requestBody"), doc));

            ops.push(ToolOperation {
                id: slug(op.get("operationId").and_then(Value::as_str), method, path),
                method,
                path: normalise_path(path),
                summary: str_of(op, "summary").or_else(|| str_of(op, "description")),
                enabled: true,
                params,
            });
        }
    }
    ops.sort_by(|a, b| {
        (a.path.clone(), a.method.as_str()).cmp(&(b.path.clone(), b.method.as_str()))
    });
    (base, ops)
}

fn params_from(list: Option<&Value>, doc: &Value) -> Vec<ToolParam> {
    let Some(items) = list.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|p| {
            let p = resolve_ref(p, doc);
            let name = p.get("name").and_then(Value::as_str)?.to_string();
            let location = match p.get("in").and_then(Value::as_str)? {
                "path" => ParamLocation::Path,
                "query" => ParamLocation::Query,
                "header" => ParamLocation::Header,
                "cookie" => ParamLocation::Cookie,
                // Swagger 2 body/formData are handled by their own parser.
                _ => return None,
            };
            Some(ToolParam {
                name,
                location,
                required: p.get("required").and_then(Value::as_bool).unwrap_or(false),
                description: str_of(&p, "description"),
                schema: p.get("schema").cloned(),
                fill: Fill::agent(),
            })
        })
        .collect()
}

/// Top-level properties of a JSON request body become body params.
///
/// Nested objects are kept whole rather than flattened: an agent filling
/// `address` with an object is clearer than filling `address.street`,
/// `address.city` as separate strings, and it keeps the schema honest.
fn openapi_body(body: Option<&Value>, doc: &Value) -> Vec<ToolParam> {
    let Some(body) = body else {
        return Vec::new();
    };
    let body = resolve_ref(body, doc);
    let required_body = body
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(content) = body.get("content").and_then(Value::as_object) else {
        return Vec::new();
    };
    // Prefer JSON; otherwise whatever the document offers first.
    let schema = content
        .get("application/json")
        .or_else(|| content.values().next())
        .and_then(|m| m.get("schema"))
        .map(|s| resolve_ref(s, doc));
    let Some(schema) = schema else {
        return Vec::new();
    };
    body_params_from_schema(&schema, required_body, doc)
}

fn body_params_from_schema(schema: &Value, body_required: bool, doc: &Value) -> Vec<ToolParam> {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    match schema.get("properties").and_then(Value::as_object) {
        Some(props) => props
            .iter()
            .map(|(name, sub)| {
                let sub = resolve_ref(sub, doc);
                ToolParam {
                    name: name.clone(),
                    location: ParamLocation::Body,
                    required: body_required && required.contains(&name.as_str()),
                    description: str_of(&sub, "description"),
                    schema: Some(sub),
                    fill: Fill::agent(),
                }
            })
            .collect(),
        // A body that is not an object (an array, a string) is one field.
        None => vec![ToolParam {
            name: "body".into(),
            location: ParamLocation::Body,
            required: body_required,
            description: str_of(schema, "description"),
            schema: Some(schema.clone()),
            fill: Fill::agent(),
        }],
    }
}

/// Follow a local `$ref` one hop at a time.
///
/// Bounded rather than recursive: a spec with a `$ref` cycle is a spec someone
/// will paste in one day, and hanging the engine on it is not an option.
fn resolve_ref(v: &Value, doc: &Value) -> Value {
    let mut cur = v.clone();
    for _ in 0..8 {
        let Some(r) = cur.get("$ref").and_then(Value::as_str) else {
            return cur;
        };
        let Some(target) = r.strip_prefix("#/") else {
            return cur;
        };
        let pointer = format!("/{}", target);
        match doc.pointer(&pointer) {
            Some(next) => cur = next.clone(),
            None => return cur,
        }
    }
    cur
}

// --- Swagger 2 --------------------------------------------------------------

fn swagger2(doc: &Value) -> (String, Vec<ToolOperation>) {
    let scheme = doc
        .pointer("/schemes/0")
        .and_then(Value::as_str)
        .unwrap_or("https");
    let host = doc.get("host").and_then(Value::as_str).unwrap_or_default();
    let base_path = doc
        .get("basePath")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_end_matches('/');
    let base = if host.is_empty() {
        String::new()
    } else {
        format!("{scheme}://{host}{base_path}")
    };

    let mut ops = Vec::new();
    let Some(paths) = doc.get("paths").and_then(Value::as_object) else {
        return (base, ops);
    };
    for (path, item) in paths {
        let shared = params_from(item.get("parameters"), doc);
        let Some(methods) = item.as_object() else {
            continue;
        };
        for (verb, op) in methods {
            let Some(method) = method_of(verb) else {
                continue;
            };
            let mut params = shared.clone();
            params.extend(params_from(op.get("parameters"), doc));
            params.extend(swagger_body(op.get("parameters"), doc));

            ops.push(ToolOperation {
                id: slug(op.get("operationId").and_then(Value::as_str), method, path),
                method,
                path: normalise_path(path),
                summary: str_of(op, "summary").or_else(|| str_of(op, "description")),
                enabled: true,
                params,
            });
        }
    }
    ops.sort_by(|a, b| {
        (a.path.clone(), a.method.as_str()).cmp(&(b.path.clone(), b.method.as_str()))
    });
    (base, ops)
}

/// Swagger 2 puts the body in the same `parameters` array, as `in: body` (one
/// schema) or `in: formData` (one field each).
fn swagger_body(list: Option<&Value>, doc: &Value) -> Vec<ToolParam> {
    let Some(items) = list.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for p in items {
        let p = resolve_ref(p, doc);
        match p.get("in").and_then(Value::as_str) {
            Some("body") => {
                let required = p.get("required").and_then(Value::as_bool).unwrap_or(false);
                if let Some(schema) = p.get("schema") {
                    let schema = resolve_ref(schema, doc);
                    out.extend(body_params_from_schema(&schema, required, doc));
                }
            }
            Some("formData") => {
                if let Some(name) = p.get("name").and_then(Value::as_str) {
                    out.push(ToolParam {
                        name: name.to_string(),
                        location: ParamLocation::Body,
                        required: p.get("required").and_then(Value::as_bool).unwrap_or(false),
                        description: str_of(&p, "description"),
                        schema: p.get("type").map(|t| serde_json::json!({ "type": t })),
                        fill: Fill::agent(),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

// --- Postman v2.1 -----------------------------------------------------------

fn postman(doc: &Value) -> (String, Vec<ToolOperation>) {
    let mut ops = Vec::new();
    let mut hosts: Vec<String> = Vec::new();
    if let Some(items) = doc.get("item").and_then(Value::as_array) {
        for item in items {
            postman_item(item, &mut ops, &mut hosts);
        }
    }
    // Collections have no single base_url; the commonest origin is the best
    // guess, and the user confirms it in the UI before anything is called.
    let base = hosts.first().cloned().unwrap_or_default();
    (base, ops)
}

/// Folders nest arbitrarily, so this walks rather than assuming one level.
fn postman_item(item: &Value, ops: &mut Vec<ToolOperation>, hosts: &mut Vec<String>) {
    if let Some(children) = item.get("item").and_then(Value::as_array) {
        for c in children {
            postman_item(c, ops, hosts);
        }
        return;
    }
    let Some(req) = item.get("request") else {
        return;
    };
    let Some(method) = req
        .get("method")
        .and_then(Value::as_str)
        .and_then(method_of)
    else {
        return;
    };
    let name = item.get("name").and_then(Value::as_str);
    let url = req.get("url");
    let (origin, raw_path) = postman_url(url);
    // Normalise BEFORE looking for placeholders: Postman writes `:id` and the
    // extractor looks for `{id}`, so deriving parameters from the raw path
    // silently found none and handed the agent an operation it could not call.
    let path = normalise_path(&raw_path);
    if !origin.is_empty() && !hosts.contains(&origin) {
        hosts.push(origin);
    }

    let mut params = Vec::new();
    for h in req
        .get("header")
        .and_then(Value::as_array)
        .unwrap_or(&vec![])
    {
        if h.get("disabled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(key) = h.get("key").and_then(Value::as_str) {
            params.push(ToolParam {
                name: key.to_string(),
                location: ParamLocation::Header,
                required: false,
                description: str_of(h, "description"),
                schema: Some(serde_json::json!({ "type": "string" })),
                fill: Fill::agent(),
            });
        }
    }
    if let Some(query) = url.and_then(|u| u.get("query")).and_then(Value::as_array) {
        for q in query {
            if q.get("disabled").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            if let Some(key) = q.get("key").and_then(Value::as_str) {
                params.push(ToolParam {
                    name: key.to_string(),
                    location: ParamLocation::Query,
                    required: false,
                    description: str_of(q, "description"),
                    schema: Some(serde_json::json!({ "type": "string" })),
                    fill: Fill::agent(),
                });
            }
        }
    }
    for name in path_variables(&path) {
        params.push(ToolParam {
            name,
            location: ParamLocation::Path,
            required: true,
            description: None,
            schema: Some(serde_json::json!({ "type": "string" })),
            fill: Fill::agent(),
        });
    }
    params.extend(postman_body(req.get("body")));

    ops.push(ToolOperation {
        id: slug(name, method, &path),
        method,
        path,
        summary: name.map(str::to_string),
        enabled: true,
        params,
    });
}

/// Postman stores a URL either as a string or as a structured object, and
/// exports in the wild contain both.
fn postman_url(url: Option<&Value>) -> (String, String) {
    let Some(url) = url else {
        return (String::new(), "/".into());
    };
    if let Some(raw) = url.as_str() {
        return split_origin(raw);
    }
    if let Some(raw) = url.get("raw").and_then(Value::as_str) {
        return split_origin(raw);
    }
    let host = url
        .get("host")
        .and_then(Value::as_array)
        .map(|h| {
            h.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(".")
        })
        .unwrap_or_default();
    let path = url
        .get("path")
        .and_then(Value::as_array)
        .map(|p| {
            p.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default();
    let origin = if host.is_empty() {
        String::new()
    } else {
        format!("https://{host}")
    };
    (origin, format!("/{path}"))
}

fn postman_body(body: Option<&Value>) -> Vec<ToolParam> {
    let Some(body) = body else {
        return Vec::new();
    };
    match body.get("mode").and_then(Value::as_str) {
        Some("raw") => {
            // A raw body is usually JSON with example values; its KEYS are the
            // fields an agent fills.
            let raw = body.get("raw").and_then(Value::as_str).unwrap_or_default();
            match serde_json::from_str::<Value>(raw) {
                Ok(Value::Object(map)) => map
                    .into_iter()
                    .map(|(name, example)| ToolParam {
                        name,
                        location: ParamLocation::Body,
                        required: false,
                        description: None,
                        schema: Some(serde_json::json!({ "type": json_type(&example) })),
                        fill: Fill::agent(),
                    })
                    .collect(),
                _ => vec![ToolParam {
                    name: "body".into(),
                    location: ParamLocation::Body,
                    required: false,
                    description: None,
                    schema: Some(serde_json::json!({ "type": "string" })),
                    fill: Fill::agent(),
                }],
            }
        }
        Some("urlencoded") | Some("formdata") => body
            .get(
                body.get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("urlencoded"),
            )
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|i| i.get("disabled").and_then(Value::as_bool) != Some(true))
                    .filter_map(|i| i.get("key").and_then(Value::as_str))
                    .map(|k| ToolParam {
                        name: k.to_string(),
                        location: ParamLocation::Body,
                        required: false,
                        description: None,
                        schema: Some(serde_json::json!({ "type": "string" })),
                        fill: Fill::agent(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

// --- Insomnia v4 ------------------------------------------------------------

fn insomnia(doc: &Value) -> (String, Vec<ToolOperation>) {
    let mut ops = Vec::new();
    let mut hosts: Vec<String> = Vec::new();
    let Some(resources) = doc.get("resources").and_then(Value::as_array) else {
        return (String::new(), ops);
    };
    for r in resources {
        if r.get("_type").and_then(Value::as_str) != Some("request") {
            continue;
        }
        let Some(method) = r.get("method").and_then(Value::as_str).and_then(method_of) else {
            continue;
        };
        let (origin, raw_path) =
            split_origin(r.get("url").and_then(Value::as_str).unwrap_or_default());
        let path = normalise_path(&raw_path);
        if !origin.is_empty() && !hosts.contains(&origin) {
            hosts.push(origin);
        }
        let name = r.get("name").and_then(Value::as_str);

        let mut params = Vec::new();
        for h in r
            .get("headers")
            .and_then(Value::as_array)
            .unwrap_or(&vec![])
        {
            if h.get("disabled").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            if let Some(k) = h.get("name").and_then(Value::as_str) {
                params.push(ToolParam {
                    name: k.to_string(),
                    location: ParamLocation::Header,
                    required: false,
                    description: None,
                    schema: Some(serde_json::json!({ "type": "string" })),
                    fill: Fill::agent(),
                });
            }
        }
        for q in r
            .get("parameters")
            .and_then(Value::as_array)
            .unwrap_or(&vec![])
        {
            if q.get("disabled").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            if let Some(k) = q.get("name").and_then(Value::as_str) {
                params.push(ToolParam {
                    name: k.to_string(),
                    location: ParamLocation::Query,
                    required: false,
                    description: None,
                    schema: Some(serde_json::json!({ "type": "string" })),
                    fill: Fill::agent(),
                });
            }
        }
        for name in path_variables(&path) {
            params.push(ToolParam {
                name,
                location: ParamLocation::Path,
                required: true,
                description: None,
                schema: Some(serde_json::json!({ "type": "string" })),
                fill: Fill::agent(),
            });
        }
        if let Some(text) = r.pointer("/body/text").and_then(Value::as_str) {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text) {
                for (name, example) in map {
                    params.push(ToolParam {
                        name,
                        location: ParamLocation::Body,
                        required: false,
                        description: None,
                        schema: Some(serde_json::json!({ "type": json_type(&example) })),
                        fill: Fill::agent(),
                    });
                }
            }
        }

        ops.push(ToolOperation {
            id: slug(name, method, &path),
            method,
            path,
            summary: name.map(str::to_string),
            enabled: true,
            params,
        });
    }
    (hosts.first().cloned().unwrap_or_default(), ops)
}

// --- shared helpers ---------------------------------------------------------

fn method_of(verb: &str) -> Option<ToolMethod> {
    Some(match verb.to_ascii_uppercase().as_str() {
        "GET" => ToolMethod::Get,
        "POST" => ToolMethod::Post,
        "PUT" => ToolMethod::Put,
        "PATCH" => ToolMethod::Patch,
        "DELETE" => ToolMethod::Delete,
        "HEAD" => ToolMethod::Head,
        "OPTIONS" => ToolMethod::Options,
        _ => return None,
    })
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        _ => "string",
    }
}

/// `https://api.example.com/v1/users` -> (`https://api.example.com`, `/v1/users`)
fn split_origin(raw: &str) -> (String, String) {
    // Postman and Insomnia URLs routinely contain {{variables}}, including in
    // the host. Those are the user's to resolve, so they are left in the path
    // and simply not treated as an origin.
    let raw = raw.trim();
    for scheme in ["https://", "http://"] {
        if let Some(rest) = raw.strip_prefix(scheme) {
            let (host, path) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, "/"),
            };
            let path = path.split(['?', '#']).next().unwrap_or("/");
            return (format!("{scheme}{host}"), path.to_string());
        }
    }
    let path = raw.split(['?', '#']).next().unwrap_or("/");
    (String::new(), normalise_path(path))
}

fn normalise_path(p: &str) -> String {
    let p = p.split(['?', '#']).next().unwrap_or("/").trim();
    if p.is_empty() {
        return "/".into();
    }
    // Postman writes `:id`, OpenAPI writes `{id}`. One shape downstream.
    let converted = p
        .split('/')
        .map(|seg| match seg.strip_prefix(':') {
            Some(name) if !name.is_empty() => format!("{{{name}}}"),
            _ => seg.to_string(),
        })
        .collect::<Vec<_>>()
        .join("/");
    if converted.starts_with('/') {
        converted
    } else {
        format!("/{converted}")
    }
}

/// `{id}` placeholders in a path are parameters the caller must fill.
fn path_variables(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        let name = &rest[start + 1..start + end];
        // `{{postman_var}}` is an environment variable, not a path parameter.
        if !name.is_empty() && !name.starts_with('{') && !out.contains(&name.to_string()) {
            out.push(name.to_string());
        }
        rest = &rest[start + end + 1..];
    }
    out
}

/// A stable, charset-safe operation id.
///
/// This becomes half of an MCP tool name (`<tool>__<id>`), so it is restricted
/// to `^[a-zA-Z0-9_][a-zA-Z0-9_-]*$` regardless of what the document called it.
fn slug(preferred: Option<&str>, method: ToolMethod, path: &str) -> String {
    let from_name = preferred.map(sanitise).filter(|s| !s.is_empty());
    from_name.unwrap_or_else(|| {
        let p = sanitise(path);
        let p = p.trim_matches('_');
        if p.is_empty() {
            method.as_str().to_ascii_lowercase()
        } else {
            format!("{}_{}", method.as_str().to_ascii_lowercase(), p)
        }
    })
}

fn sanitise(s: &str) -> String {
    let mut out = String::new();
    let mut last_us = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_us = false;
        } else if !last_us && !out.is_empty() {
            out.push('_');
            last_us = true;
        }
    }
    let out = out.trim_matches('_').to_string();
    // The id must not START with '-' or a digit-unfriendly char per the
    // contract's charset; a leading digit is allowed, a leading '-' is not.
    match out.chars().next() {
        Some(c) if c.is_ascii_alphanumeric() || c == '_' => out,
        _ => format!("op_{out}"),
    }
}

/// Ids must be unique within a node: they are the address an agent calls, and
/// two operations answering to one name is a silent misroute.
fn dedupe_ids(ops: &mut [ToolOperation]) {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for op in ops.iter_mut() {
        let n = seen.entry(op.id.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            op.id = format!("{}_{}", op.id, *n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op<'a>(imported: &'a Imported, id: &str) -> &'a ToolOperation {
        imported
            .operations
            .iter()
            .find(|o| o.id == id)
            .unwrap_or_else(|| {
                panic!(
                    "no operation {id:?}; got {:?}",
                    imported
                        .operations
                        .iter()
                        .map(|o| &o.id)
                        .collect::<Vec<_>>()
                )
            })
    }

    fn param<'a>(o: &'a ToolOperation, name: &str) -> &'a ToolParam {
        o.params
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no param {name:?} on {}", o.id))
    }

    const PETSTORE: &str = r##"{
      "openapi": "3.0.0",
      "info": {"title": "Petstore", "version": "1.0"},
      "servers": [{"url": "https://api.petstore.example/v1/"}],
      "paths": {
        "/pets": {
          "parameters": [
            {"name": "X-Tenant", "in": "header", "required": true, "schema": {"type": "string"}}
          ],
          "get": {
            "operationId": "listPets",
            "summary": "List all pets",
            "parameters": [
              {"name": "limit", "in": "query", "schema": {"type": "integer"}, "description": "how many"}
            ]
          },
          "post": {
            "operationId": "createPet",
            "requestBody": {
              "required": true,
              "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Pet"}}}
            }
          }
        },
        "/pets/{petId}": {
          "get": {
            "operationId": "showPetById",
            "parameters": [
              {"name": "petId", "in": "path", "required": true, "schema": {"type": "string"}}
            ]
          },
          "delete": {}
        }
      },
      "components": {
        "schemas": {
          "Pet": {
            "type": "object",
            "required": ["name"],
            "properties": {
              "name": {"type": "string"},
              "tag": {"type": "string", "description": "a tag"}
            }
          }
        }
      }
    }"##;

    #[test]
    fn openapi3_yields_the_operations_paths_and_parameters() {
        let got = import(PETSTORE, None).unwrap();
        assert_eq!(got.format, ToolFormat::Openapi);
        // The trailing slash on the server URL must not survive: every path
        // begins with one, and `//pets` is a different resource.
        assert_eq!(got.base_url, "https://api.petstore.example/v1");
        assert_eq!(got.operations.len(), 4);

        let list = op(&got, "listPets");
        assert_eq!(list.method, ToolMethod::Get);
        assert_eq!(list.path, "/pets");
        assert_eq!(list.summary.as_deref(), Some("List all pets"));
        assert!(list.enabled);

        // A path-level parameter applies to every method under it.
        assert_eq!(param(list, "X-Tenant").location, ParamLocation::Header);
        assert!(param(list, "X-Tenant").required);
        assert_eq!(
            param(op(&got, "createPet"), "X-Tenant").location,
            ParamLocation::Header
        );

        let limit = param(list, "limit");
        assert_eq!(limit.location, ParamLocation::Query);
        assert!(!limit.required);
        assert_eq!(limit.description.as_deref(), Some("how many"));
        assert_eq!(limit.schema.as_ref().unwrap()["type"], "integer");

        // Path templates keep their placeholders.
        assert_eq!(op(&got, "showPetById").path, "/pets/{petId}");
        assert_eq!(
            param(op(&got, "showPetById"), "petId").location,
            ParamLocation::Path
        );
    }

    /// A `$ref` body must be followed, or the agent is given an operation with
    /// no fields and no way to say what it wants.
    #[test]
    fn a_referenced_request_body_becomes_fillable_fields() {
        let got = import(PETSTORE, None).unwrap();
        let create = op(&got, "createPet");
        let name = param(create, "name");
        assert_eq!(name.location, ParamLocation::Body);
        assert!(name.required, "declared required and the body is required");
        let tag = param(create, "tag");
        assert!(!tag.required);
        assert_eq!(tag.description.as_deref(), Some("a tag"));
    }

    /// An operation with no operationId still needs a stable, safe id.
    #[test]
    fn an_operation_without_an_id_gets_one_from_its_method_and_path() {
        let got = import(PETSTORE, None).unwrap();
        let derived = op(&got, "delete_pets_petId");
        assert_eq!(derived.method, ToolMethod::Delete);
        assert_eq!(derived.path, "/pets/{petId}");
    }

    /// Every id becomes half of an MCP tool name, so the charset is not
    /// negotiable however the document spelled things.
    #[test]
    fn every_id_is_safe_to_use_as_an_mcp_tool_name() {
        let doc = r#"{
          "openapi": "3.0.0",
          "servers": [{"url": "https://x.example"}],
          "paths": {
            "/a": {"get": {"operationId": "get user/profile (v2)!"}},
            "/b": {"get": {"operationId": "  "}},
            "/c": {"get": {"operationId": "-leading-dash"}}
          }
        }"#;
        let got = import(doc, None).unwrap();
        for o in &got.operations {
            assert!(
                !o.id.is_empty()
                    && o.id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "unsafe id {:?}",
                o.id
            );
            let first = o.id.chars().next().unwrap();
            assert!(
                first.is_ascii_alphanumeric() || first == '_',
                "id must not start with {first:?}: {:?}",
                o.id
            );
            assert!(o.mcp_name("tool").starts_with("tool__"));
        }
    }

    /// Two operations answering to one name is a silent misroute.
    #[test]
    fn duplicate_ids_are_made_unique() {
        let doc = r#"{
          "openapi": "3.0.0",
          "servers": [{"url": "https://x.example"}],
          "paths": {
            "/a": {"get": {"operationId": "same"}},
            "/b": {"get": {"operationId": "same"}},
            "/c": {"get": {"operationId": "same"}}
          }
        }"#;
        let got = import(doc, None).unwrap();
        let mut ids: Vec<&str> = got.operations.iter().map(|o| o.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 3, "ids must be unique within a node");
    }

    /// Most real OpenAPI documents are YAML. Refusing them would exclude most
    /// of the specs anyone actually has.
    #[test]
    fn yaml_documents_are_accepted_too() {
        let yaml = "openapi: 3.0.0\n\
                    servers:\n  - url: https://yaml.example\n\
                    paths:\n  /ping:\n    get:\n      operationId: ping\n";
        let got = import(yaml, None).unwrap();
        assert_eq!(got.format, ToolFormat::Openapi);
        assert_eq!(got.base_url, "https://yaml.example");
        assert_eq!(op(&got, "ping").path, "/ping");
    }

    const SWAGGER2: &str = r#"{
      "swagger": "2.0",
      "host": "api.legacy.example",
      "basePath": "/v2",
      "schemes": ["https"],
      "paths": {
        "/things": {
          "post": {
            "operationId": "addThing",
            "parameters": [
              {"name": "api_key", "in": "header", "required": true, "type": "string"},
              {"name": "body", "in": "body", "required": true,
               "schema": {"type": "object", "required": ["title"],
                          "properties": {"title": {"type": "string"}, "count": {"type": "integer"}}}}
            ]
          }
        },
        "/upload": {
          "post": {
            "operationId": "upload",
            "parameters": [{"name": "file", "in": "formData", "type": "string", "required": true}]
          }
        }
      }
    }"#;

    #[test]
    fn swagger2_builds_its_base_url_and_reads_both_body_styles() {
        let got = import(SWAGGER2, None).unwrap();
        assert_eq!(got.format, ToolFormat::Swagger2);
        assert_eq!(got.base_url, "https://api.legacy.example/v2");

        let add = op(&got, "addThing");
        assert_eq!(param(add, "api_key").location, ParamLocation::Header);
        // `in: body` is a schema whose properties are the fields.
        assert_eq!(param(add, "title").location, ParamLocation::Body);
        assert!(param(add, "title").required);
        assert_eq!(param(add, "count").location, ParamLocation::Body);
        assert!(!param(add, "count").required);

        // `in: formData` is one field each.
        assert_eq!(
            param(op(&got, "upload"), "file").location,
            ParamLocation::Body
        );
    }

    const POSTMAN: &str = r#"{
      "info": {"_postman_id": "abc", "name": "My API",
               "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
      "item": [
        {"name": "Folder", "item": [
          {"name": "Get user",
           "request": {"method": "GET",
             "header": [{"key": "Authorization", "value": "Bearer x"},
                        {"key": "X-Off", "value": "1", "disabled": true}],
             "url": {"raw": "https://api.postman.example/users/:userId?verbose=1",
                     "query": [{"key": "verbose", "value": "1"}]}}}
        ]},
        {"name": "Create user",
         "request": {"method": "POST",
           "url": {"raw": "https://api.postman.example/users"},
           "body": {"mode": "raw", "raw": "{\"email\": \"a@b.c\", \"age\": 30}"}}}
      ]
    }"#;

    #[test]
    fn postman_walks_folders_and_reads_urls_headers_and_bodies() {
        let got = import(POSTMAN, None).unwrap();
        assert_eq!(got.format, ToolFormat::Postman);
        assert_eq!(got.base_url, "https://api.postman.example");
        assert_eq!(got.operations.len(), 2, "nested folder items must be found");

        let get = op(&got, "Get_user");
        assert_eq!(get.method, ToolMethod::Get);
        // Postman's `:userId` and OpenAPI's `{userId}` mean the same thing;
        // downstream sees one shape.
        assert_eq!(get.path, "/users/{userId}");
        assert_eq!(param(get, "userId").location, ParamLocation::Path);
        assert!(param(get, "userId").required);
        assert_eq!(param(get, "Authorization").location, ParamLocation::Header);
        assert_eq!(param(get, "verbose").location, ParamLocation::Query);
        // A disabled header is one the user turned OFF; importing it would
        // re-enable something they had deliberately silenced.
        assert!(get.params.iter().all(|p| p.name != "X-Off"));

        // A raw JSON body's KEYS are the fields an agent fills, and their
        // example values suggest the types.
        let create = op(&got, "Create_user");
        assert_eq!(param(create, "email").location, ParamLocation::Body);
        assert_eq!(
            param(create, "email").schema.as_ref().unwrap()["type"],
            "string"
        );
        assert_eq!(
            param(create, "age").schema.as_ref().unwrap()["type"],
            "integer"
        );
    }

    const INSOMNIA: &str = r#"{
      "_type": "export", "__export_format": 4,
      "resources": [
        {"_type": "workspace", "name": "ws"},
        {"_type": "request", "name": "Search", "method": "GET",
         "url": "https://api.insomnia.example/search",
         "headers": [{"name": "Accept", "value": "application/json"},
                     {"name": "X-Off", "value": "1", "disabled": true}],
         "parameters": [{"name": "q", "value": "cats"}]},
        {"_type": "request", "name": "Submit", "method": "POST",
         "url": "https://api.insomnia.example/items/{itemId}",
         "body": {"mimeType": "application/json", "text": "{\"note\": \"hi\", \"n\": 2}"}}
      ]
    }"#;

    #[test]
    fn insomnia_reads_requests_and_ignores_everything_else() {
        let got = import(INSOMNIA, None).unwrap();
        assert_eq!(got.format, ToolFormat::Insomnia);
        assert_eq!(got.base_url, "https://api.insomnia.example");
        // The workspace resource is not an operation.
        assert_eq!(got.operations.len(), 2);

        let search = op(&got, "Search");
        assert_eq!(param(search, "Accept").location, ParamLocation::Header);
        assert_eq!(param(search, "q").location, ParamLocation::Query);
        assert!(search.params.iter().all(|p| p.name != "X-Off"));

        let submit = op(&got, "Submit");
        assert_eq!(submit.path, "/items/{itemId}");
        assert_eq!(param(submit, "itemId").location, ParamLocation::Path);
        assert_eq!(param(submit, "note").location, ParamLocation::Body);
        assert_eq!(
            param(submit, "n").schema.as_ref().unwrap()["type"],
            "integer"
        );
    }

    /// Every field starts as the agent's to fill (§3d default), and the user
    /// narrows it afterwards. Importing anything as `static` or `vault` would
    /// invent a decision nobody made.
    #[test]
    fn everything_imports_as_an_agent_field() {
        for doc in [PETSTORE, SWAGGER2, POSTMAN, INSOMNIA] {
            let got = import(doc, None).unwrap();
            for o in &got.operations {
                for p in &o.params {
                    assert_eq!(p.fill.mode, wheel_core::FillMode::Agent, "{}", o.id);
                    assert!(p.fill.value.is_none());
                    assert!(p.fill.vault_ref.is_none());
                }
            }
        }
    }

    #[test]
    fn the_format_is_detected_without_being_told() {
        assert_eq!(import(PETSTORE, None).unwrap().format, ToolFormat::Openapi);
        assert_eq!(import(SWAGGER2, None).unwrap().format, ToolFormat::Swagger2);
        assert_eq!(import(POSTMAN, None).unwrap().format, ToolFormat::Postman);
        assert_eq!(import(INSOMNIA, None).unwrap().format, ToolFormat::Insomnia);
    }

    #[test]
    fn a_document_that_is_not_a_spec_says_so_rather_than_importing_nothing() {
        let err = import(r#"{"hello": "world"}"#, None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("could not tell what kind of document"),
            "{err}"
        );

        let err = import("not json, not yaml: [", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("neither valid JSON nor valid YAML"), "{err}");

        // A real spec shape with nothing callable in it is a different error.
        let err = import(r#"{"openapi": "3.0.0", "paths": {}}"#, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no operations"), "{err}");
    }

    /// A spec with one malformed operation out of forty should import
    /// thirty-nine, not nothing.
    #[test]
    fn one_unusable_operation_does_not_lose_the_rest() {
        let doc = r#"{
          "openapi": "3.0.0",
          "servers": [{"url": "https://x.example"}],
          "paths": {
            "/ok": {"get": {"operationId": "fine"}},
            "/weird": {"get": {"operationId": "alsoFine",
                       "parameters": [{"name": "nope", "in": "nowhere"},
                                      {"in": "query"},
                                      {"name": "yes", "in": "query"}]}},
            "/notamethod": {"x-vendor": {"operationId": "ignored"}}
          }
        }"#;
        let got = import(doc, None).unwrap();
        assert_eq!(got.operations.len(), 2);
        let weird = op(&got, "alsoFine");
        // The unusable parameters are skipped; the usable one survives.
        assert_eq!(weird.params.len(), 1);
        assert_eq!(weird.params[0].name, "yes");
    }

    /// A `$ref` cycle is a document someone will paste in one day.
    #[test]
    fn a_reference_cycle_does_not_hang_the_import() {
        let doc = r##"{
          "openapi": "3.0.0",
          "servers": [{"url": "https://x.example"}],
          "paths": {"/a": {"post": {"operationId": "loop",
            "requestBody": {"$ref": "#/components/requestBodies/A"}}}},
          "components": {"requestBodies": {
            "A": {"$ref": "#/components/requestBodies/B"},
            "B": {"$ref": "#/components/requestBodies/A"}}}
        }"##;
        let got = import(doc, None).unwrap();
        assert_eq!(got.operations.len(), 1);
    }

    /// Postman and Insomnia URLs routinely contain {{variables}}, including in
    /// the host. Those are the user's to resolve and must not be mistaken for
    /// path parameters an agent should fill.
    #[test]
    fn environment_variables_in_a_url_are_not_path_parameters() {
        let doc = r#"{
          "info": {"_postman_id": "x"},
          "item": [{"name": "Var", "request": {"method": "GET",
                    "url": {"raw": "{{base_url}}/things/:id"}}}]
        }"#;
        let got = import(doc, None).unwrap();
        let o = &got.operations[0];
        assert!(
            o.params.iter().any(|p| p.name == "id"),
            "a real path parameter must still be found: {:?}",
            o.params
        );
        assert!(
            o.params.iter().all(|p| !p.name.contains("base_url")),
            "an environment variable is not a path parameter: {:?}",
            o.params
        );
    }
}
