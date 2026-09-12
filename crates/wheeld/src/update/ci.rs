// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The CI gate: is the target commit green on GitHub Actions?
//!
//! Only check-runs from the GitHub Actions app count. Commit statuses are
//! ignored entirely — any token with `repo:status` can post one — and so is a
//! check-run from any other app, since an app with `checks:write` could post a
//! green one for any commit (proposal T1c). Unverifiable is never green.

use async_trait::async_trait;
use serde::Deserialize;

pub const ACTIONS_APP: &str = "github-actions";
const PAGE: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Green,
    Pending,
    Red(String),
    Unverifiable(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckRun {
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub app: Option<App>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct App {
    pub slug: String,
}

#[derive(Deserialize)]
struct Page {
    total_count: usize,
    check_runs: Vec<CheckRun>,
}

pub fn evaluate(runs: &[CheckRun], required: &[String]) -> Verdict {
    let ours: Vec<&CheckRun> = runs
        .iter()
        .filter(|r| r.app.as_ref().is_some_and(|a| a.slug == ACTIONS_APP))
        .collect();
    if ours.is_empty() || ours.iter().any(|r| r.status != "completed") {
        return Verdict::Pending;
    }
    let passed = |r: &&CheckRun| {
        matches!(
            r.conclusion.as_deref(),
            Some("success" | "neutral" | "skipped")
        )
    };
    let failed: Vec<&str> = ours
        .iter()
        .filter(|r| !passed(r))
        .map(|r| r.name.as_str())
        .collect();
    if !failed.is_empty() {
        return Verdict::Red(format!("failed: {}", failed.join(", ")));
    }
    let missing: Vec<&str> = required
        .iter()
        .filter(|name| {
            !ours
                .iter()
                .any(|r| &r.name == *name && r.conclusion.as_deref() == Some("success"))
        })
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Verdict::Red(format!(
            "required check did not pass: {}",
            missing.join(", ")
        ));
    }
    Verdict::Green
}

#[async_trait]
pub trait CiGate: Send + Sync {
    async fn verdict(&self, sha: &str) -> Verdict;
}

pub struct GithubChecks {
    client: reqwest::Client,
    api: String,
    repo: Option<String>,
    token: Option<String>,
    required: Vec<String>,
}

impl GithubChecks {
    pub fn new(repo: Option<String>, token: Option<String>, required: Vec<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api: "https://api.github.com".into(),
            repo,
            token,
            required,
        }
    }

    pub fn with_api(mut self, api: impl Into<String>) -> Self {
        self.api = api.into();
        self
    }
}

#[async_trait]
impl CiGate for GithubChecks {
    async fn verdict(&self, sha: &str) -> Verdict {
        let Some(token) = &self.token else {
            return Verdict::Unverifiable(format!("{} is not set", super::policy::ENV_TOKEN));
        };
        let Some(repo) = &self.repo else {
            return Verdict::Unverifiable(format!(
                "origin is not a github.com repository and {} is unset",
                super::policy::ENV_GITHUB_REPO
            ));
        };
        if wheel_core::Sha::parse(sha).is_none() {
            return Verdict::Unverifiable(format!("{sha:?} is not a commit id"));
        }
        let url = format!(
            "{}/repos/{repo}/commits/{sha}/check-runs?per_page={PAGE}",
            self.api
        );
        let resp = self
            .client
            .get(&url)
            .bearer_auth(token)
            .header("accept", "application/vnd.github+json")
            .header("user-agent", "wheeld-auto-update")
            .header("x-github-api-version", "2022-11-28")
            .send()
            .await;
        let resp = match resp {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => return Verdict::Unverifiable(format!("GitHub answered {}", r.status())),
            Err(e) => return Verdict::Unverifiable(format!("GitHub unreachable: {e}")),
        };
        match resp.json::<Page>().await {
            Ok(page) if page.total_count > page.check_runs.len() => Verdict::Unverifiable(format!(
                "{} check runs on one commit is more than one page of {PAGE}",
                page.total_count
            )),
            Ok(page) => evaluate(&page.check_runs, &self.required),
            Err(e) => Verdict::Unverifiable(format!("GitHub's answer did not parse: {e}")),
        }
    }
}

/// `owner/name` from what `origin` points at, when that is github.com.
pub fn github_repo_from_url(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, name) = rest.split_once('/')?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    (ok(owner) && ok(name)).then(|| format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn run(name: &str, status: &str, conclusion: Option<&str>, app: &str) -> CheckRun {
        CheckRun {
            name: name.into(),
            status: status.into(),
            conclusion: conclusion.map(str::to_string),
            app: Some(App { slug: app.into() }),
        }
    }

    fn required() -> Vec<String> {
        vec!["make check".into()]
    }

    #[test]
    fn green_needs_every_actions_run_done_and_passed_and_the_required_ones_present() {
        let runs = [
            run("make check", "completed", Some("success"), ACTIONS_APP),
            run("e2e", "completed", Some("skipped"), ACTIONS_APP),
            run("size", "completed", Some("neutral"), ACTIONS_APP),
        ];
        assert_eq!(evaluate(&runs, &required()), Verdict::Green);
    }

    #[test]
    fn a_run_still_going_is_pending_and_no_runs_yet_is_pending() {
        let runs = [
            run("make check", "completed", Some("success"), ACTIONS_APP),
            run("integration", "in_progress", None, ACTIONS_APP),
        ];
        assert_eq!(evaluate(&runs, &required()), Verdict::Pending);
        assert_eq!(evaluate(&[], &required()), Verdict::Pending);
    }

    #[test]
    fn any_failure_is_red_and_names_the_check() {
        let runs = [
            run("make check", "completed", Some("success"), ACTIONS_APP),
            run("integration", "completed", Some("failure"), ACTIONS_APP),
            run("e2e", "completed", Some("cancelled"), ACTIONS_APP),
        ];
        assert_eq!(
            evaluate(&runs, &required()),
            Verdict::Red("failed: integration, e2e".into())
        );
    }

    /// Removing or skipping the gate job must not make a commit green: a
    /// required check has to be there, and has to have succeeded.
    #[test]
    fn a_required_check_that_is_missing_or_skipped_is_red() {
        let skipped = [run("make check", "completed", Some("skipped"), ACTIONS_APP)];
        assert!(
            matches!(evaluate(&skipped, &required()), Verdict::Red(r) if r.contains("make check"))
        );
        let absent = [run("lint", "completed", Some("success"), ACTIONS_APP)];
        assert!(matches!(evaluate(&absent, &required()), Verdict::Red(_)));
    }

    /// T1c: a green check-run from any other app is not our CI.
    #[test]
    fn only_the_actions_app_counts() {
        let forged = [
            run("make check", "completed", Some("success"), "some-other-app"),
            CheckRun {
                app: None,
                ..run("make check", "completed", Some("success"), "x")
            },
        ];
        assert_eq!(evaluate(&forged, &required()), Verdict::Pending);
        let mixed = [
            run("make check", "completed", Some("failure"), ACTIONS_APP),
            run("make check", "completed", Some("success"), "forger"),
        ];
        assert!(matches!(evaluate(&mixed, &required()), Verdict::Red(_)));
    }

    #[test]
    fn the_repo_comes_from_any_github_remote_form_and_nothing_else() {
        for url in [
            "https://github.com/Morgandri1/wheel.git",
            "https://github.com/Morgandri1/wheel",
            "git@github.com:Morgandri1/wheel.git",
            "ssh://git@github.com/Morgandri1/wheel.git\n",
        ] {
            assert_eq!(
                github_repo_from_url(url).as_deref(),
                Some("Morgandri1/wheel"),
                "{url}"
            );
        }
        for url in [
            "https://gitlab.com/o/r.git",
            "https://github.com/o",
            "https://github.com/o/r/../../x",
            "https://github.com//r",
            "/srv/git/wheel.git",
        ] {
            assert_eq!(github_repo_from_url(url), None, "{url}");
        }
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    /// A GitHub API stand-in that records what it was asked.
    async fn github(status: u16, body: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
        use axum::{extract::Request, response::IntoResponse, routing::get, Router};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let app = Router::new().route(
            "/repos/{owner}/{name}/commits/{sha}/check-runs",
            get(move |req: Request| {
                let log = log.clone();
                async move {
                    let auth = req
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    log.lock().unwrap().push(format!("{} {auth}", req.uri()));
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        [("content-type", "application/json")],
                        body,
                    )
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        (base, seen)
    }

    fn gate(api: &str, token: Option<&str>) -> GithubChecks {
        GithubChecks::new(Some("o/r".into()), token.map(str::to_string), required()).with_api(api)
    }

    #[tokio::test]
    async fn the_gate_asks_github_about_the_target_with_the_deployment_token() {
        let (api, seen) = github(
            200,
            r#"{"total_count":1,"check_runs":[{"name":"make check","status":"completed","conclusion":"success","app":{"slug":"github-actions"}}]}"#,
        )
        .await;
        assert_eq!(gate(&api, Some("tok")).verdict(SHA).await, Verdict::Green);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert!(
            seen[0].contains(&format!("/repos/o/r/commits/{SHA}/check-runs")),
            "{seen:?}"
        );
        assert!(seen[0].ends_with("Bearer tok"), "{seen:?}");
    }

    /// Never silently skipped: with no token nothing is asked and nothing is green.
    #[tokio::test]
    async fn no_token_is_unverifiable_and_asks_nothing() {
        let (api, seen) = github(200, r#"{"total_count":0,"check_runs":[]}"#).await;
        let v = gate(&api, None).verdict(SHA).await;
        assert!(
            matches!(&v, Verdict::Unverifiable(why) if why.contains("WHEEL_UPDATE_GITHUB_TOKEN")),
            "{v:?}"
        );
        assert!(seen.lock().unwrap().is_empty());

        let no_repo = GithubChecks::new(None, Some("tok".into()), required()).with_api(&api);
        assert!(
            matches!(no_repo.verdict(SHA).await, Verdict::Unverifiable(w) if w.contains("WHEEL_UPDATE_GITHUB_REPO"))
        );
        assert!(matches!(
            gate(&api, Some("t")).verdict("HEAD~1").await,
            Verdict::Unverifiable(_)
        ));
    }

    #[tokio::test]
    async fn anything_but_a_clean_answer_is_unverifiable() {
        let (api, _) = github(403, r#"{"message":"rate limited"}"#).await;
        assert!(
            matches!(gate(&api, Some("t")).verdict(SHA).await, Verdict::Unverifiable(w) if w.contains("403"))
        );

        let (api, _) = github(200, "not json").await;
        assert!(
            matches!(gate(&api, Some("t")).verdict(SHA).await, Verdict::Unverifiable(w) if w.contains("parse"))
        );

        let (api, _) = github(200, r#"{"total_count":101,"check_runs":[]}"#).await;
        assert!(
            matches!(gate(&api, Some("t")).verdict(SHA).await, Verdict::Unverifiable(w) if w.contains("101"))
        );

        let dead = gate("http://127.0.0.1:1", Some("t")).verdict(SHA).await;
        assert!(matches!(dead, Verdict::Unverifiable(w) if w.contains("unreachable")));
    }
}
