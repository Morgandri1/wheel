// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `wheel-docker-proxy` — the only process that holds the real docker socket.
//!
//! Configuration is environment only, and every value that decides what a tenant container may be
//! has no default: a missing one is a boot failure, not a permissive guess.
//!
//!   DOCKER_PROXY_LISTEN         where the sandbox host connects (unix socket path)
//!   DOCKER_PROXY_UPSTREAM       the real socket (default /var/run/docker.sock)
//!   DOCKER_PROXY_SOCKET_MODE    octal mode of the listen socket (default 0660)
//!   DOCKER_PROXY_SOCKET_OWNER   `uid:gid` to chown it to (default: leave as created)
//!   ENGINE_IMAGE, DOCKER_NETWORK, ENGINE_PORT (default 7000),
//!   DOCKER_PROXY_RUN_ROOT       host directory holding ONLY per-project socket directories; set, engines
//!                               listen on a unix socket there instead of TCP (unset: the TCP form)
//!   DOCKER_PROXY_MAX_PROJECTS   most project containers/volumes that may exist (default 200; 0 = no ceiling)
//!   CONTAINER_MEMORY_MB, CONTAINER_CPUS, CONTAINER_PIDS_LIMIT — the EXACT limits a tenant
//!   container may carry; give the sandbox host the same values.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use wheel_host::docker_proxy::{
    policy::{Policy, RunRoot},
    serve, Proxy,
};

fn required(key: &str) -> Result<String> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => bail!("{key} must be set"),
    }
}

fn or<T: std::str::FromStr>(key: &str, default: T) -> Result<T> {
    match std::env::var(key) {
        Ok(v) => v
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("{key} is not valid")),
        Err(_) => Ok(default),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let policy = Policy {
        image: required("ENGINE_IMAGE")?,
        network: required("DOCKER_NETWORK")?,
        memory: or("CONTAINER_MEMORY_MB", 1024i64)? * 1024 * 1024,
        nano_cpus: (or("CONTAINER_CPUS", 1.0f64)? * 1e9) as i64,
        pids_limit: or("CONTAINER_PIDS_LIMIT", 512i64)?,
        engine_port: or("ENGINE_PORT", 7000u16)?,
        run_root: match std::env::var("DOCKER_PROXY_RUN_ROOT") {
            Ok(v) if !v.trim().is_empty() => Some(
                RunRoot::new(v.trim())
                    .map_err(|e| anyhow::anyhow!("DOCKER_PROXY_RUN_ROOT: {e}"))?,
            ),
            _ => None,
        },
        max_projects: or("DOCKER_PROXY_MAX_PROJECTS", 200usize)?,
    };
    let listen = PathBuf::from(required("DOCKER_PROXY_LISTEN")?);
    let upstream = PathBuf::from(
        std::env::var("DOCKER_PROXY_UPSTREAM").unwrap_or_else(|_| "/var/run/docker.sock".into()),
    );
    let mode = u32::from_str_radix(
        std::env::var("DOCKER_PROXY_SOCKET_MODE")
            .unwrap_or_else(|_| "660".into())
            .trim_start_matches('0'),
        8,
    )
    .context("DOCKER_PROXY_SOCKET_MODE must be octal")?;
    let owner = match std::env::var("DOCKER_PROXY_SOCKET_OWNER") {
        Ok(v) => {
            let (u, g) = v
                .split_once(':')
                .context("DOCKER_PROXY_SOCKET_OWNER is uid:gid")?;
            Some((u.parse()?, g.parse()?))
        }
        Err(_) => None,
    };

    tracing::info!(listen = %listen.display(), upstream = %upstream.display(), image = %policy.image, "wheel-docker-proxy starting");
    serve(Proxy::new(policy, upstream), &listen, mode, owner).await
}
