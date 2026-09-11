// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! A stop must reach the agents even while a client holds a request open.
//!
//! The HTTP server's graceful shutdown waits for open requests. Unbounded, one client that never
//! finished sending held `serve_until` open until whoever was waiting gave up and aborted it, and the
//! abort dropped the supervisor shutdown with it (review round 1, finding 7).

use std::time::Duration;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

#[tokio::test]
async fn a_half_sent_request_cannot_hold_the_engine_open() {
    let dir = std::path::PathBuf::from(format!(
        "/tmp/we-d-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(dir.join("data")).unwrap();
    let socket = dir.join("run").join("engine.sock");
    let cfg = wheel_engine::Config {
        project_id: Uuid::new_v4(),
        engine_secret: "serve-until-drain-secret".into(),
        vault_key: None,
        data_dir: dir.join("data"),
        listen: wheel_core::ListenAddr::Unix(socket.clone()),
        json_logs: false,
        tool_allow_hosts: Vec::new(),
        startup_deadline_secs: wheel_engine::config::DEFAULT_STARTUP_DEADLINE_SECS,
        harness_auth: Default::default(),
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let engine = tokio::spawn(wheel_engine::serve_until(cfg, async move {
        let _ = stopped.await;
    }));

    let mut held = None;
    for _ in 0..200 {
        if let Ok(stream) = tokio::net::UnixStream::connect(&socket).await {
            held = Some(stream);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let mut held = held.expect("the engine never listened");
    held.write_all(b"GET /v1/board HTTP/1.1\r\nHost: engine\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    stop.send(()).unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(15), engine).await;
    drop(held);
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        finished.is_ok(),
        "the engine did not finish stopping while a request was half-sent"
    );
}
