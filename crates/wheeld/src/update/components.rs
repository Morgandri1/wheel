// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Which part of Wheel a changed path belongs to, and whether this deployment
//! runs it. First match wins, in the order of the proposal's table.

use wheel_core::Component;

/// What `wheeld` runs. Web is a separate process and docs run nowhere.
pub const WHEELD_RUNS: &[Component] = &[
    Component::Core,
    Component::Engine,
    Component::Cli,
    Component::Host,
    Component::Api,
];

pub fn classify(path: &str) -> Component {
    let under = |dir: &str| path.starts_with(dir);
    let qa_budget = path
        .strip_prefix("qa/")
        .is_some_and(|rest| !rest.contains('/') && rest.ends_with(".json"));

    if under(".github/")
        || path == "qa/check.sh"
        || under("qa/tools/")
        || qa_budget
        || path == "Makefile"
    {
        Component::Ci
    } else if under("crates/wheel-core/")
        || under("crates/wheel-sqlite/")
        || path == "Cargo.toml"
        || path == "Cargo.lock"
        || under("rust-toolchain")
    {
        Component::Core
    } else if under("crates/wheel-engine/") {
        Component::Engine
    } else if under("crates/wheel-cli/") {
        Component::Cli
    } else if under("crates/wheel-host/") || under("crates/wheeld/") {
        Component::Host
    } else if under("crates/wheel-api/") {
        Component::Api
    } else if under("crates/") {
        // A crate nobody has classified yet is Rust in the workspace: assume it
        // touches everything rather than let it through as "not pertinent".
        Component::Core
    } else if under("web/") {
        Component::Web
    } else if under("docs/") || under("redteam/") || path.ends_with(".md") {
        Component::Docs
    } else {
        Component::Other
    }
}

/// Sorted and without repeats, so a notice reads the same whatever order git
/// listed the paths in.
pub fn of(paths: &[String]) -> Vec<Component> {
    let mut out: Vec<Component> = paths.iter().map(|p| classify(p)).collect();
    out.sort();
    out.dedup();
    out
}

pub fn pertinent(changed: &[Component], runs: &[Component]) -> bool {
    // `core` is every Rust component's dependency, so a deployment that runs any
    // of them runs it too. Without this a `Cargo.lock` bump would read as "not
    // pertinent" to an engine-only deployment, which is how a board silently
    // keeps running code it was told to replace.
    let runs_rust = runs.iter().any(|c| {
        matches!(
            c,
            Component::Core | Component::Engine | Component::Cli | Component::Host | Component::Api
        )
    });
    changed
        .iter()
        .any(|c| runs.contains(c) || (*c == Component::Core && runs_rust))
}

/// Only the components a notice should name: what this deployment runs, plus
/// `ci`, which is why an update may be operator-only.
pub fn named(changed: &[Component], runs: &[Component]) -> Vec<Component> {
    changed
        .iter()
        .copied()
        .filter(|c| *c == Component::Ci || runs.contains(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use Component::*;

    #[test]
    fn every_row_of_the_proposals_table_maps_where_it_says() {
        for (path, want) in [
            (".github/workflows/ci.yml", Ci),
            ("qa/check.sh", Ci),
            ("qa/tools/coverage_gate.py", Ci),
            ("qa/coverage-floors.json", Ci),
            ("Makefile", Ci),
            ("crates/wheel-core/src/update.rs", Core),
            ("crates/wheel-sqlite/src/lib.rs", Core),
            ("Cargo.toml", Core),
            ("Cargo.lock", Core),
            ("rust-toolchain.toml", Core),
            ("crates/wheel-engine/src/supervisor/mod.rs", Engine),
            ("crates/wheel-engine/README.md", Engine),
            ("crates/wheel-cli/src/main.rs", Cli),
            ("crates/wheel-host/src/lib.rs", Host),
            ("crates/wheeld/src/update/mod.rs", Host),
            ("crates/wheel-api/src/lib.rs", Api),
            ("crates/wheel-new-thing/src/lib.rs", Core),
            ("web/src/app/page.tsx", Web),
            ("docs/ARCHITECTURE.md", Docs),
            ("redteam/findings/001.md", Docs),
            ("README.md", Docs),
            ("infra/railway/README.md", Docs),
            ("qa/fixtures/board.json", Other),
            ("qa/harness/fake-claude", Other),
            ("docker/Dockerfile.host", Other),
            ("infra/trim-target.sh", Other),
        ] {
            assert_eq!(classify(path), want, "{path}");
        }
    }

    #[test]
    fn a_docs_only_change_is_not_pertinent_and_an_engine_one_is() {
        let docs = of(&["docs/a.md".into(), "README.md".into()]);
        assert_eq!(docs, vec![Docs]);
        assert!(!pertinent(&docs, WHEELD_RUNS));

        let web_and_infra = of(&["web/x.ts".into(), "infra/y.sh".into()]);
        assert!(!pertinent(&web_and_infra, WHEELD_RUNS));

        let engine = of(&["docs/a.md".into(), "crates/wheel-engine/src/lib.rs".into()]);
        assert!(pertinent(&engine, WHEELD_RUNS));
    }

    /// `core` touches everything, so a deployment running anything runs it.
    #[test]
    fn a_core_change_is_pertinent_to_any_rust_deployment() {
        let core = of(&["Cargo.lock".into()]);
        for runs in [&[Engine][..], &[Cli][..], WHEELD_RUNS] {
            assert!(pertinent(&core, runs), "{runs:?}");
        }
    }

    /// A CI-only change runs nowhere, so it is not worth a restart — but when it
    /// rides along with code, the notice must say so, because it is why the
    /// update becomes operator-only.
    #[test]
    fn ci_is_named_but_never_pertinent_on_its_own() {
        let ci = of(&[".github/workflows/ci.yml".into()]);
        assert!(!pertinent(&ci, WHEELD_RUNS));

        let mixed = of(&[
            "crates/wheel-cli/src/main.rs".into(),
            ".github/workflows/ci.yml".into(),
            "docs/x.md".into(),
            "crates/wheel-cli/src/mcp.rs".into(),
        ]);
        assert_eq!(mixed, vec![Ci, Cli, Docs]);
        assert_eq!(named(&mixed, WHEELD_RUNS), vec![Ci, Cli]);
    }
}
