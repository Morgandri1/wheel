// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The CI gate: is the target commit green on GitHub Actions?
//!
//! Only check-runs from the GitHub Actions app count. Commit statuses are
//! ignored entirely — any token with `repo:status` can post one — and so is a
//! check-run from any other app, since an app with `checks:write` could post a
//! green one for any commit (proposal T1c). Unverifiable is never green.
//!
//! **The token is optional.** `wheel` is a public repository, so an unauthenticated
//! `GET /repos/:repo/commits/:sha/check-runs` already answers this question — GitHub just
//! meters it at 60 requests/IP/hour instead of 5,000/token/hour. A token, when set, only raises
//! that ceiling (or reaches a private fork); it is never required to boot with auto-update on.
//! Two things make 60/hour workable for a gate that polls every `fetch_every` (minimum 60s):
//! conditional requests (`ETag`/`If-None-Match`, a free re-ask once the answer is cached) and
//! honouring GitHub's own backoff signal on a 403/429 instead of retrying into it.

use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const ACTIONS_APP: &str = "github-actions";
const PAGE: usize = 100;
/// Used when GitHub rate-limits us with no `Retry-After` and no
/// `X-RateLimit-Reset` to compute a wait from — should not happen in
/// practice, but a fixed floor beats hammering the endpoint.
const DEFAULT_BACKOFF: Duration = Duration::from_secs(60);

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

/// What one prior successful answer for a commit is worth keeping: the `ETag` GitHub sent with
/// it, so the next poll can ask "has this changed?" for free, and the verdict to answer with when
/// it hasn't.
struct Cached {
    etag: String,
    verdict: Verdict,
}

pub struct GithubChecks {
    client: reqwest::Client,
    api: String,
    repo: Option<String>,
    token: Option<String>,
    required: Vec<String>,
    cache: Mutex<HashMap<String, Cached>>,
    /// Set on a 403/429 to (the moment GitHub said to try again, how long that was from when we
    /// asked). Cleared on any answer that is not itself a rate limit. Checked before every
    /// request so a caller polling on a short interval backs off in memory rather than re-asking
    /// into the same limit. The message always reports the second element, not a live countdown
    /// recomputed from the first — GitHub's own number, stated once, rather than one that visibly
    /// shrinks (and, near the boundary, can round down a whole second) between two calls a few
    /// milliseconds apart.
    retry_not_before: Mutex<Option<(Instant, Duration)>>,
}

impl GithubChecks {
    pub fn new(repo: Option<String>, token: Option<String>, required: Vec<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api: "https://api.github.com".into(),
            repo,
            token,
            required,
            cache: Mutex::new(HashMap::new()),
            retry_not_before: Mutex::new(None),
        }
    }

    pub fn with_api(mut self, api: impl Into<String>) -> Self {
        self.api = api.into();
        self
    }

    /// `Some(original_wait)` while still inside a recorded backoff window, `None` once it has
    /// passed (and the window is not cleared here — the next real answer clears or replaces it).
    fn still_backing_off(&self) -> Option<Duration> {
        let guard = self
            .retry_not_before
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (until, wait) = (*guard)?;
        (until > Instant::now()).then_some(wait)
    }

    fn record_backoff(&self, headers: &reqwest::header::HeaderMap) -> Duration {
        let wait = retry_after(headers).unwrap_or(DEFAULT_BACKOFF);
        *self
            .retry_not_before
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((Instant::now() + wait, wait));
        wait
    }

    fn clear_backoff(&self) {
        *self
            .retry_not_before
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// How long to wait before asking again, from whichever of GitHub's two rate-limit headers is
/// present: `Retry-After` (seconds, the secondary/abuse limiter) takes priority over
/// `X-RateLimit-Reset` (a unix timestamp, the primary hourly limiter) because it is the more
/// specific signal when both happen to be set.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let secs =
        |name: &str| -> Option<u64> { headers.get(name)?.to_str().ok()?.trim().parse().ok() };
    if let Some(s) = secs("retry-after") {
        return Some(Duration::from_secs(s));
    }
    let reset = secs("x-ratelimit-reset")?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(Duration::from_secs(reset.saturating_sub(now)))
}

#[async_trait]
impl CiGate for GithubChecks {
    async fn verdict(&self, sha: &str) -> Verdict {
        let Some(repo) = &self.repo else {
            return Verdict::Unverifiable(format!(
                "origin is not a github.com repository and {} is unset",
                super::policy::ENV_GITHUB_REPO
            ));
        };
        if wheel_core::Sha::parse(sha).is_none() {
            return Verdict::Unverifiable(format!("{sha:?} is not a commit id"));
        }
        if let Some(wait) = self.still_backing_off() {
            return Verdict::Unverifiable(format!(
                "GitHub rate-limited a recent check; waiting {}s before asking again{}",
                wait.as_secs(),
                token_hint(&self.token),
            ));
        }

        let cached_etag = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(sha)
            .map(|c| c.etag.clone());

        let url = format!(
            "{}/repos/{repo}/commits/{sha}/check-runs?per_page={PAGE}",
            self.api
        );
        let mut req = self
            .client
            .get(&url)
            .header("accept", "application/vnd.github+json")
            .header("user-agent", "wheeld-auto-update")
            .header("x-github-api-version", "2022-11-28");
        // Anonymous is the default, not a fallback: `wheel` is public, and an unauthenticated
        // read of its check-runs is exactly as trustworthy as an authenticated one — the token
        // only buys quota (module doc comment).
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        if let Some(etag) = &cached_etag {
            req = req.header("if-none-match", etag.clone());
        }
        let resp = req.send().await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => return Verdict::Unverifiable(format!("GitHub unreachable: {e}")),
        };

        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            self.clear_backoff();
            let cached = self
                .cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(sha)
                .map(|c| c.verdict.clone());
            return cached.unwrap_or(Verdict::Pending);
        }
        if resp.status() == reqwest::StatusCode::FORBIDDEN
            || resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            let wait = self.record_backoff(resp.headers());
            return Verdict::Unverifiable(format!(
                "GitHub rate-limited this check (answered {}); waiting {}s before asking again{}",
                resp.status(),
                wait.as_secs(),
                token_hint(&self.token),
            ));
        }
        if !resp.status().is_success() {
            return Verdict::Unverifiable(format!("GitHub answered {}", resp.status()));
        }
        self.clear_backoff();
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let verdict = match resp.json::<Page>().await {
            Ok(page) if page.total_count > page.check_runs.len() => Verdict::Unverifiable(format!(
                "{} check runs on one commit is more than one page of {PAGE}",
                page.total_count
            )),
            Ok(page) => evaluate(&page.check_runs, &self.required),
            Err(e) => Verdict::Unverifiable(format!("GitHub's answer did not parse: {e}")),
        };
        // A `Pending` or `Unverifiable` verdict is not cached against the `ETag`: GitHub would
        // send the same `ETag` right up until the moment a run finishes, and caching one of
        // those would mean a `304` freezing the daemon on "still checking" past the point where
        // asking again would show green.
        if let (Some(etag), Verdict::Green | Verdict::Red(_)) = (etag, &verdict) {
            self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(
                sha.to_string(),
                Cached {
                    etag,
                    verdict: verdict.clone(),
                },
            );
        }
        verdict
    }
}

/// The part of an `Unverifiable` message that tells the operator a token would help — only when
/// one is not already set, so a deployment that already has one does not get told to add another.
fn token_hint(token: &Option<String>) -> String {
    if token.is_some() {
        String::new()
    } else {
        format!(
            " (set {} to raise the anonymous 60/hour limit)",
            super::policy::ENV_TOKEN
        )
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

    /// One scripted response: (status, extra headers, body).
    type Step = (u16, Vec<(&'static str, String)>, String);
    /// One request the stand-in received: (uri, `authorization`, `if-none-match`).
    type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

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

    /// A GitHub API stand-in that records what it was asked (path, `authorization`,
    /// `if-none-match`) and answers with the next entry in `script`, repeating the last one once
    /// `script` runs out — so a two-request test can give two different answers.
    async fn scripted(script: Vec<Step>) -> (String, Seen) {
        use axum::{extract::Request, response::IntoResponse, routing::get, Router};
        use std::collections::VecDeque;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let script = Arc::new(Mutex::new(VecDeque::from(script)));
        let app = Router::new().route(
            "/repos/{owner}/{name}/commits/{sha}/check-runs",
            get(move |req: Request| {
                let log = log.clone();
                let script = script.clone();
                async move {
                    let header = |name: &str| {
                        req.headers()
                            .get(name)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_string()
                    };
                    log.lock().unwrap().push((
                        req.uri().to_string(),
                        header("authorization"),
                        header("if-none-match"),
                    ));
                    let mut script = script.lock().unwrap();
                    let (status, headers, body) = if script.len() > 1 {
                        script.pop_front().unwrap()
                    } else {
                        script.front().cloned().unwrap()
                    };
                    let mut res =
                        (axum::http::StatusCode::from_u16(status).unwrap(), body).into_response();
                    res.headers_mut()
                        .insert("content-type", "application/json".parse().unwrap());
                    for (k, v) in headers {
                        res.headers_mut().insert(
                            axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                            v.parse().unwrap(),
                        );
                    }
                    res
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        (base, seen)
    }

    /// A GitHub API stand-in that always answers the same way.
    async fn github(status: u16, body: &'static str) -> (String, Seen) {
        scripted(vec![(status, vec![], body.to_string())]).await
    }

    /// How many requests a stand-in received.
    fn count(seen: &Seen) -> usize {
        seen.lock().unwrap().len()
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
            seen[0]
                .0
                .contains(&format!("/repos/o/r/commits/{SHA}/check-runs")),
            "{seen:?}"
        );
        assert!(seen[0].1.ends_with("Bearer tok"), "{seen:?}");
    }

    /// A public repo needs no token: an unauthenticated request still asks GitHub and still
    /// reaches a real verdict. This is the whole point of the change — auto-update must not be
    /// dead in the water on a deployment that never set `WHEEL_UPDATE_GITHUB_TOKEN`.
    #[tokio::test]
    async fn no_token_still_asks_anonymously_and_reaches_a_verdict() {
        let (api, seen) = github(
            200,
            r#"{"total_count":1,"check_runs":[{"name":"make check","status":"completed","conclusion":"success","app":{"slug":"github-actions"}}]}"#,
        )
        .await;
        assert_eq!(gate(&api, None).verdict(SHA).await, Verdict::Green);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(
            seen[0].1.is_empty(),
            "no token means no authorization header, not a fabricated one: {seen:?}"
        );

        let no_repo = GithubChecks::new(None, None, required()).with_api(&api);
        assert!(
            matches!(no_repo.verdict(SHA).await, Verdict::Unverifiable(w) if w.contains("WHEEL_UPDATE_GITHUB_REPO"))
        );
        assert!(matches!(
            gate(&api, None).verdict("HEAD~1").await,
            Verdict::Unverifiable(_)
        ));
    }

    #[tokio::test]
    async fn anything_but_a_clean_answer_is_unverifiable() {
        let (api, _) = github(500, r#"{"message":"internal error"}"#).await;
        assert!(
            matches!(gate(&api, Some("t")).verdict(SHA).await, Verdict::Unverifiable(w) if w.contains("500"))
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

    /// The rate-limit path: a 403 with `Retry-After` is unverifiable (fail-closed — never green
    /// because the gate could not be asked), names why, and a second call inside the backoff
    /// window does not make a second request at all.
    #[tokio::test]
    async fn a_rate_limited_answer_is_unverifiable_and_the_gate_backs_off_in_memory() {
        let (api, seen) = scripted(vec![(
            403,
            vec![("retry-after", "120".to_string())],
            r#"{"message":"API rate limit exceeded"}"#.to_string(),
        )])
        .await;
        let g = gate(&api, None);

        let first = g.verdict(SHA).await;
        assert!(
            matches!(&first, Verdict::Unverifiable(w) if w.contains("rate") && w.contains("120") && w.contains(super::super::policy::ENV_TOKEN)),
            "{first:?}"
        );

        let second = g.verdict(SHA).await;
        assert!(
            matches!(&second, Verdict::Unverifiable(w) if w.contains("120")),
            "{second:?}"
        );
        assert_eq!(
            count(&seen),
            1,
            "a second call inside the backoff window must not hit the network again"
        );

        // A token already set means the hint would be redundant.
        let (api2, _) = scripted(vec![(
            403,
            vec![("retry-after", "5".to_string())],
            "{}".to_string(),
        )])
        .await;
        let with_token = gate(&api2, Some("tok")).verdict(SHA).await;
        assert!(
            matches!(&with_token, Verdict::Unverifiable(w) if !w.contains(super::super::policy::ENV_TOKEN)),
            "{with_token:?}"
        );
    }

    /// The reset-timestamp form of the header (the primary, hourly limiter) works too, and a
    /// `429` is treated the same as a `403` — GitHub uses either for rate limiting.
    #[tokio::test]
    async fn the_reset_timestamp_header_and_429_are_both_understood() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let (api, _) = scripted(vec![(
            429,
            vec![("x-ratelimit-reset", (now + 30).to_string())],
            "{}".to_string(),
        )])
        .await;
        let v = gate(&api, None).verdict(SHA).await;
        assert!(
            matches!(&v, Verdict::Unverifiable(w) if w.contains("30") || w.contains("29")),
            "{v:?}"
        );
    }

    /// Conditional requests: a cached `ETag` is sent back as `If-None-Match`, and a `304` reuses
    /// the last verdict rather than costing a fresh evaluation — the mechanism that makes polling
    /// on a short interval affordable within GitHub's anonymous 60/hour ceiling.
    #[tokio::test]
    async fn a_304_against_a_cached_etag_reuses_the_last_verdict() {
        let (api, seen) = scripted(vec![
            (
                200,
                vec![("etag", "\"v1\"".to_string())],
                r#"{"total_count":1,"check_runs":[{"name":"make check","status":"completed","conclusion":"success","app":{"slug":"github-actions"}}]}"#.to_string(),
            ),
            (304, vec![], String::new()),
        ])
        .await;
        let g = gate(&api, None);
        assert_eq!(g.verdict(SHA).await, Verdict::Green);
        assert_eq!(g.verdict(SHA).await, Verdict::Green);

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[0].2, "", "no cached etag on the first ask: {seen:?}");
        assert_eq!(
            seen[1].2, "\"v1\"",
            "the second ask must send back the etag the first answer carried: {seen:?}"
        );
    }

    /// A `Pending` verdict is never cached against its `ETag`: GitHub can serve the same `ETag`
    /// right up until a run finishes, and caching it would freeze the daemon on "still checking"
    /// past the moment asking again would show green.
    #[tokio::test]
    async fn a_pending_verdict_is_not_cached_even_with_an_etag() {
        let (api, seen) = scripted(vec![
            (
                200,
                vec![("etag", "\"v1\"".to_string())],
                r#"{"total_count":1,"check_runs":[{"name":"make check","status":"in_progress","app":{"slug":"github-actions"}}]}"#.to_string(),
            ),
            (
                200,
                vec![("etag", "\"v2\"".to_string())],
                r#"{"total_count":1,"check_runs":[{"name":"make check","status":"completed","conclusion":"success","app":{"slug":"github-actions"}}]}"#.to_string(),
            ),
        ])
        .await;
        let g = gate(&api, None);
        assert_eq!(g.verdict(SHA).await, Verdict::Pending);
        assert_eq!(g.verdict(SHA).await, Verdict::Green);

        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen[1].2, "",
            "a pending answer must not be sent back as an if-none-match: {seen:?}"
        );
    }

    /// The required-checks-missing path, through the real (anonymous) HTTP round trip rather
    /// than the pure `evaluate` unit tests above — this is what the gate actually returns to the
    /// update daemon when a deployment renamed or dropped its gate job.
    #[tokio::test]
    async fn a_missing_required_check_is_red_over_the_wire_with_no_token() {
        let (api, _) = github(
            200,
            r#"{"total_count":1,"check_runs":[{"name":"lint","status":"completed","conclusion":"success","app":{"slug":"github-actions"}}]}"#,
        )
        .await;
        let v = gate(&api, None).verdict(SHA).await;
        assert!(
            matches!(&v, Verdict::Red(w) if w.contains("make check")),
            "{v:?}"
        );
    }
}
