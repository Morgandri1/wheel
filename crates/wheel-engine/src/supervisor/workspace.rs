// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Materialising an agent's working copies (§3e `workspaces`, tickets A9/A8).
//!
//! Until this existed, agents IMPROVISED. Each one ran its own `git clone`, and
//! the shortest form that works — `https://<token>@github.com/...` — wrote a
//! live credential into `.git/config` on the production volume (finding 036).
//! It also cost a full copy of the repository per agent: a checkout plus its
//! build tree measured 1.9 GB, and three agents filled a 4.6 GB volume.
//!
//! Both are properties of HOW the checkout is made, so both are fixed here.
//!
//! ONE OBJECT STORE PER REPOSITORY, per project. Every agent wired to the same
//! repo gets a `git worktree` off that one store, which shares the object
//! database and costs kilobytes rather than gigabytes. A8 is therefore not a
//! mode of this — it is the only thing it does. There is no one-clone-per-agent
//! path to migrate away from later.
//!
//! DETACHED, always. `git worktree` refuses to check out one branch in two
//! trees, so two agents sharing a repo cannot both hold `main`. Checking out
//! detached at the ref gives each the same commit and no branch, and agents
//! make their own branch when they want one — which is what they already do,
//! since every PR they open comes from one.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use std::time::Duration;
use wheel_core::{GitSource, Workspace};

use super::git_creds;

/// Where the shared object store for a repository lives.
///
/// Keyed by a hash of the URL rather than by anything human-chosen: two
/// workspaces naming the same repository must land on the SAME store or the
/// sharing does not happen, and a name is something two configs can disagree
/// about while meaning one repo.
fn store_dir(data_dir: &Path, url: &str) -> PathBuf {
    // `wheel_core`'s own sha256, not a new dependency: this is a directory
    // name, and A10 makes a crate a thing somebody has to argue for.
    let hex: String = wheel_core::sha256_hex(url.as_bytes())
        .chars()
        .take(16)
        .collect();
    let readable: String = url
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .trim_end_matches(".git")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(32)
        .collect();
    data_dir.join("repos").join(format!("{readable}-{hex}"))
}

/// A clone of a large repository over a slow link is legitimately slow, so this
/// is generous. It exists to bound a HANG, not to police duration.
const GIT_TIMEOUT: Duration = Duration::from_secs(600);

/// Local, no network: if these take a minute something is wrong.
const GIT_QUICK_TIMEOUT: Duration = Duration::from_secs(60);

/// Run a child to completion, or kill it and fail.
///
/// `Command::output` waits forever. A `git clone` that never returns — an
/// unreachable host, a credential prompt we did not suppress, a half-open
/// connection — would hang the agent's spawn with no error and no timeout, and
/// the agent would sit in `starting` looking like the wedged-start bug rather
/// than like a stuck clone.
async fn run_capped(
    mut cmd: tokio::process::Command,
    limit: Duration,
    what: &str,
) -> Result<std::process::Output> {
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Dropping the future on timeout must actually kill the child; without
        // this the process outlives us and keeps holding the repository lock.
        .kill_on_drop(true);
    let child = cmd.spawn().with_context(|| format!("spawning {what}"))?;
    match tokio::time::timeout(limit, child.wait_with_output()).await {
        Ok(out) => out.with_context(|| format!("running {what}")),
        Err(_) => bail!("{what} exceeded {}s and was killed", limit.as_secs()),
    }
}

/// Run a git command with credentials supplied OUT OF BAND.
///
/// The token reaches git through the askpass helper's environment. Never in the
/// URL, where it lands in `.git/config` and survives the process, and never in
/// argv, which `/proc` publishes to every process of the same uid — which, until
/// per-node uids land (§3e), is every other agent in the project.
async fn git(run_dir: &Path, token: Option<&str>, args: &[&str], cwd: Option<&Path>) -> Result<()> {
    // `child_command`, not a Command of our own: it clears the environment and
    // re-inherits only the allowlist, and a second env policy for git is
    // exactly the drift that gate exists to prevent.
    let mut cmd = super::child_command("git");
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if let Some(token) = token {
        let askpass = git_creds::write_askpass(run_dir)?;
        cmd.env("GIT_ASKPASS", &askpass);
        cmd.env("GITHUB_TOKEN", token);
    }
    let what = format!("git {}", args.join(" "));
    let out = run_capped(cmd, GIT_TIMEOUT, &what).await?;
    if !out.status.success() {
        // The token is never in argv or the URL, so this is safe to surface —
        // but the message is the agent's to read, so it gets the reason and not
        // the environment.
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

/// Ensure the shared object store for `src` exists and is current.
async fn ensure_store(
    data_dir: &Path,
    run_dir: &Path,
    src: &GitSource,
    token: Option<&str>,
) -> Result<PathBuf> {
    let store = store_dir(data_dir, &src.url);
    if store.join("HEAD").exists() {
        // The directory is named from a hash of the URL, so this should always
        // hold — but if it ever did not, the agent would silently get somebody
        // else's repository, which is worse than a failed clone. Cheap to check
        // and the failure it prevents is unreadable.
        let mut probe = super::child_command("git");
        probe.args([
            "-C",
            &store.to_string_lossy(),
            "remote",
            "get-url",
            "origin",
        ]);
        if let Ok(out) = run_capped(probe, GIT_QUICK_TIMEOUT, "git remote get-url origin").await {
            let found = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !found.is_empty() && found != src.url {
                bail!(
                    "the shared clone at {} is {found}, not {}; refusing to hand an agent the \
                     wrong repository",
                    store.display(),
                    src.url
                );
            }
        }
        // A fetch failure is not fatal: an existing store can still serve a
        // worktree at a ref it already has, and refusing to start an agent
        // because a remote was briefly unreachable is a worse failure than a
        // slightly stale checkout.
        if let Err(e) = git(
            run_dir,
            token,
            &["-C", &store.to_string_lossy(), "fetch", "--prune", "origin"],
            None,
        )
        .await
        {
            tracing::warn!(error = %e, store = %store.display(), "could not refresh the shared clone; using what is on disk");
        }
        return Ok(store);
    }
    if let Some(parent) = store.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // `--bare`: the store is an object database, never a checkout. Nothing
    // works in it, so it cannot accumulate a build tree — which is what filled
    // the volume when every agent had its own full clone.
    git(
        run_dir,
        token,
        &["clone", "--bare", &src.url, &store.to_string_lossy()],
        None,
    )
    .await?;
    Ok(store)
}

/// Materialise one workspace and return the directory the agent will see.
async fn materialise_one(
    data_dir: &Path,
    ws_root: &Path,
    run_dir: &Path,
    ws: &Workspace,
    token: Option<&str>,
) -> Result<PathBuf> {
    let rel = wheel_core::normalize_chest_key(&ws.path)
        .map_err(|_| anyhow::anyhow!("workspace path {:?} is not a safe relative path", ws.path))?;
    let dest = ws_root.join(&rel);

    let Some(src) = &ws.git else {
        std::fs::create_dir_all(&dest)?;
        return Ok(dest);
    };

    // Already a checkout: leave it alone. The agent has work in there, and
    // re-materialising over it would discard uncommitted changes — the thing
    // this engine must never do to somebody else's tree.
    if dest.join(".git").exists() {
        return Ok(dest);
    }

    let store = ensure_store(data_dir, run_dir, src, token).await?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Stale administrative entries from a worktree whose directory was removed
    // out from under git would make `add` refuse.
    let _ = git(
        run_dir,
        token,
        &["-C", &store.to_string_lossy(), "worktree", "prune"],
        None,
    )
    .await;

    let git_ref = src.git_ref.clone().unwrap_or_else(|| "HEAD".to_string());
    git(
        run_dir,
        token,
        &[
            "-C",
            &store.to_string_lossy(),
            "worktree",
            "add",
            "--detach",
            &dest.to_string_lossy(),
            &git_ref,
        ],
        None,
    )
    .await?;
    Ok(dest)
}

/// The outcome of materialising an agent's workspaces.
///
/// `failures` is carried out rather than only logged because the engine log is
/// not where anyone looks. A workspace that failed to clone leaves the agent
/// running with a missing directory, which presents as "the agent did something
/// odd" — the caller puts these on the agent's own log stream so the reason is
/// visible next to the behaviour it explains.
pub struct Materialised {
    pub cwd: Option<PathBuf>,
    pub failures: Vec<String>,
}

/// Materialise every workspace an agent declares. Returns the child's cwd —
/// the first workspace, per §3e.
pub async fn materialise(
    data_dir: &Path,
    ws_root: &Path,
    run_dir: &Path,
    workspaces: &[Workspace],
    token: Option<&str>,
) -> Result<Materialised> {
    let mut cwd = None;
    let mut failures = Vec::new();
    for ws in workspaces {
        match materialise_one(data_dir, ws_root, run_dir, ws, token).await {
            Ok(dir) => {
                if cwd.is_none() {
                    cwd = Some(dir);
                }
            }
            // One unusable workspace must not stop the agent: it may have three
            // and need two. The agent finds the directory missing and can say
            // so, which is more useful than a start that never happens.
            Err(e) => {
                tracing::error!(
                    path = %ws.path,
                    error = %e,
                    "could not materialise this workspace; the agent starts without it"
                );
                failures.push(format!("workspace {:?}: {e}", ws.path));
            }
        }
    }
    Ok(Materialised { cwd, failures })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_repo(at: &Path) {
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(at)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .output()
                .unwrap();
            assert!(
                ok.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&ok.stderr)
            );
        };
        std::fs::create_dir_all(at).unwrap();
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(at.join("README"), "hello").unwrap();
        run(&["add", "README"]);
        run(&["commit", "-qm", "first"]);
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wheel-ws-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A8, which is the whole design rather than a later optimisation: two
    /// agents on one repository share ONE object store and cost a worktree
    /// each. Measured today, the alternative was 1.9 GB per agent and three
    /// agents filling a 4.6 GB volume.
    #[tokio::test]
    async fn two_agents_on_one_repo_share_a_single_object_store() {
        let root = scratch("shared");
        let origin = root.join("origin");
        seed_repo(&origin);
        let url = format!("file://{}", origin.display());

        let data = root.join("data");
        let run = root.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let ws = vec![Workspace {
            path: "wheel".into(),
            git: Some(GitSource {
                url: url.clone(),
                git_ref: None,
                vault_ref: None,
            }),
        }];

        // A run dir EACH, because that is how the engine calls this — it is
        // per node. Sharing one here made the test blind to a store keyed on
        // anything agent-specific, which is precisely the regression it exists
        // to catch.
        let run_a = run.join("alice");
        let run_b = run.join("bob");
        std::fs::create_dir_all(&run_a).unwrap();
        std::fs::create_dir_all(&run_b).unwrap();
        let a = materialise(&data, &data.join("ws/alice"), &run_a, &ws, None)
            .await
            .unwrap()
            .cwd
            .unwrap();
        let b = materialise(&data, &data.join("ws/bob"), &run_b, &ws, None)
            .await
            .unwrap()
            .cwd
            .unwrap();

        assert!(a.join("README").exists(), "alice got a real checkout");
        assert!(b.join("README").exists(), "bob got a real checkout");
        assert_ne!(a, b, "each agent works in its own directory");

        let stores: Vec<_> = std::fs::read_dir(data.join("repos"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(
            stores.len(),
            1,
            "one repository must mean one object store, however many agents check it out"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Finding 036, asserted where it happened: after a clone there must be no
    /// credential anywhere in the tree — not in `.git/config`, not in a remote
    /// URL, not in a leftover helper file.
    #[tokio::test]
    async fn a_materialised_workspace_has_no_credential_on_disk() {
        let root = scratch("nocreds");
        let origin = root.join("origin");
        seed_repo(&origin);
        let url = format!("file://{}", origin.display());

        let data = root.join("data");
        let run = root.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let ws = vec![Workspace {
            path: "repo".into(),
            git: Some(GitSource {
                url,
                git_ref: None,
                vault_ref: None,
            }),
        }];

        let token = "ghp_thisisaplantedcredential000000000000";
        materialise(&data, &data.join("ws/a"), &run, &ws, Some(token))
            .await
            .unwrap();

        // Planted in the production shape, so a detector that finds nothing is
        // failing to look rather than finding nothing to find.
        let mut hits = Vec::new();
        fn walk(dir: &Path, token: &str, hits: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, token, hits);
                } else if std::fs::read(&p)
                    .map(|b| String::from_utf8_lossy(&b).contains(token))
                    .unwrap_or(false)
                {
                    hits.push(p.display().to_string());
                }
            }
        }
        walk(&data, token, &mut hits);
        walk(&run, token, &mut hits);
        assert!(hits.is_empty(), "the token reached disk at: {hits:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Two agents cannot both hold one branch — `git worktree` refuses it — so
    /// checkouts are detached. If this ever became a branch checkout, the
    /// SECOND agent on a repo would fail to start.
    #[tokio::test]
    async fn a_checkout_holds_no_branch_so_a_second_agent_can_have_one_too() {
        let root = scratch("detached");
        let origin = root.join("origin");
        seed_repo(&origin);
        let url = format!("file://{}", origin.display());
        let data = root.join("data");
        let run = root.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let ws = vec![Workspace {
            path: "r".into(),
            git: Some(GitSource {
                url,
                git_ref: Some("main".into()),
                vault_ref: None,
            }),
        }];

        materialise(&data, &data.join("ws/one"), &run, &ws, None)
            .await
            .unwrap();
        let second = materialise(&data, &data.join("ws/two"), &run, &ws, None)
            .await
            .unwrap();
        assert!(
            second
                .cwd
                .map(|d| d.join("README").exists())
                .unwrap_or(false),
            "a second agent on the same ref must still get a checkout"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// An agent's uncommitted work is not ours to discard: re-materialising
    /// over an existing checkout must be a no-op.
    #[tokio::test]
    async fn re_materialising_leaves_an_existing_checkout_alone() {
        let root = scratch("idempotent");
        let origin = root.join("origin");
        seed_repo(&origin);
        let url = format!("file://{}", origin.display());
        let data = root.join("data");
        let run = root.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let ws = vec![Workspace {
            path: "r".into(),
            git: Some(GitSource {
                url,
                git_ref: None,
                vault_ref: None,
            }),
        }];
        let ws_root = data.join("ws/a");

        let dir = materialise(&data, &ws_root, &run, &ws, None)
            .await
            .unwrap()
            .cwd
            .unwrap();
        std::fs::write(dir.join("WIP"), "half-finished work").unwrap();

        materialise(&data, &ws_root, &run, &ws, None).await.unwrap();
        assert!(
            dir.join("WIP").exists(),
            "re-materialising must not discard an agent's uncommitted work"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_child_that_outlives_its_limit_is_killed_and_reported() {
        let mut cmd = super::super::child_command("sleep");
        cmd.arg("30");
        let started = std::time::Instant::now();
        let err = run_capped(cmd, Duration::from_millis(300), "sleep 30")
            .await
            .expect_err("a child past its limit must fail, not block");

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "run_capped waited {:?} — it returned only when the child did, so nothing is bounded",
            started.elapsed()
        );
        let msg = err.to_string();
        assert!(
            msg.contains("sleep 30") && msg.contains("exceeded"),
            "the error must name the command and why it stopped, got: {msg}"
        );
    }

    #[tokio::test]
    async fn a_child_inside_its_limit_returns_its_output() {
        let mut cmd = super::super::child_command("echo");
        cmd.arg("hello");
        let out = run_capped(cmd, Duration::from_secs(30), "echo")
            .await
            .expect("a fast child must succeed");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
    }

    /// `kill_on_drop` is the whole reason a timeout is safe: without it the
    /// child outlives the future that gave up on it and keeps holding the
    /// repository lock, so the NEXT clone fails for a reason that names the
    /// wrong thing.
    #[tokio::test]
    async fn the_killed_child_is_actually_dead_not_merely_abandoned() {
        let marker = std::env::temp_dir().join(format!("wheel-capped-{}", std::process::id()));
        std::fs::remove_file(&marker).ok();

        let mut cmd = super::super::child_command("sh");
        cmd.arg("-c")
            .arg(format!("sleep 2; touch {}", marker.display()));
        let _ = run_capped(cmd, Duration::from_millis(200), "sh").await;

        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !marker.exists(),
            "the child ran to completion after we gave up on it — it was abandoned, not killed"
        );
        std::fs::remove_file(&marker).ok();
    }

    #[tokio::test]
    async fn a_workspace_that_cannot_be_cloned_is_reported_not_just_logged() {
        let root = std::env::temp_dir().join(format!("wheel-badws-{}", std::process::id()));
        let data = root.join("data");
        let run = data.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let ws = vec![Workspace {
            path: "r".into(),
            // A local path that does not exist: git fails immediately, so the
            // test neither reaches the network nor waits on a timeout.
            git: Some(GitSource {
                url: format!("file://{}", root.join("no-such-repo.git").display()),
                git_ref: None,
                vault_ref: None,
            }),
        }];

        let out = materialise(&data, &data.join("ws/a"), &run, &ws, None)
            .await
            .expect("a failed workspace is a reported outcome, not an error");

        assert!(
            out.cwd.is_none(),
            "nothing was materialised, so there is no cwd"
        );
        assert_eq!(out.failures.len(), 1, "the one failure must be carried out");
        assert!(
            out.failures[0].contains("\"r\""),
            "the failure must name which workspace it was, got: {}",
            out.failures[0]
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
