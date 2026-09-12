// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `POST /v1/scripts/:id/run` — run a Script node as the project owner, from
//! the board.
//!
//! §3's CLI grammar gives an agent `wheel run <script>` over a `read` wire,
//! but there was never an operator-authenticated equivalent: Web's script
//! inspector shipped a permanently-disabled Run button with a comment saying
//! exactly that (`web/src/components/inspector/script-panel.tsx`). This is
//! that route. It answers the owner directly — no wire to check, the same as
//! `POST /v1/tables/:id/query` needing none for the project's own owner.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use uuid::Uuid;
use wheel_core::{NodeConfig, NodeType, ScriptConfig, ScriptLanguage};

use super::{ApiError, ApiResult, AppState};
use crate::db::board;

/// Per-stream cap. There is no documented ceiling for a script's own output
/// (§3's "Read ceilings" only covers table/ls paging), so this picks a number
/// in the same spirit as the tool executor's 5 MiB HTTP response cap (§3d#3):
/// generous for real output, small enough that a script that never stops
/// writing cannot grow the engine's memory without bound.
const MAX_SCRIPT_OUTPUT: usize = 1_000_000;

#[derive(Debug, Default, Deserialize)]
pub struct RunBody {
    #[serde(default)]
    pub args: Vec<String>,
}

fn require_script(s: &AppState, id: Uuid) -> ApiResult<(wheel_core::Node, ScriptConfig)> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let node = board::get(&conn, id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(id.to_string()))?;
    match node.config.clone() {
        NodeConfig::Script(cfg) => Ok((node, cfg)),
        _ => Err(ApiError::invalid(format!(
            "{} is a {} node, not a script",
            node.name,
            node.node_type()
        ))),
    }
}

/// `POST /v1/scripts/:id/run` — write the node's CURRENT `config.source` to
/// its run directory and execute it, so the response always reflects what the
/// inspector shows, not whatever was last saved to disk.
pub async fn run(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    body: Option<Json<RunBody>>,
) -> ApiResult<Json<serde_json::Value>> {
    // docs/proposals/script-execution-scope.md: PM's ruling is that per-node uid
    // isolation (F007) is a precondition of RUNNING a script, not later
    // hardening -- every child on a project shares one uid today, so a script
    // can read every sibling node's token file and reach whatever the host's
    // network can, with no SSRF policy of its own. Off by default; a
    // deployment opts in with `WHEEL_SCRIPT_EXEC=1` once that gate closes.
    //
    // This flag alone is discipline, not enforcement (ADVERSARY review of this
    // PR): nothing stops a future deploy, a copied `.env`, or someone who has
    // forgotten why it is off from flipping it on a still-single-uid box, and
    // nothing here would catch that. `execute` below checks the fact that
    // actually matters -- whether the spawned child is PROVABLY isolated --
    // against reality, not against this flag's say-so.
    if !s.cfg.script_execution_enabled {
        return Err(ApiError::config(
            "script execution is disabled on this deployment pending per-node isolation \
             (F007) and an adversary egress review -- see docs/proposals/script-execution-scope.md",
        ));
    }
    let (node, cfg) = require_script(&s, id)?;
    debug_assert_eq!(node.node_type(), NodeType::Script);
    let args = body.map(|Json(b)| b.args).unwrap_or_default();
    execute(&s, id, &cfg, &args, true).await
}

/// The engine's own real uid does not survive `child_command`'s `env_clear`,
/// so this is read once per call from the OS rather than assumed to be
/// whatever the process started as.
fn own_real_uid() -> u32 {
    // SAFETY: getuid takes no arguments and cannot fail.
    unsafe { libc::getuid() }
}

/// A spawned child's real uid, read from procfs. `None` when it cannot be
/// determined -- a race with the child exiting, a non-Linux host, anything --
/// which [`uids_prove_isolation`] treats as UNPROVEN, not as isolated.
#[cfg(target_os = "linux")]
fn real_uid_of(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    // "Uid:\t<real>\t<effective>\t<saved>\t<filesystem>"
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Procfs does not exist here, so a child's real uid can never be read --
/// which is the correct, honest answer: this route cannot prove isolation on
/// a platform it has no way to inspect, so it must refuse. Production always
/// runs this engine under Linux/Docker (§5b); this arm exists so a macOS
/// `cargo build`/`cargo test` still compiles rather than needing `cfg(test)`
/// everywhere this is used.
#[cfg(not(target_os = "linux"))]
fn real_uid_of(_pid: u32) -> Option<u32> {
    None
}

/// The one fact that actually has to be true before this engine runs
/// untrusted, agent-or-operator-authored code: the child's real uid differs
/// from the engine's own. A pure comparison over plain `u32`s (rather than a
/// function that spawns something) so the DECISION is unit-testable without
/// a privileged process to observe -- see the tests below.
///
/// `child` is `None` when it could not be determined, which counts as
/// UNPROVEN and therefore refused: a fact this engine cannot verify is not
/// one it may act as though it verified.
///
/// Under today's reality -- nothing in this engine calls setuid/setgid or
/// `pre_exec` anywhere (F007 is unbuilt) -- `child` is always `Some(own)`,
/// so this always returns `false` and [`execute`] always refuses, regardless
/// of `WHEEL_SCRIPT_EXEC`. That is deliberate: the check does not need F007's
/// code to exist to do its job, and it will start passing correctly, with no
/// further change here, the day a real per-node uid drop lands and actually
/// changes what a spawned child's uid is.
fn uids_prove_isolation(own: u32, child: Option<u32>) -> bool {
    child.is_some_and(|uid| uid != own)
}

/// `enforce_isolation` exists ONLY so the tests below can exercise the spawn/capture/timeout
/// mechanics on their own -- every real caller is [`run`], which always passes `true`, hardcoded
/// at its one call site above. There is no HTTP-reachable, wire-reachable, or CLI-reachable path
/// that can pass `false`; the parameter is private to this module and this file is the only place
/// that ever calls this function.
async fn execute(
    s: &AppState,
    id: Uuid,
    cfg: &ScriptConfig,
    args: &[String],
    enforce_isolation: bool,
) -> ApiResult<Json<serde_json::Value>> {
    let dir = s
        .cfg
        .scripts_dir()
        .join(id.to_string())
        .join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir)
        .map_err(|e| ApiError::internal(format!("creating the script's run directory: {e}")))?;
    // 0700: the script's own source and its captured output sit here briefly, and this
    // directory is one setuid drop away from being readable by an equally-isolated sibling
    // that happens to share a supplementary group -- the same reasoning as the OAuth renewal
    // scratch dir (`supervisor/refresh.rs`) right next to this code. Inert today (nothing else
    // can reach `/data/scripts` under the single uid this runs as either), cheap now, and the
    // exact thing F007's completion checklist would otherwise have to rediscover.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
            let _ = std::fs::remove_dir_all(&dir);
            ApiError::internal(format!("locking down the script's run directory: {e}"))
        })?;
    }
    let file = dir.join(cfg.language.main_file());
    let write_result = std::fs::write(&file, &cfg.source);
    if let Err(e) = write_result {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(ApiError::internal(format!(
            "writing the script source: {e}"
        )));
    }

    let (program, mut argv): (&str, Vec<std::ffi::OsString>) = match cfg.language {
        ScriptLanguage::Python => ("python3", vec![file.clone().into()]),
        ScriptLanguage::Js => ("node", vec![file.clone().into()]),
        // Node 22 strips TypeScript types natively behind this flag rather
        // than needing a second interpreter (`ts-node`/`tsx`) in the image.
        ScriptLanguage::Ts => (
            "node",
            vec![
                "--no-warnings".into(),
                "--experimental-strip-types".into(),
                file.clone().into(),
            ],
        ),
    };
    argv.extend(args.iter().map(std::ffi::OsString::from));

    let mut cmd = crate::supervisor::child_command(program);
    cmd.args(&argv)
        .current_dir(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        // Its own process group, so a timeout reaches whatever the script itself started.
        .process_group(0);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "spawn_failed",
                format!("could not start {program}: {e}"),
            ));
        }
    };
    let pid = child.id();
    let pgid = pid.and_then(|pid| libc::pid_t::try_from(pid).ok());

    // ADVERSARY review of this PR: `WHEEL_SCRIPT_EXEC` is discipline, not enforcement. This is
    // the enforcement -- checked against the just-spawned child's ACTUAL uid, as early as
    // possible and before a byte of its output is read, so a deployment that flipped the flag
    // without per-node isolation actually in effect gets a clear refusal instead of an agent's
    // (or the board owner's) code quietly running with no isolation from its siblings.
    if enforce_isolation {
        let own = own_real_uid();
        let child_uid = pid.and_then(real_uid_of);
        if !uids_prove_isolation(own, child_uid) {
            if let Some(pgid) = pgid {
                // SAFETY: killpg only delivers a signal, to our own child's process group.
                unsafe { libc::killpg(pgid, libc::SIGKILL) };
            }
            let _ = child.wait().await;
            let _ = std::fs::remove_dir_all(&dir);
            return Err(ApiError::config(
                "script execution refused: this engine cannot prove the spawned child would run \
                 under a different uid than its own (per-node isolation, F007, is not yet in \
                 effect) -- see docs/proposals/script-execution-scope.md",
            ));
        }
    }

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let out_task = tokio::spawn(read_capped(stdout, MAX_SCRIPT_OUTPUT));
    let err_task = tokio::spawn(read_capped(stderr, MAX_SCRIPT_OUTPUT));

    let timeout = std::time::Duration::from_secs(cfg.timeout_secs() as u64);
    let waited = tokio::time::timeout(timeout, child.wait()).await;
    let timed_out = waited.is_err();
    if timed_out {
        // The leader alone: `kill_on_drop` only reaches the process we spawned, not whatever
        // it forked. The whole group is what §3's "engine kills a script at `timeout_secs`"
        // promise has to mean.
        if let Some(pgid) = pgid {
            // SAFETY: killpg only delivers a signal, to our own child's process group.
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        }
        let _ = child.wait().await;
    }
    let exit_code = match waited {
        Ok(Ok(status)) => status.code(),
        _ => None,
    };
    let (stdout_bytes, stdout_truncated) = out_task.await.unwrap_or_default();
    let (stderr_bytes, stderr_truncated) = err_task.await.unwrap_or_default();

    let _ = std::fs::remove_dir_all(&dir);

    Ok(Json(serde_json::json!({
        "stdout": String::from_utf8_lossy(&stdout_bytes),
        "stderr": String::from_utf8_lossy(&stderr_bytes),
        "exit_code": exit_code,
        "timed_out": timed_out,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
    })))
}

/// Drain a child's pipe to a capped buffer without deadlocking the child on a
/// full pipe: bytes past the cap are read and discarded rather than left
/// unread, which is what would stall a script whose output exceeds the cap
/// forever instead of letting it exit.
async fn read_capped(mut reader: impl tokio::io::AsyncRead + Unpin, cap: usize) -> (Vec<u8>, bool) {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if buf.len() < cap {
                    let take = (cap - buf.len()).min(n);
                    buf.extend_from_slice(&chunk[..take]);
                    if take < n {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
        }
    }
    (buf, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_state;

    fn script_node(s: &AppState, language: ScriptLanguage, source: &str, timeout: u32) -> Uuid {
        let node = wheel_core::Node::new(
            Uuid::new_v4(),
            "s".parse().unwrap(),
            wheel_core::Position::default(),
            NodeConfig::Script(ScriptConfig {
                language,
                source: source.to_string(),
                timeout_secs: Some(timeout),
            }),
        );
        let conn = s.db.lock().unwrap();
        board::create(&conn, &node).unwrap();
        node.id
    }

    fn script_cfg(language: ScriptLanguage, source: &str, timeout: u32) -> ScriptConfig {
        ScriptConfig {
            language,
            source: source.to_string(),
            timeout_secs: Some(timeout),
        }
    }

    // --- the uid isolation gate (ADVERSARY review of this PR) ------------------------------

    #[test]
    fn uids_prove_isolation_is_true_only_for_a_known_different_uid() {
        assert!(
            !uids_prove_isolation(1000, Some(1000)),
            "the same uid, however it was spawned, proves nothing"
        );
        assert!(
            uids_prove_isolation(1000, Some(1001)),
            "a genuinely different uid is what the check exists to find"
        );
        assert!(
            !uids_prove_isolation(1000, None),
            "an unreadable/undetermined child uid must be UNPROVEN, not assumed safe"
        );
    }

    /// The end-to-end proof this review asked for: under today's reality (nothing in this
    /// engine calls setuid/setgid -- F007 is unbuilt) every real, spawned child shares the
    /// engine's own uid, so `run` must refuse EVEN WITH THE FLAG ON. This is what makes the
    /// gate structural rather than a second flag someone could also forget to check: it is
    /// provably true in any environment this test runs in, including production, until a real
    /// uid drop lands and changes what `real_uid_of` actually reads.
    #[tokio::test]
    async fn run_refuses_a_real_script_even_with_the_flag_enabled_because_isolation_is_unproven() {
        let s = test_state();
        assert!(
            s.cfg.script_execution_enabled,
            "test_state() enables the flag; the isolation check must be what refuses this"
        );
        let id = script_node(&s, ScriptLanguage::Python, "print('should not run')", 10);
        let err = run(State(s), Path(id), None).await.unwrap_err();
        assert!(matches!(
            err,
            ApiError(StatusCode::SERVICE_UNAVAILABLE, "config", _)
        ));
    }

    /// docs/proposals/script-execution-scope.md's gate, as code: a deployment
    /// that has not opted in must refuse to run ANY script, not merely warn.
    #[tokio::test]
    async fn script_execution_is_refused_when_the_deployment_has_not_opted_in() {
        let mut s = test_state();
        s.cfg = std::sync::Arc::new(crate::config::Config {
            script_execution_enabled: false,
            ..(*s.cfg).clone()
        });
        let id = script_node(&s, ScriptLanguage::Python, "print('should not run')", 10);
        let err = run(State(s), Path(id), None).await.unwrap_err();
        assert!(matches!(
            err,
            ApiError(StatusCode::SERVICE_UNAVAILABLE, "config", _)
        ));
    }

    // --- execution mechanics -----------------------------------------------------------------
    //
    // These call `execute(..., false)` directly rather than going through `run`, because the
    // isolation gate above refuses every real child in any environment these tests run in
    // (there is no way to spawn a genuinely different-uid process without real privileges to
    // drop to). `false` is reachable ONLY from this test module -- `run`'s one call site
    // hardcodes `true` -- so this bypass cannot exist in a build that serves real traffic.
    // What these tests are FOR is the mechanics: does stdout/stderr/exit-code/timeout capture
    // actually work, independent of whether it is safe to reach in production.

    #[tokio::test]
    async fn a_python_script_returns_its_stdout_stderr_and_exit_code() {
        let s = test_state();
        let cfg = script_cfg(
            ScriptLanguage::Python,
            "import sys\nprint('out')\nprint('err', file=sys.stderr)\nsys.exit(3)\n",
            10,
        );
        let out = execute(&s, Uuid::new_v4(), &cfg, &[], false)
            .await
            .unwrap()
            .0;
        assert_eq!(out["stdout"], "out\n");
        assert_eq!(out["stderr"], "err\n");
        assert_eq!(out["exit_code"], 3);
        assert_eq!(out["timed_out"], false);
    }

    #[tokio::test]
    async fn args_reach_the_script() {
        let s = test_state();
        let cfg = script_cfg(
            ScriptLanguage::Python,
            "import sys\nprint(sys.argv[1:])\n",
            10,
        );
        let out = execute(
            &s,
            Uuid::new_v4(),
            &cfg,
            &["a".to_string(), "b".to_string()],
            false,
        )
        .await
        .unwrap()
        .0;
        assert_eq!(out["stdout"], "['a', 'b']\n");
    }

    #[tokio::test]
    async fn a_script_past_its_timeout_is_killed_and_reported() {
        let s = test_state();
        let cfg = script_cfg(ScriptLanguage::Python, "import time\ntime.sleep(30)\n", 1);
        let out = execute(&s, Uuid::new_v4(), &cfg, &[], false)
            .await
            .unwrap()
            .0;
        assert_eq!(out["timed_out"], true);
        assert!(out["exit_code"].is_null());
    }

    /// ADVERSARY review of this PR: the run directory holds the script's source and its
    /// captured output, and must be locked down the same way the OAuth renewal scratch dir
    /// is (`supervisor/refresh.rs`) rather than left at whatever `create_dir_all`'s default
    /// mode is. Caught by actually stat-ing it WHILE a script is still running, not by
    /// reading the code that sets it.
    #[tokio::test]
    async fn the_run_directory_is_locked_down_while_the_script_is_still_running() {
        use std::os::unix::fs::PermissionsExt;

        let s = test_state();
        let id = Uuid::new_v4();
        let cfg = script_cfg(ScriptLanguage::Python, "import time\ntime.sleep(2)\n", 10);
        let handle = tokio::spawn({
            let s = s.clone();
            async move { execute(&s, id, &cfg, &[], false).await }
        });

        let base = s.cfg.scripts_dir().join(id.to_string());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let dir = loop {
            if let Some(Ok(entry)) = std::fs::read_dir(&base).ok().and_then(|mut d| d.next()) {
                break entry.path();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the run directory never appeared"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };

        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "run directory must be 0700, was {mode:o}");

        let _ = handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_non_script_node_is_refused() {
        let s = test_state();
        let node = wheel_core::Node::new(
            Uuid::new_v4(),
            "c".parse().unwrap(),
            wheel_core::Position::default(),
            NodeConfig::Ctx(wheel_core::CtxConfig {
                markdown: String::new(),
            }),
        );
        let id = node.id;
        {
            let conn = s.db.lock().unwrap();
            board::create(&conn, &node).unwrap();
        }
        let err = run(State(s), Path(id), None).await.unwrap_err();
        assert!(matches!(
            err,
            ApiError(StatusCode::BAD_REQUEST, "invalid", _)
        ));
    }

    #[tokio::test]
    async fn a_missing_node_is_404() {
        let s = test_state();
        let err = run(State(s), Path(Uuid::new_v4()), None).await.unwrap_err();
        assert!(matches!(
            err,
            ApiError(StatusCode::NOT_FOUND, "not_found", _)
        ));
    }
}
