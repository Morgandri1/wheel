// Finding 034 — poison message panics the AgentPrompt escaper by a byte-index slice off a char boundary.
// The slice condition is ported VERBATIM from crates/wheel-core/src/message.rs:183-214
// (escape_envelope_body). In Rust, `body[name_at..name_at+TAG.len()]` PANICS if either bound is not a
// UTF-8 char boundary ("byte index N is not a char boundary"). This reproduces that exact bound in JS
// and flags it — JS won't panic, so we detect the off-boundary slice the way Rust's slice index checks it.
// Run: node t_escape_bytepanic.mjs   (exit 1 = a panic-triggering input found)

const TAG = "agentprompt"; // 11 bytes
const enc = new TextEncoder();

// char-boundary set of a UTF-8 byte string: index i is a boundary iff byte[i] is not a continuation byte (0b10xxxxxx)
function boundaries(bytes) {
  const b = new Set([bytes.length]);
  for (let i = 0; i < bytes.length; i++) if ((bytes[i] & 0xc0) !== 0x80) b.add(i);
  return b;
}

// Returns the (name_at, end) slice Rust would take for the FIRST '<', or null if the length guard fails.
function firstSlice(s) {
  const bytes = enc.encode(s);
  for (let i = 0; i < bytes.length; i++) {
    if (bytes[i] === 0x3c /* '<' */) {
      const after = i + 1;
      const name_at = after < bytes.length && bytes[after] === 0x2f /* '/' */ ? after + 1 : after;
      if (bytes.length >= name_at + TAG.length) return { bytes, name_at, end: name_at + TAG.length };
      return null; // length guard false -> no slice -> no panic for this '<'
    }
  }
  return null;
}

function panics(s) {
  const sl = firstSlice(s);
  if (!sl) return false;
  const b = boundaries(sl.bytes);
  // Rust panics if EITHER slice bound is not on a char boundary.
  return !b.has(sl.name_at) || !b.has(sl.end);
}

const cases = [
  ["<—————", "'<' + em-dashes (the production instance class: TAG is 11 bytes, need name_at+11 off a boundary)"],
  ["hello <————— world", "em-dashes after a '<' mid-message"],
  ["</————", "'</' + em-dashes (the escaper's own close-tag path)"],
  ["<\u{1F600}\u{1F600}\u{1F600}\u{1F600}", "'<' + emoji (4-byte chars)"],
];
const safe = [
  ["</agentprompt>", "a real close tag (ascii) — must NOT panic"],
  ["a — b", "an em dash with no preceding '<' — must NOT panic"],
  ["<x", "'<' then short — length guard saves it"],
];

let found = 0;
console.log("PANIC-TRIGGERING inputs (Rust escape_envelope_body would panic):");
for (const [s, why] of cases) {
  const p = panics(s);
  console.log(`  ${p ? "PANIC" : "ok   "}  ${why}  ${p ? "-> byte slice off a char boundary" : ""}`);
  if (p) found++;
}
console.log("Control (must NOT panic):");
for (const [s, why] of safe) {
  const p = panics(s);
  console.log(`  ${p ? "PANIC(BAD)" : "ok"}  ${why}`);
  if (p) { console.log("  FAIL: a safe input was flagged"); process.exit(2); }
}
if (found) { console.log(`\n${found} poison inputs -> escape_envelope_body panics -> the delivering engine dies; reconcile replays -> PERMANENT project DoS.`); process.exit(1); }
console.log("no panic found"); process.exit(0);
