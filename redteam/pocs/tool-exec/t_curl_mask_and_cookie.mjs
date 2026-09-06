// Tool executor — TWO findings on the PURE layer (no route needed). Owner: SDK/Engine.
// The functions below are ported VERBATIM from crates/wheel-engine/src/tools/execute.rs @1f1d5e0
// (only Rust->JS syntax changed; the logic is identical) so this runs the actual behaviour:
//   encode()        execute.rs:197-208   (percent-encode all but unreserved)
//   url build       execute.rs:153-161   (query = encode(k)=encode(v))
//   path fill       execute.rs:133       (encode(value) into the path)
//   curl mask()     execute.rs:216-224   (replace each RAW secret with <redacted> in headers/cookies/body/URL)
//   cookie join     execute.rs:286-287   ("; ".join(`${k}=${v}`), value NOT encoded)  [send(), also curl_for:234]
// Run: node t_curl_mask_and_cookie.mjs   (exit 1 = findings reproduced)

const UNRESERVED = /[A-Za-z0-9\-_.~]/;
const encode = (s) => {
  let out = "";
  for (const b of Buffer.from(s, "utf8")) {
    const ch = String.fromCharCode(b);
    out += UNRESERVED.test(ch) ? ch : "%" + b.toString(16).toUpperCase().padStart(2, "0");
  }
  return out;
};
// curl_for's mask: replace each raw secret substring with <redacted>
const mask = (s, secrets) => {
  let out = s;
  for (const sec of secrets) if (sec) out = out.split(sec).join("<redacted>");
  return out;
};

const findings = [];

// ---- FINDING 1: a vault/static secret placed in a QUERY (or PATH) survives into the curl URL ----
// A realistic secret that contains non-unreserved chars (base64-ish: '/', '+', '='):
const SECRET = "sk/live+abc=="; // e.g. a vault-filled API key used as a query value
const secrets = [SECRET];
// url built by build_request for a query-mode fill (execute.rs:153-161):
const url = "https://api.example.com/data" + "?" + `${encode("key")}=${encode(SECRET)}`;
const curlUrl = mask(url, secrets); // curl_for masks p.url (execute.rs:246)
console.log("query url in curl :", curlUrl);
if (!curlUrl.includes("<redacted>") && /sk%2Flive/.test(curlUrl)) {
  findings.push("FINDING 1: query-placed secret is percent-encoded in p.url but mask() searches the RAW value -> the (encoded, trivially-decodable) secret SURVIVES in the curl string");
}
// same for a PATH fill (execute.rs:133 encode(value)):
const pathUrl = mask("https://api.example.com/t/" + encode(SECRET), secrets);
console.log("path url in curl  :", pathUrl);
if (!pathUrl.includes("<redacted>") && /sk%2Flive/.test(pathUrl)) {
  findings.push("FINDING 1b: path-placed secret likewise survives (encoded) in the curl string");
}
// contrast: a HEADER-placed secret IS masked (header value is not encoded), proving the gap is encoding-specific:
const hdr = mask(`x-api-key: ${SECRET}`, secrets);
console.log("header in curl    :", hdr, hdr.includes("<redacted>") ? "(masked OK)" : "(LEAK)");

// ---- FINDING 2: agent cookie VALUE is not encoded -> "; k=v" injects a second cookie ----
// build_request pushes the cookie value raw (execute.rs:137); send()/curl join with "; " (286-287).
const cookies = [["sid", "x; admin=true; role=root"]]; // agent-supplied value for an agent-visible cookie param
const cookieHeader = cookies.map(([k, v]) => `${k}=${v}`).join("; ");
console.log("Cookie header     :", cookieHeader);
if (/;\s*admin=true/.test(cookieHeader)) {
  findings.push("FINDING 2: cookie values are not percent-encoded; an agent value 'x; admin=true' injects additional cookies (structure out of a value). Query/path are encoded but cookies are not.");
}

if (findings.length) {
  console.log("");
  for (const f of findings) console.log("FAIL:", f);
  process.exit(1);
}
console.log("PASS: no leak");
