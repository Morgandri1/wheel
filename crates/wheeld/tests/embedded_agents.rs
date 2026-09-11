// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Stopping one project must stop its agents while the daemon keeps running.
//!
//! This is the orphan that happened in normal use, not only at exit: the embedded backend stopped
//! a project by aborting its engine task, the supervisor lived on in its own tasks, and the agent
//! went on running with nothing that could stop it. The engine runs in this very process, so this
//! test is the agent's parent and can also see a zombie: a child killed but never reaped still
//! exists, and fails the assertion.
//!
//! Its own test binary, because it sets PATH for the whole process.

mod common;

use std::time::Duration;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_project_stops_its_agents_and_everything_they_started() {
    let fake = common::fake_claude_dir();
    std::env::set_var("PATH", common::path_with(&fake));
    let pid_file = fake.join("grandchild.pid");

    let dir = std::path::PathBuf::from(format!(
        "/tmp/wd-a-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    let dir = wheeld::supervise::prepare_data_dir(&dir).unwrap();
    let keys = wheeld::supervise::Keys::load_or_create(&dir).unwrap();
    let host = wheeld::start_host(&dir, &keys).await.expect("host starts");
    let secret = std::env::var("WHEEL_HOST_SECRET").unwrap();
    let http = reqwest::Client::new();
    let project = Uuid::new_v4();
    let at = |path: &str| format!("{}/host/v1/projects/{project}{path}", host.url);

    let put = http
        .put(at(""))
        .bearer_auth(&secret)
        .json(&serde_json::json!({
            "engine_secret": "embedded-agents-engine-secret",
            "vault_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 200);
    let start = http
        .post(at("/start"))
        .bearer_auth(&secret)
        .send()
        .await
        .unwrap();
    assert_eq!(
        start.status(),
        200,
        "{}",
        start.text().await.unwrap_or_default()
    );

    let node: serde_json::Value = http
        .post(at("/engine/v1/nodes"))
        .bearer_auth(&secret)
        .json(&common::agent_node("worker"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let agent = node["id"].as_str().expect("an agent id").to_string();
    let started = http
        .post(at(&format!("/engine/v1/agents/{agent}/start")))
        .bearer_auth(&secret)
        .send()
        .await
        .unwrap();
    assert!(started.status().is_success());
    let sent = http
        .post(at(&format!("/engine/v1/agents/{agent}/send")))
        .bearer_auth(&secret)
        .json(&common::spawn_grandchild(&pid_file))
        .send()
        .await
        .unwrap();
    assert!(sent.status().is_success());

    let grandchild = {
        let pid_file = pid_file.clone();
        tokio::task::spawn_blocking(move || {
            common::wait_for_pid_file(&pid_file, Duration::from_secs(60))
        })
        .await
        .unwrap()
        .expect("the agent never ran its command: no grandchild pid")
    };
    let agents = common::children_matching(std::process::id(), &fake.display().to_string());
    assert_eq!(
        agents.len(),
        1,
        "expected one agent process, found {agents:?}"
    );

    let stop = http
        .post(at("/stop"))
        .bearer_auth(&secret)
        .send()
        .await
        .unwrap();
    assert_eq!(stop.status(), 200);

    let survivors: Vec<i32> = agents
        .iter()
        .copied()
        .chain([grandchild])
        .filter(|pid| !common::gone_within(*pid, Duration::from_secs(5)))
        .collect();
    for pid in &survivors {
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
    host.sandbox.shutdown_all().await;
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&fake).ok();
    assert!(
        survivors.is_empty(),
        "a stopped project left processes behind (or unreaped): {survivors:?}"
    );
}
