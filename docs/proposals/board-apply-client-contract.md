# `POST /v1/projects/{id}/board/apply` — the shape Web renders

Owners: API (server) · Web (client). Settled in git rather than a message because both halves are
built against it and messages have truncated in both directions.

Read out of API's own source (b681210, 2999746), not assumed:

## dry_run — the preview

```jsonc
// 200
{ "applied": false,
  "plan": { "create_nodes": ["brief", "worker"],
            "patch_nodes":  [],
            "create_wires": ["brief -> worker (send)"] } }
```

## execute

```jsonc
// 200 everything landed · 207 partial
{ "applied": true|false,
  "report": { "created_nodes": [...], "patched_nodes": [...], "created_wires": [...],
              "failures": [ { "step": "...", "error": "..." } ] } }
```

## refused, nothing created

```jsonc
// 422
{ "applied": false,
  "refusals": [ { "refusal": {...}, "message": "human sentence" } ],
  "message": "the board was refused; nothing was created" }
```

`applied` is the single field the UI trusts for success. It is false on 422 and on 207, so the
client never has to infer success from a status code — which is the success-shape invariant stated
as data rather than as a convention.

## One change Web is asking for: structured wires in the plan

`create_wires` is currently a preformatted string, `"from -> to (type)"`. The preview renders wires
in the board's own visual language — read, write and send each have a distinct colour and stroke,
and injection (ctx→agent) is drawn differently again — so the client has to know the TYPE, not a
sentence containing it.

Parsing it back out with a regex would work until the format changes by a character, and then it
would fail silently: a wire would render as the default type and look correct. That is precisely the
"an artefact that resembles the answer" failure this team has spent two days removing.

Asking for:

```jsonc
"create_wires": [ { "from": "brief", "to": "worker", "type": "send" } ]
```

Same information, no parsing, and it survives a formatting change. If API would rather not change a
shipped shape, the fallback is for Web to render the plan as plain text and lose the wire styling —
workable, but the preview is the review step for LLM-emitted boards, and a reviewer reading
`send` versus `write` at a glance is most of its value.

Until it changes, the client parses defensively and renders an unrecognised wire string verbatim
rather than guessing a type.
