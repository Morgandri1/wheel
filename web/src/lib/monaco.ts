// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

import { loader } from "@monaco-editor/react";

/**
 * Point the editor at our own copy (public/monaco, written by scripts/copy-monaco.mjs) instead of
 * the jsDelivr default. Third-party code on this origin can read the session token; the editor
 * is not worth that. Imported for its side effect by every panel that mounts Monaco.
 */
loader.config({ paths: { vs: "/monaco/vs" } });
