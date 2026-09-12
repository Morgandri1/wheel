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
    if !s.cfg.script_execution_enabled {
        return Err(ApiError::config(
            "script execution is disabled on this deployment pending per-node isolation \
             (F007) and an adversary egress review -- see docs/proposals/script-execution-scope.md",
        ));
    }
    let (node, cfg) = require_script(&s, id)?;
    debug_assert_eq!(node.node_type(), NodeType::Script);
    let args = body.map(|Json(b)| b.args).unwrap_or_default();
    execute(&s, id, &cfg, &args).await
}

async fn execute(
    s: &AppState,
    id: Uuid,
    cfg: &ScriptConfig,
    args: &[String],
) -> ApiResult<Json<serde_json::Value>> {
    let dir = s
        .cfg
        .scripts_dir()
        .join(id.to_string())
        .join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir)
        .map_err(|e| ApiError::internal(format!("creating the script's run directory: {e}")))?;
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
    let pgid = child.id().and_then(|pid| libc::pid_t::try_from(pid).ok());
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

    #[tokio::test]
    async fn a_python_script_returns_its_stdout_stderr_and_exit_code() {
        let s = test_state();
        let id = script_node(
            &s,
            ScriptLanguage::Python,
            "import sys\nprint('out')\nprint('err', file=sys.stderr)\nsys.exit(3)\n",
            10,
        );
        let out = run(State(s), Path(id), None).await.unwrap().0;
        assert_eq!(out["stdout"], "out\n");
        assert_eq!(out["stderr"], "err\n");
        assert_eq!(out["exit_code"], 3);
        assert_eq!(out["timed_out"], false);
    }

    #[tokio::test]
    async fn args_reach_the_script() {
        let s = test_state();
        let id = script_node(
            &s,
            ScriptLanguage::Python,
            "import sys\nprint(sys.argv[1:])\n",
            10,
        );
        let out = run(
            State(s),
            Path(id),
            Some(Json(RunBody {
                args: vec!["a".into(), "b".into()],
            })),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(out["stdout"], "['a', 'b']\n");
    }

    #[tokio::test]
    async fn a_script_past_its_timeout_is_killed_and_reported() {
        let s = test_state();
        let id = script_node(
            &s,
            ScriptLanguage::Python,
            "import time\ntime.sleep(30)\n",
            1,
        );
        let out = run(State(s), Path(id), None).await.unwrap().0;
        assert_eq!(out["timed_out"], true);
        assert!(out["exit_code"].is_null());
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
