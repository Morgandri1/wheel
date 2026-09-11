// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `GET /v1/engine`: what a running engine build can do.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The capabilities implemented by a running engine build.
///
/// Node configs reject unknown fields, so a client checks `features` before
/// sending an optional one. Deliberately NOT `deny_unknown_fields`: this is the
/// document a newer engine extends, and an older client must still read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EngineInfo {
    /// The engine crate version, compiled in.
    pub version: String,
    /// The commit this binary was built from, or `unknown` for an unstamped build.
    pub build: String,
    /// The control-plane contract version, `v1`.
    pub api_version: String,
    /// Harnesses this build can run. A modelled harness without a driver is absent.
    pub harnesses: Vec<String>,
    /// Spawn profiles this build supports.
    pub profiles: Vec<String>,
    /// Stable ids of implemented capabilities. Clients ignore ids they do not know.
    pub features: Vec<String>,
}
