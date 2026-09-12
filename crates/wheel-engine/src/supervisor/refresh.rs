// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Renewing vault-held Claude logins before they lapse
//! (docs/proposals/harness-oauth-refresh.md, design (b′)).
//!
//! One refresher per vault, and it is the only holder of the refresh token:
//! children run on the access token alone. That is what makes the rotation
//! race impossible rather than unlikely — a single-use refresh token has one
//! user — and it is why nothing an agent can write is ever read back into a
//! vault. The exchange itself is Claude Code's own (`oauth::refresh_via_cli`);
//! this module only decides when, serialises, checks what came back, and moves
//! running children onto the result.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;
use wheel_core::{AgentStatus, Timestamp};

use super::Supervisor;
use crate::{auth::OauthSession, db::board, oauth::RefreshFailure};

/// Renew when less than this is left. Long enough that a busy child reaches
/// the end of its turn — where it is moved onto the new token — well before
/// the old one lapses.
const DEFAULT_LEAD: Duration = Duration::from_secs(30 * 60);
/// How soon a renewal that failed for a reason that may pass is tried again.
const DEFAULT_RETRY: Duration = Duration::from_secs(5 * 60);

/// A failed renewal's warning starts with this, so a later success clears
/// exactly that warning from an agent's `last_error` and nothing else.
pub(crate) const WARNING_PREFIX: &str = "credential refresh failed";

/// Which login a child was started on: the vault, and the expiry the vault
/// recorded at the time. A different expiry now means it was renewed since.
pub(crate) type CredentialGen = (Uuid, Option<Timestamp>);

pub(crate) struct Broker {
    pub(crate) lead: Duration,
    pub(crate) retry: Duration,
    pub(crate) timeout: Duration,
    /// The CLI that performs the exchange; the harness's own when unset.
    pub(crate) program: Option<String>,
    locks: Mutex<HashMap<Uuid, Arc<AsyncMutex<()>>>>,
    failures: Mutex<HashMap<Uuid, String>>,
    timers: Mutex<HashMap<Uuid, u64>>,
    /// Exchanges attempted, for the engine log and for tests that have to
    /// prove N callers cost one exchange.
    pub(crate) exchanges: AtomicU64,
}

impl Default for Broker {
    fn default() -> Self {
        Self {
            lead: DEFAULT_LEAD,
            retry: DEFAULT_RETRY,
            timeout: crate::oauth::REFRESH_TIMEOUT,
            program: None,
            locks: Mutex::default(),
            failures: Mutex::default(),
            timers: Mutex::default(),
            exchanges: AtomicU64::new(0),
        }
    }
}

fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

impl Supervisor {
    /// A login was just saved to this vault by a sign-in: forget any earlier
    /// failure, schedule its renewal, and move running readers onto it.
    pub fn session_saved(self: &Arc<Self>, vault: Uuid) {
        self.broker.failures.lock().unwrap().remove(&vault);
        self.clear_warnings(vault);
        if let Some(at) = self.session_generation(vault) {
            self.arm_renewal(
                vault,
                (at.into_inner().unix_timestamp_nanos() / 1_000_000) as i64,
            );
        }
        self.spawn_adopt(vault);
    }

    fn lineage_lock(&self, vault: Uuid) -> Arc<AsyncMutex<()>> {
        self.broker
            .locks
            .lock()
            .unwrap()
            .entry(vault)
            .or_default()
            .clone()
    }

    /// The expiry the vault currently records for its login: the generation
    /// a child is compared against.
    pub(crate) fn session_generation(&self, vault: Uuid) -> Option<Timestamp> {
        let conn = self.db.lock().unwrap();
        crate::vault::expiry_of(&conn, vault, wheel_core::CLAUDE_OAUTH_SESSION)
            .ok()
            .flatten()
    }

    /// Was this child started on a login the vault has since renewed?
    pub(crate) fn credential_stale(&self, gen: Option<&CredentialGen>) -> bool {
        gen.is_some_and(|(vault, spawned_with)| self.session_generation(*vault) != *spawned_with)
    }

    /// Why the last renewal of this vault's login failed, while that is still
    /// the latest word on it.
    pub fn refresh_warning(&self, vault: Uuid) -> Option<String> {
        self.broker.failures.lock().unwrap().get(&vault).cloned()
    }

    /// Make sure the vault's login has more than the lead left, renewing it
    /// first if not, and return its expiry (ms).
    ///
    /// `renew_if_still` forces a renewal when the vault is still on exactly
    /// that generation — the one a child just failed on. Checked under the
    /// lineage lock, so N children failing together cost one exchange, not N.
    ///
    /// A renewal that fails while the current token is still good is not a
    /// refusal: the caller runs on what is there, and the failure is surfaced
    /// as a warning until it is too late to be one.
    pub(crate) async fn ensure_fresh(
        self: &Arc<Self>,
        vault: Uuid,
        renew_if_still: Option<Option<Timestamp>>,
    ) -> Result<i64, String> {
        if self.cfg.harness_auth == crate::config::HarnessAuthPolicy::ApiKeyOnly {
            return Err(
                "this project is api-key-only: a refreshable OAuth login is not \
                        permitted here, store an API key instead"
                    .into(),
            );
        }
        // Shutting down: renewing would spawn a CLI child the engine is about
        // to stop waiting for, and the next boot re-arms the renewal anyway.
        if self.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("the engine is shutting down".into());
        }
        let vk = self.vault_key().ok_or(super::NO_VAULT_KEY)?;
        let lock = self.lineage_lock(vault);
        let _held = lock.lock().await;

        let session = {
            let conn = self.db.lock().unwrap();
            crate::vault::get_session(&conn, vk, vault)
        }
        .map_err(|e| format!("the vault's stored login is unreadable: {e}"))?
        .ok_or("the vault no longer holds a login")?;
        let expires = session
            .expires_at()
            .ok_or("the vault's stored login records no expiry")?;
        let now = now_ms();
        let forced = renew_if_still.is_some_and(|g| g == self.session_generation(vault));
        if !forced && expires - now > self.broker.lead.as_millis() as i64 {
            // Nothing to do now, but something must be waiting to do it later:
            // every vault holding a login has a renewal pending, or a board
            // that simply never restarts would never schedule its first one.
            // Arming again supersedes the previous timer rather than adding to
            // it.
            self.arm_renewal(vault, expires);
            return Ok(expires);
        }
        if !session.is_refreshable() {
            return Err(
                "the vault's stored login cannot be renewed (no refresh token or scopes)".into(),
            );
        }

        match self.renew(vault, vk, &session).await {
            Ok(next) => {
                self.broker.failures.lock().unwrap().remove(&vault);
                self.clear_warnings(vault);
                self.arm_renewal(vault, next);
                self.spawn_adopt(vault);
                Ok(next)
            }
            Err(failure) => {
                let usable = expires > now;
                let warning = self.record_failure(vault, &failure, usable.then_some(expires));
                if usable && !failure.permanent {
                    self.arm_refresh_timer(vault, now + self.broker.retry.as_millis() as i64);
                }
                if usable && !forced {
                    Ok(expires)
                } else {
                    Err(warning)
                }
            }
        }
    }

    /// One exchange: the CLI renews in a fresh directory of our own, the
    /// result passes the gate, and it is written only if the vault still
    /// holds what it was renewed from.
    async fn renew(
        self: &Arc<Self>,
        vault: Uuid,
        vk: &crate::vault::VaultKey,
        prev: &OauthSession,
    ) -> Result<i64, RefreshFailure> {
        let failed = |reason: String, permanent: bool| RefreshFailure { reason, permanent };
        let refresh = prev
            .refresh_token()
            .ok_or_else(|| failed("no refresh token".into(), true))?;

        let root = self.cfg.data_dir.join("oauth-refresh");
        let dir = root.join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).map_err(|e| {
            failed(
                format!("could not make a directory to renew in: {e}"),
                false,
            )
        })?;
        let scratch = crate::auth::ScratchDir(dir);
        for d in [&root, &scratch.0] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| failed(format!("could not lock down {}: {e}", d.display()), false))?;
        }

        let program = self
            .broker
            .program
            .clone()
            .unwrap_or_else(|| self.harness.program().to_string());
        self.broker.exchanges.fetch_add(1, Ordering::SeqCst);
        crate::oauth::refresh_via_cli(
            &program,
            &scratch.0,
            refresh,
            &prev.scopes(),
            self.broker.timeout,
        )
        .await?;

        let mut next = crate::auth::read_session(&scratch.0)
            .map_err(|e| failed(format!("the renewed login could not be read: {e}"), false))?;
        next.carry_forward(prev);
        // Refused output is not retried: the exchange already happened, so the
        // refresh token it spent is gone either way, and trying again would
        // only repeat the refusal.
        crate::auth::check_refresh(prev, &next, now_ms())
            .map_err(|why| failed(format!("the renewed login was refused: {why}"), true))?;
        let expires = next
            .expires_at()
            .ok_or_else(|| failed("no expiry".into(), true))?;

        let wrote = {
            let conn = self.db.lock().unwrap();
            crate::vault::replace_session_if(&conn, vk, vault, refresh, &next)
        }
        .map_err(|e| failed(format!("could not store the renewed login: {e}"), false))?;
        drop(scratch);
        if !wrote {
            tracing::warn!(%vault, "the login was replaced while it was being renewed; kept the replacement");
            let conn = self.db.lock().unwrap();
            return crate::vault::get_session(&conn, vk, vault)
                .ok()
                .flatten()
                .and_then(|s| s.expires_at())
                .ok_or_else(|| failed("the replacement login records no expiry".into(), false));
        }
        tracing::info!(%vault, "renewed the vault's Claude login");
        Ok(expires)
    }

    fn record_failure(
        &self,
        vault: Uuid,
        failure: &RefreshFailure,
        still_good_until: Option<i64>,
    ) -> String {
        let name = {
            let conn = self.db.lock().unwrap();
            board::get(&conn, vault)
                .ok()
                .flatten()
                .map(|n| n.name.to_string())
                .unwrap_or_else(|| vault.to_string())
        };
        let until = match still_good_until.and_then(crate::vault::millis_to_timestamp) {
            Some(t) => format!("agents keep running on the current login until {t}"),
            None => "the current login has lapsed".to_string(),
        };
        let next = if failure.permanent {
            "sign in again and save to this vault"
        } else {
            "the engine will retry; sign in again if this persists"
        };
        let warning = format!(
            "{WARNING_PREFIX} for vault {name}: {}; {until}; {next}",
            failure.reason
        );
        tracing::warn!(%vault, permanent = failure.permanent, reason = %failure.reason, "oauth refresh failed");
        self.broker
            .failures
            .lock()
            .unwrap()
            .insert(vault, warning.clone());
        self.warn_readers(vault, Some(&warning));
        warning
    }

    fn readers(&self, vault: Uuid) -> Vec<Uuid> {
        let conn = self.db.lock().unwrap();
        crate::vault::agents_reading(&conn, vault).unwrap_or_default()
    }

    /// Put the warning on every reader, BEFORE any of them is refused, and say
    /// so on the events stream (`node.state`). Status is left alone: an agent
    /// running on a still-good token is still running.
    fn warn_readers(&self, vault: Uuid, warning: Option<&str>) {
        for agent in self.readers(vault) {
            let conn = self.db.lock().unwrap();
            board::set_last_error(&conn, agent, warning);
            super::publish_state(&self.events, &conn, agent);
        }
    }

    fn clear_warnings(&self, vault: Uuid) {
        for agent in self.readers(vault) {
            let conn = self.db.lock().unwrap();
            let ours = board::agent_state(&conn, agent)
                .ok()
                .and_then(|s| s.last_error)
                .is_some_and(|e| e.starts_with(WARNING_PREFIX));
            if ours {
                board::set_last_error(&conn, agent, None);
                super::publish_state(&self.events, &conn, agent);
            }
        }
    }

    fn arm_renewal(self: &Arc<Self>, vault: Uuid, expires_ms: i64) {
        self.arm_refresh_timer(vault, expires_ms - self.broker.lead.as_millis() as i64);
    }

    /// One sleeping timer per vault; arming again supersedes the previous one.
    /// Nothing polls.
    pub(crate) fn arm_refresh_timer(self: &Arc<Self>, vault: Uuid, at_ms: i64) {
        let gen = {
            let mut timers = self.broker.timers.lock().unwrap();
            let g = timers.entry(vault).or_insert(0);
            *g += 1;
            *g
        };
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let wait = (at_ms - now_ms()).max(0) as u64;
            tokio::time::sleep(Duration::from_millis(wait)).await;
            if me.broker.timers.lock().unwrap().get(&vault) != Some(&gen) {
                return;
            }
            let _ = me.ensure_fresh(vault, None).await;
        });
    }

    /// Re-arm a renewal for every vault that holds a login, at boot. Reads
    /// only the recorded expiry, so nothing is decrypted to schedule it.
    pub fn arm_all_refresh_timers(self: &Arc<Self>) {
        if self.cfg.harness_auth == crate::config::HarnessAuthPolicy::ApiKeyOnly {
            return;
        }
        let pending: Vec<(Uuid, i64)> = {
            let conn = self.db.lock().unwrap();
            crate::vault::session_vaults(&conn)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| {
                    let at = crate::vault::expiry_of(&conn, v, wheel_core::CLAUDE_OAUTH_SESSION)
                        .ok()
                        .flatten()?;
                    Some((
                        v,
                        (at.into_inner().unix_timestamp_nanos() / 1_000_000) as i64,
                    ))
                })
                .collect()
        };
        for (vault, expires) in pending {
            self.arm_renewal(vault, expires);
        }
    }

    /// Move idle readers still on the old login onto the new one now. Busy
    /// ones move at the end of their turn (`pump_stdout`).
    ///
    /// Spawned rather than awaited: this runs from inside `start`, which holds
    /// the starting agent's slot, and waiting on that slot here would wait on
    /// ourselves.
    fn spawn_adopt(self: &Arc<Self>, vault: Uuid) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            for agent in me.readers(vault) {
                // Between turns, judged by the slot rather than by the status
                // in the database: `init` sets `idle` even when a turn is
                // already in flight, so the database's idea of idle would
                // recycle a child mid-turn. Liveness comes from the supervisor
                // that owns the process (§3c#15).
                let stale_and_free = {
                    let slot = me.slot(agent).await;
                    let guard = slot.lock().await;
                    guard.as_ref().is_some_and(|r| {
                        r.in_flight.is_none() && me.credential_stale(r.credential_gen.as_ref())
                    })
                };
                if stale_and_free {
                    me.recycle(agent).await;
                }
            }
        });
    }

    /// Stop a child and bring it straight back, keeping its session: the
    /// resume is transparent (§3c#14), and the new child gets the current
    /// environment. Anything queued resumes it; nothing queued leaves it
    /// parked, which costs nothing.
    pub(crate) async fn recycle(self: &Arc<Self>, agent: Uuid) {
        if self.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let interrupted = {
            let slot = self.slot(agent).await;
            let mut guard = slot.lock().await;
            match guard.take() {
                Some(mut r) => {
                    if let Err(e) = r.child.kill().await {
                        tracing::warn!(%agent, error = %e, "killing a child to move it onto a renewed login failed");
                    }
                    r.in_flight
                }
                None => None,
            }
        };
        // Taking the slot here means `reap` will not settle this child, so
        // anything written to it never ran and is handed back to the queue by
        // us. Without this it would sit `delivered` for ever: never answered,
        // never redelivered.
        if let Some(mid) = interrupted {
            let conn = self.db.lock().unwrap();
            let _ = crate::db::messages::requeue_undelivered(
                &conn,
                mid,
                "the harness was restarted onto a renewed login before this message was processed",
            );
            super::publish_message(&self.events, &conn, mid);
        }
        {
            let conn = self.db.lock().unwrap();
            let _ = crate::db::tokens::revoke(&conn, agent);
        }
        self.set_status(agent, AgentStatus::Parked, None);
        let _ = self.deliver(agent).await;
    }

    /// A turn failed on authentication. If the vault has a newer login than
    /// the one this child holds, move it across. If the child IS on the newest
    /// login and that login is near its end, renew once and move it across.
    /// Otherwise it stays `needs_auth` — the failure is not expiry, and
    /// renewing a login that is not the problem would loop.
    pub(crate) async fn recover_from_auth_failure(
        self: &Arc<Self>,
        agent: Uuid,
        gen: Option<CredentialGen>,
    ) -> bool {
        let Some((vault, spawned_with)) = gen else {
            return false;
        };
        if self.session_generation(vault) != spawned_with {
            self.recycle(agent).await;
            return true;
        }
        let near_end = spawned_with.is_some_and(|t| {
            let ms = (t.into_inner().unix_timestamp_nanos() / 1_000_000) as i64;
            ms - now_ms() <= self.broker.lead.as_millis() as i64
        });
        if near_end && self.ensure_fresh(vault, Some(spawned_with)).await.is_ok() {
            self.recycle(agent).await;
            return true;
        }
        false
    }

    /// Stop an agent that cannot authenticate, keeping its session and its
    /// reason.
    ///
    /// The queue then WAITS instead of being written into a child holding a
    /// dead token: every message delivered to it would spend a turn to be told
    /// the same thing, and be requeued again. Nothing restarts it until a
    /// person signs in again (`auth/complete`, which resumes every reader) or
    /// starts it explicitly.
    pub(crate) async fn park_needs_auth(self: &Arc<Self>, agent: Uuid) {
        let reason = {
            let conn = self.db.lock().unwrap();
            board::agent_state(&conn, agent)
                .unwrap_or_default()
                .last_error
        };
        {
            let slot = self.slot(agent).await;
            let mut guard = slot.lock().await;
            if let Some(mut r) = guard.take() {
                if let Err(e) = r.child.kill().await {
                    tracing::warn!(%agent, error = %e, "killing an unauthenticated agent's process failed");
                }
            }
        }
        {
            let conn = self.db.lock().unwrap();
            let _ = crate::db::tokens::revoke(&conn, agent);
        }
        self.set_status(agent, AgentStatus::NeedsAuth, reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use wheel_core::{
        AgentConfig, MessageSender, MessageState, Node, NodeConfig, Position, VaultConfig, WireType,
    };

    use crate::{
        config::{Config, HarnessAuthPolicy, DEFAULT_STARTUP_DEADLINE_SECS},
        db::messages,
        harness::claude::ProgramDriver,
        vault::VaultKey,
    };

    /// The QA fake, driven exactly as the real CLI is: same argv, same env,
    /// same stream-json parsing. Its fake token store mints pairs, ROTATES
    /// refresh tokens (single use) and checks every turn's access token, which
    /// is what makes a rotation race observable rather than theoretical.
    const FAKE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../qa/harness/fake-claude");
    const SCOPES: &[&str] = &["user:inference", "user:profile"];
    const VAULT_KEY_B64: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";

    struct Spec {
        agents: &'static [&'static str],
        /// How long the vault's CURRENT access token still has.
        ttl_ms: i64,
        /// Lifetime of every token the fake mints from here on.
        lifetime_ms: i64,
        lead_ms: u64,
        retry_ms: u64,
        /// The refresh token is not in the store: renewal fails `invalid_grant`.
        revoked: bool,
        policy: HarnessAuthPolicy,
        /// Extra fake-claude steering (e.g. a renewal claiming another account).
        fake: serde_json::Value,
    }

    impl Default for Spec {
        fn default() -> Self {
            Self {
                agents: &["worker"],
                ttl_ms: 3_000,
                lifetime_ms: 20_000,
                lead_ms: 1_500,
                retry_ms: 60_000,
                revoked: false,
                policy: HarnessAuthPolicy::OauthToken,
                fake: serde_json::json!({}),
            }
        }
    }

    struct Rig {
        sup: Arc<Supervisor>,
        vault: Uuid,
        agents: Vec<Uuid>,
        dir: PathBuf,
        store: PathBuf,
        /// The pair the vault started with, to compare a renewal against.
        first: OauthSession,
    }

    fn vk() -> VaultKey {
        VaultKey::from_base64(VAULT_KEY_B64).unwrap()
    }

    fn session_of(access: &str, refresh: &str, expires: i64) -> OauthSession {
        OauthSession::for_tests(access, refresh, expires, SCOPES, Some("acct-A"))
    }

    impl Rig {
        fn new(name: &str, spec: Spec) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!(
                "wheel-refresh-{name}-{}-{}",
                std::process::id(),
                Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).unwrap();

            let expires = now_ms() + spec.ttl_ms;
            let access = "sk-ant-oat01-fake-acct-A-1";
            let refresh = "sk-ant-ort01-fake-acct-A-1";
            let store = dir.join("token-store.json");
            let refresh_entry = if spec.revoked {
                serde_json::json!({})
            } else {
                serde_json::json!({ refresh: { "account": "acct-A", "scopes": SCOPES } })
            };
            std::fs::write(
                &store,
                serde_json::json!({
                    "access": { access: { "exp": expires, "account": "acct-A" } },
                    "refresh": refresh_entry,
                    "lifetime_ms": spec.lifetime_ms,
                    "rotate": true,
                    "refreshes": 0,
                    "counter": 1,
                })
                .to_string(),
            )
            .unwrap();

            let mut fake = serde_json::json!({
                "token_store": store.display().to_string(),
                "env_dump": dir.join("env.jsonl").display().to_string(),
                "login_account": "acct-A",
            });
            for (k, v) in spec.fake.as_object().cloned().unwrap_or_default() {
                fake[k] = v;
            }
            let fake_cfg = dir.join("fake.json");
            std::fs::write(&fake_cfg, fake.to_string()).unwrap();

            // The engine clears a child's environment (F015), so the fake is
            // steered from inside the program it runs, not from ours.
            let program = dir.join("claude.sh");
            std::fs::write(
                &program,
                format!(
                    "#!/bin/sh\nexport WHEEL_FAKE_CONFIG='{}'\nexec python3 '{FAKE}' \"$@\"\n",
                    fake_cfg.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            let program = program.display().to_string();

            let conn = crate::db::open_memory().unwrap();
            let vault = Node::new(
                Uuid::new_v4(),
                "anthropic".parse().unwrap(),
                Position::default(),
                NodeConfig::Vault(VaultConfig { keys: vec![] }),
            );
            board::create(&conn, &vault).unwrap();
            let mut agents = Vec::new();
            for name in spec.agents {
                let node = Node::new(
                    Uuid::new_v4(),
                    name.parse().unwrap(),
                    Position::default(),
                    NodeConfig::Agent(AgentConfig {
                        harness: wheel_core::Harness::Claude,
                        system_prompt: "test".into(),
                        ..Default::default()
                    }),
                );
                board::create(&conn, &node).unwrap();
                board::add_wire(&conn, node.id, vault.id, WireType::Read, None).unwrap();
                board::set_status(&conn, node.id, AgentStatus::Parked, None);
                agents.push(node.id);
            }

            let first = session_of(access, refresh, expires);
            crate::vault::put_with_expiry(
                &conn,
                &vk(),
                vault.id,
                wheel_core::CLAUDE_OAUTH_SESSION,
                &first.to_vault_value(),
                crate::vault::millis_to_timestamp(expires),
            )
            .unwrap();

            let cfg = Arc::new(Config {
                project_id: Uuid::new_v4(),
                engine_secret: "0123456789abcdef".into(),
                vault_key: Some(VAULT_KEY_B64.into()),
                data_dir: dir.clone(),
                listen: wheel_core::ListenAddr::parse("tcp://127.0.0.1:7999").unwrap(),
                json_logs: false,
                tool_allow_hosts: Vec::new(),
                startup_deadline_secs: DEFAULT_STARTUP_DEADLINE_SECS,
                harness_auth: spec.policy,
            });
            let mut sup = Supervisor::with_harness(
                cfg,
                Arc::new(Mutex::new(conn)),
                Arc::new(crate::events::Bus::new()),
                Arc::new(ProgramDriver(program.clone())),
            );
            sup.broker.program = Some(program);
            sup.broker.lead = Duration::from_millis(spec.lead_ms);
            sup.broker.retry = Duration::from_millis(spec.retry_ms);
            sup.broker.timeout = Duration::from_secs(30);

            Self {
                sup: Arc::new(sup),
                vault: vault.id,
                agents,
                dir,
                store,
                first,
            }
        }

        fn send(&self, agent: Uuid) -> Uuid {
            let msg = {
                let conn = self.sup.db.lock().unwrap();
                messages::enqueue(&conn, MessageSender::User, agent, "hello".into(), None).unwrap()
            };
            msg.id
        }

        async fn send_and_deliver(&self, agent: Uuid) -> Uuid {
            let id = self.send(agent);
            let _ = self.sup.deliver(agent).await;
            id
        }

        fn message(&self, id: Uuid) -> wheel_core::Message {
            let conn = self.sup.db.lock().unwrap();
            messages::get(&conn, id).unwrap().unwrap()
        }

        fn answered(&self, id: Uuid) -> bool {
            let m = self.message(id);
            m.state == MessageState::Consumed && m.last_error.is_none()
        }

        fn session(&self) -> OauthSession {
            let conn = self.sup.db.lock().unwrap();
            crate::vault::get_session(&conn, &vk(), self.vault)
                .unwrap()
                .unwrap()
        }

        fn status(&self, agent: Uuid) -> AgentStatus {
            super::super::current_status(&self.sup.db, agent)
        }

        fn last_error(&self, agent: Uuid) -> Option<String> {
            let conn = self.sup.db.lock().unwrap();
            board::agent_state(&conn, agent).unwrap().last_error
        }

        fn store_json(&self) -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(&self.store).unwrap()).unwrap()
        }

        fn refreshes(&self) -> u64 {
            self.store_json()["refreshes"].as_u64().unwrap_or(0)
        }

        fn exchanges(&self) -> u64 {
            self.sup.broker.exchanges.load(Ordering::SeqCst)
        }

        /// Every environment variable digest every spawned child was given.
        fn child_env_digests(&self) -> Vec<String> {
            let path = self.dir.join("env.jsonl");
            let Ok(raw) = std::fs::read_to_string(path) else {
                return Vec::new();
            };
            raw.lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .flat_map(|rec| {
                    rec["env_digests"]
                        .as_object()
                        .map(|m| {
                            m.values()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_else(Vec::new)
                })
                .collect()
        }

        fn spawns(&self) -> usize {
            std::fs::read_to_string(self.dir.join("env.jsonl"))
                .map(|r| r.lines().count())
                .unwrap_or(0)
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn until(what: &str, mut cond: impl FnMut() -> bool) {
        // Generous for the same reason the supervisor's own helper is: these
        // spawn real processes on a shared dev host.
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {what}");
    }

    fn saw_needs_auth(rx: &mut tokio::sync::broadcast::Receiver<wheel_core::Event>) -> bool {
        let mut seen = false;
        while let Ok(ev) = rx.try_recv() {
            if let wheel_core::Event::NodeState {
                state: wheel_core::NodeState::Agent(st),
                ..
            } = ev
            {
                seen |= st.status == AgentStatus::NeedsAuth;
            }
        }
        seen
    }

    impl Rig {
        async fn send_body(&self, agent: Uuid, body: &str) -> Uuid {
            let msg = {
                let conn = self.sup.db.lock().unwrap();
                messages::enqueue(&conn, MessageSender::User, agent, body.into(), None).unwrap()
            };
            let _ = self.sup.deliver(agent).await;
            msg.id
        }

        /// Replace the vault's refreshable login with a bare credential, for
        /// the policy cases that are not about renewal at all.
        fn put_plain(&self, key: &str, value: &str) {
            let conn = self.sup.db.lock().unwrap();
            for existing in crate::vault::list_keys(&conn, self.vault).unwrap() {
                crate::vault::delete(&conn, self.vault, &existing).unwrap();
            }
            crate::vault::put(&conn, &vk(), self.vault, key, value).unwrap();
        }

        /// How many times this message was written to a child's stdin, from
        /// the transcript stream — the engine's own record of what it sent.
        fn deliveries_of(&self, id: Uuid) -> usize {
            let conn = self.sup.db.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT COUNT(*) FROM logs WHERE stream = 'transcript' AND text LIKE ?1")
                .unwrap();
            stmt.query_row(rusqlite::params![format!("%{id}%")], |r| r.get::<_, i64>(0))
                .unwrap_or(0) as usize
        }

        fn digest(&self, s: &str) -> String {
            wheel_core::sha256_hex(s.as_bytes())
        }
    }

    /// THE acceptance test (docs/proposals/harness-oauth-refresh.md §8): an
    /// agent keeps answering past the expiry of the token it started on, with
    /// no human touch, and the vault ends up holding the renewed pair.
    ///
    /// Mutation-checked: make `ensure_fresh` return early without renewing and
    /// the second message comes back `Login expired · Please run /login`.
    #[tokio::test]
    async fn an_agent_keeps_answering_past_its_original_expiry_and_the_vault_holds_the_renewal() {
        let rig = Rig::new("survives", Spec::default());
        let agent = rig.agents[0];
        let mut events = rig.sup.events().subscribe();
        let original_expiry = rig.first.expires_at().unwrap();

        let first = rig.send_and_deliver(agent).await;
        until("the first turn to be answered", || rig.answered(first)).await;

        // Nobody asks for this: the renewal is a timer the engine armed.
        until("the vault to hold a renewed login", || {
            rig.session().access_token() != rig.first.access_token()
        })
        .await;

        until("the original token to have expired", || {
            now_ms() > original_expiry + 200
        })
        .await;
        let second = rig.send_and_deliver(agent).await;
        until("a turn past the original expiry to be answered", || {
            rig.answered(second)
        })
        .await;

        let now = rig.session();
        assert!(
            now.expires_at().unwrap() > original_expiry,
            "the vault must hold a later expiry than the token the agent started on"
        );
        assert_ne!(now.refresh_token(), rig.first.refresh_token(), "rotated");
        assert_eq!(rig.refreshes(), 1, "exactly one exchange with the server");
        assert!(
            !saw_needs_auth(&mut events),
            "the agent must never have gone needs_auth: it was renewed, not recovered"
        );

        // TH3: children run on access tokens. A refresh token in a child is a
        // second refresher of a single-use token and a place to steal it from.
        let digests = rig.child_env_digests();
        assert!(
            !digests.is_empty(),
            "the children recorded their environment"
        );
        for refresh in [
            rig.first.refresh_token().unwrap(),
            now.refresh_token().unwrap(),
        ] {
            assert!(
                !digests.contains(&rig.digest(refresh)),
                "a refresh token reached a child's environment"
            );
        }
        assert!(
            digests.contains(&rig.digest(now.access_token().unwrap())),
            "the child must have been given the RENEWED access token"
        );
    }

    /// The other half of adoption: a child that is mid-turn when the renewal
    /// lands moves onto it at the end of its turn, which is the one moment it
    /// can be swapped without losing work.
    ///
    /// Mutation-checked: drop the turn-end staleness check and the agent is
    /// still holding the old token when it expires, so the next message goes
    /// through an auth failure — `saw_needs_auth` catches it.
    #[tokio::test]
    async fn a_busy_child_moves_onto_the_renewed_login_at_the_end_of_its_turn() {
        let rig = Rig::new("busy", Spec::default());
        let agent = rig.agents[0];
        let mut events = rig.sup.events().subscribe();
        let original_expiry = rig.first.expires_at().unwrap();

        // A turn that is still running when the renewal timer fires.
        let long = rig.send_body(agent, "<<FAKE:SLEEP=2.5>> working").await;
        until("the vault to hold a renewed login", || {
            rig.session().access_token() != rig.first.access_token()
        })
        .await;
        until("the long turn to finish", || rig.answered(long)).await;
        until("the original token to have expired", || {
            now_ms() > original_expiry + 200
        })
        .await;

        let after = rig.send_and_deliver(agent).await;
        until("the next turn to be answered", || rig.answered(after)).await;
        assert!(
            !saw_needs_auth(&mut events),
            "the child must have been moved onto the renewed login at its turn boundary"
        );
    }

    /// TH2, the rotation race. Two agents share one login; a single-use
    /// refresh token has ONE user, so a design where both refresh leaves the
    /// loser's store wiped. Neither may brick the other.
    #[tokio::test]
    async fn two_agents_on_one_login_both_survive_its_rotation() {
        let rig = Rig::new(
            "pair",
            Spec {
                agents: &["alpha", "beta"],
                ..Spec::default()
            },
        );
        let (alpha, beta) = (rig.agents[0], rig.agents[1]);
        let mut events = rig.sup.events().subscribe();
        let original_expiry = rig.first.expires_at().unwrap();

        let (a1, b1) = (
            rig.send_and_deliver(alpha).await,
            rig.send_and_deliver(beta).await,
        );
        until("both first turns", || rig.answered(a1) && rig.answered(b1)).await;
        until("the renewal", || {
            rig.session().access_token() != rig.first.access_token()
        })
        .await;
        until("the original token to have expired", || {
            now_ms() > original_expiry + 200
        })
        .await;

        let (a2, b2) = (
            rig.send_and_deliver(alpha).await,
            rig.send_and_deliver(beta).await,
        );
        until("both second turns", || rig.answered(a2) && rig.answered(b2)).await;

        assert_eq!(
            rig.refreshes(),
            1,
            "two agents on one login must cost ONE exchange, not one each"
        );
        assert!(!saw_needs_auth(&mut events), "neither agent may be bricked");
        for a in [alpha, beta] {
            assert_ne!(rig.status(a), AgentStatus::NeedsAuth);
        }
    }

    /// The serialisation itself, forced rather than hoped for: many callers
    /// arrive at once with a login that needs renewing.
    ///
    /// Mutation-checked: remove the lineage lock and this reports several
    /// exchanges — and with a rotating refresh token, all but one are
    /// `invalid_grant`.
    #[tokio::test]
    async fn concurrent_callers_cost_exactly_one_exchange() {
        let rig = Rig::new(
            "concurrent",
            Spec {
                ttl_ms: 800,
                agents: &["a", "b", "c"],
                ..Spec::default()
            },
        );
        let mut tasks = Vec::new();
        for _ in 0..6 {
            let sup = rig.sup.clone();
            let vault = rig.vault;
            tasks.push(tokio::spawn(
                async move { sup.ensure_fresh(vault, None).await },
            ));
        }
        let mut answers = Vec::new();
        for t in tasks {
            answers.push(t.await.unwrap().expect("every caller gets a usable login"));
        }
        assert!(
            answers.windows(2).all(|w| w[0] == w[1]),
            "every caller must be handed the same login: {answers:?}"
        );
        assert_eq!(rig.exchanges(), 1, "one exchange for six callers");
        assert_eq!(rig.refreshes(), 1, "and the server saw exactly one");
    }

    /// TH5. A renewal that fails warns while the current token still works —
    /// on `GET auth` and as a `node.state` event — and only then, when the
    /// token really has lapsed, parks the agent `needs_auth` with its message
    /// still queued. It must not loop.
    #[tokio::test]
    async fn a_failed_renewal_warns_first_then_parks_needs_auth_with_the_message_requeued() {
        let rig = Rig::new(
            "failing",
            Spec {
                revoked: true,
                ttl_ms: 2_500,
                lead_ms: 5_000, // so the first start already tries, and fails
                ..Spec::default()
            },
        );
        let agent = rig.agents[0];
        let mut events = rig.sup.events().subscribe();

        let first = rig.send_and_deliver(agent).await;
        until("the first turn to be answered anyway", || {
            rig.answered(first)
        })
        .await;

        // BEFORE any refusal: the warning is up, the agent is still working.
        let warning = rig
            .sup
            .refresh_warning(rig.vault)
            .expect("a failed renewal must be visible on GET auth");
        assert!(warning.contains("invalid_grant"), "{warning}");
        assert!(warning.contains("agents keep running"), "{warning}");
        assert_ne!(rig.status(agent), AgentStatus::NeedsAuth);
        assert!(
            rig.last_error(agent)
                .is_some_and(|e| e.starts_with(WARNING_PREFIX)),
            "the warning must survive the agent starting normally"
        );
        let warned_on_the_stream = {
            let mut found = false;
            while let Ok(ev) = events.try_recv() {
                if let wheel_core::Event::NodeState {
                    state: wheel_core::NodeState::Agent(st),
                    ..
                } = ev
                {
                    found |= st
                        .last_error
                        .as_deref()
                        .is_some_and(|e| e.starts_with(WARNING_PREFIX));
                }
            }
            found
        };
        assert!(warned_on_the_stream, "node.state must carry the warning");

        // ...and now the token really lapses.
        until("the token to lapse", || {
            now_ms() > rig.first.expires_at().unwrap() + 200
        })
        .await;
        let second = rig.send_and_deliver(agent).await;
        until("the agent to park needs_auth", || {
            rig.status(agent) == AgentStatus::NeedsAuth
        })
        .await;
        assert_eq!(
            rig.message(second).state,
            MessageState::Queued,
            "the message must go back on the queue, not be consumed by a dead token"
        );
        // Parking happens after one last renewal attempt, so this waits for
        // the process to go rather than racing the status write.
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while rig.sup.live_agents().await.contains(&agent) {
            assert!(
                std::time::Instant::now() < deadline,
                "an agent that cannot authenticate must hold no process: parked with \
                 a reason, not left warm"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // No loop: more messages do not spend more turns or more exchanges.
        let (exchanges, spawns) = (rig.exchanges(), rig.spawns());
        let third = rig.send_and_deliver(agent).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(
            rig.exchanges(),
            exchanges,
            "a dead login was retried in a loop"
        );
        assert_eq!(rig.spawns(), spawns, "a child was spawned for a dead login");
        assert_eq!(rig.message(third).state, MessageState::Queued);
    }

    /// The same failure before anything spawns: a login that has already
    /// lapsed and cannot be renewed is `needs_auth` with no child started at
    /// all, and the message that woke the agent stays queued.
    #[tokio::test]
    async fn a_lapsed_login_that_cannot_be_renewed_is_needs_auth_before_anything_spawns() {
        let rig = Rig::new(
            "lapsed",
            Spec {
                revoked: true,
                ttl_ms: -1_000,
                ..Spec::default()
            },
        );
        let agent = rig.agents[0];
        let queued = rig.send_and_deliver(agent).await;

        assert_eq!(rig.status(agent), AgentStatus::NeedsAuth);
        assert_eq!(rig.spawns(), 0, "no child may be spawned on a dead login");
        assert_eq!(rig.message(queued).state, MessageState::Queued);
        let err = rig.last_error(agent).unwrap_or_default();
        assert!(err.contains("anthropic"), "name the vault: {err}");
        assert!(err.contains("invalid_grant"), "name the reason: {err}");
    }

    /// TH1 at the refresher. A renewal whose output belongs to another account
    /// is refused, and the vault keeps the login it had.
    ///
    /// Mutation-checked: drop the identity arm of `check_refresh` and the
    /// vault ends up holding the other account's login — every peer would then
    /// be running as, and billing, an account nobody chose.
    #[tokio::test]
    async fn a_renewal_claiming_another_account_is_refused_and_the_vault_keeps_its_login() {
        let rig = Rig::new(
            "tamper",
            Spec {
                ttl_ms: 800,
                fake: serde_json::json!({ "refresh_claims_account": "acct-EVIL" }),
                ..Spec::default()
            },
        );
        let before = rig.session();
        let _ = rig.sup.ensure_fresh(rig.vault, None).await;

        assert_eq!(
            rig.session(),
            before,
            "the vault must still hold the login it had"
        );
        let warning = rig.sup.refresh_warning(rig.vault).unwrap_or_default();
        assert!(
            warning.contains("different account"),
            "the refusal must say what it saw: {warning}"
        );
    }

    /// TH1 at the agent. An agent is untrusted code whose HOME is its own
    /// config dir, so it can write a credential store there — of another
    /// account, valid, and newer. Nothing in that directory is ever read back
    /// into the vault.
    ///
    /// Mutation-checked: point `renew` at a reader's config dir instead of the
    /// engine's own scratch dir and the planted account is promoted.
    #[tokio::test]
    async fn a_credential_planted_in_an_agents_home_is_never_promoted_to_the_vault() {
        let rig = Rig::new(
            "planted",
            Spec {
                ttl_ms: 800,
                ..Spec::default()
            },
        );
        let agent = rig.agents[0];

        // The agent plants a live credential for another account, in both
        // layouts the CLI could ever write.
        let planted = serde_json::json!({"claudeAiOauth": {
            "accessToken": "sk-ant-oat01-fake-acct-EVIL-99",
            "refreshToken": "sk-ant-ort01-fake-acct-EVIL-99",
            "expiresAt": now_ms() + 9_000_000,
            "scopes": SCOPES,
        }})
        .to_string();
        let home = rig.dir.join("creds").join(agent.to_string());
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        for p in [
            home.join(".credentials.json"),
            home.join(".claude/.credentials.json"),
        ] {
            std::fs::write(p, &planted).unwrap();
        }
        std::fs::write(
            home.join(".claude.json"),
            serde_json::json!({"oauthAccount": {"accountUuid": "acct-A",
                "organizationUuid": "org-of-acct-A"}})
            .to_string(),
        )
        .unwrap();

        rig.sup
            .ensure_fresh(rig.vault, None)
            .await
            .expect("the renewal itself must succeed");

        let now = rig.session();
        assert!(
            now.access_token().unwrap().contains("acct-A"),
            "the vault must hold the ACCOUNT'S renewed login, not the planted one: {:?}",
            now.access_token()
        );
        assert!(!now.access_token().unwrap().contains("EVIL"));
        assert!(!now.refresh_token().unwrap().contains("EVIL"));
        assert_eq!(now.account().account_uuid.as_deref(), Some("acct-A"));
    }

    /// TH6. On an api-key-only deployment nothing here runs: no renewal, and a
    /// vault-supplied OAuth credential of either shape refuses to spawn.
    ///
    /// Mutation-checked: drop `vault_supplies_oauth` from the spawn gate and a
    /// cloud project runs on a vaulted OAuth login.
    #[tokio::test]
    async fn api_key_only_never_renews_and_refuses_a_vaulted_oauth_login_at_spawn() {
        let rig = Rig::new(
            "policy",
            Spec {
                policy: HarnessAuthPolicy::ApiKeyOnly,
                ..Spec::default()
            },
        );
        let agent = rig.agents[0];

        let refused = rig.sup.ensure_fresh(rig.vault, None).await.unwrap_err();
        assert!(refused.contains("api-key-only"), "{refused}");

        assert_eq!(rig.sup.start(agent).await.unwrap(), AgentStatus::Error);
        assert!(rig
            .last_error(agent)
            .unwrap_or_default()
            .contains("api-key-only"));
        assert_eq!(rig.spawns(), 0);
        assert_eq!(rig.exchanges(), 0, "no exchange may be attempted");

        // A bare setup-token in a vault is the same policy answer...
        rig.put_plain("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-durable");
        assert_eq!(rig.sup.start(agent).await.unwrap(), AgentStatus::Error);
        // ...and an `sk-ant-oat` value under any other name is too, because an
        // agent can export whatever it can read.
        rig.put_plain("SOMETHING_ELSE", "sk-ant-oat01-smuggled");
        assert_eq!(rig.sup.start(agent).await.unwrap(), AgentStatus::Error);

        // An API key is what this deployment is for, and it still starts.
        rig.put_plain("ANTHROPIC_API_KEY", "sk-ant-api03-real");
        assert_ne!(rig.sup.start(agent).await.unwrap(), AgentStatus::Error);
    }

    /// TH7. The refresh token goes in the environment, never argv — argv is
    /// world-readable across uids — and never into the reason a failure
    /// reports.
    ///
    /// Mutation-checked: pass it as an argument, or drop the redaction, and
    /// this fails.
    #[tokio::test]
    async fn the_refresh_token_never_reaches_argv_or_a_reported_reason() {
        use std::os::unix::fs::PermissionsExt;
        let rig = Rig::new(
            "argv",
            Spec {
                ttl_ms: 800,
                ..Spec::default()
            },
        );
        // A refresher that records how it was called, then fails loudly with
        // the secret in its own output.
        let spy = rig.dir.join("spy.sh");
        std::fs::write(
            &spy,
            format!(
                "#!/bin/sh\necho \"$@\" > '{d}/argv'\n\
                 [ -n \"$CLAUDE_CODE_OAUTH_REFRESH_TOKEN\" ] && echo yes > '{d}/env-had-it'\n\
                 echo \"boom $CLAUDE_CODE_OAUTH_REFRESH_TOKEN\" >&2\nexit 1\n",
                d = rig.dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&spy, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Driven through the same call `renew` makes, with the same arguments.
        let secret = rig.first.refresh_token().unwrap().to_string();
        let failure = crate::oauth::refresh_via_cli(
            &spy.display().to_string(),
            &rig.dir,
            &secret,
            &SCOPES.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            Duration::from_secs(30),
        )
        .await
        .unwrap_err();

        let argv = std::fs::read_to_string(rig.dir.join("argv")).unwrap();
        assert!(
            !argv.contains(&secret),
            "the refresh token reached argv: {argv}"
        );
        assert!(argv.contains("auth login"), "it is still a login: {argv}");
        assert!(
            rig.dir.join("env-had-it").exists(),
            "it must arrive in the env"
        );
        assert!(
            !failure.reason.contains(&secret),
            "the refresh token reached a reported reason: {}",
            failure.reason
        );
        assert!(failure.reason.contains("redacted"), "{}", failure.reason);
    }

    /// The anti-loop bound. A login that is NOT near its end failed
    /// authentication for some other reason (a suspended account, say), so
    /// renewing it would not help — and renewing on every failure would spin
    /// for as long as the messages last.
    ///
    /// Mutation-checked: drop the `near_end` condition and this reports an
    /// exchange.
    #[tokio::test]
    async fn a_fresh_login_that_fails_authentication_is_not_renewed() {
        let rig = Rig::new(
            "bound",
            Spec {
                ttl_ms: 60_000,
                lead_ms: 1_000,
                ..Spec::default()
            },
        );
        let agent = rig.agents[0];
        let gen = rig.sup.session_generation(rig.vault);

        let recovered = rig
            .sup
            .recover_from_auth_failure(agent, Some((rig.vault, gen)))
            .await;
        assert!(!recovered, "there is nothing to recover to");
        assert_eq!(
            rig.exchanges(),
            0,
            "a token with an hour left is not the reason the turn failed"
        );
    }

    /// Recycling takes the slot itself, so `reap` will not settle the child it
    /// kills: whatever was written to that child never ran and has to be
    /// handed back to the queue here. Without it the message sits `delivered`
    /// for ever — never answered, never redelivered — which is how a renewal
    /// could silently eat a turn.
    #[tokio::test]
    async fn a_turn_interrupted_by_a_recycle_goes_back_on_the_queue_and_is_answered() {
        let rig = Rig::new(
            "interrupted",
            Spec {
                ttl_ms: 60_000,
                lead_ms: 1_000,
                ..Spec::default()
            },
        );
        let agent = rig.agents[0];

        let long = rig.send_body(agent, "<<FAKE:SLEEP=3>> working").await;
        until("the turn to be in flight", || {
            rig.message(long).state == MessageState::Delivered
        })
        .await;

        rig.sup.recycle(agent).await;

        // `Consumed`, not `answered`: a requeue records WHY it was requeued on
        // the message, and that reason survives the turn that finally runs it.
        until(
            "the interrupted turn to be answered by the next child",
            || rig.message(long).state == MessageState::Consumed,
        )
        .await;
        assert_eq!(
            rig.deliveries_of(long),
            2,
            "the interrupted message must have been written to a child a second time"
        );
    }

    /// The belt to that brace: even with a live child, an agent whose
    /// credentials failed is not written to. Otherwise a message arriving in
    /// the moment between the failure and the park spends a turn to be told
    /// the same thing.
    #[tokio::test]
    async fn nothing_is_delivered_to_an_agent_that_needs_authentication() {
        let rig = Rig::new(
            "needsauth",
            Spec {
                ttl_ms: 60_000,
                lead_ms: 1_000,
                ..Spec::default()
            },
        );
        let agent = rig.agents[0];
        let warm = rig.send_and_deliver(agent).await;
        until("the agent to be warm", || rig.answered(warm)).await;
        let before = rig.deliveries_of(warm);

        {
            let conn = rig.sup.db.lock().unwrap();
            board::set_status(&conn, agent, AgentStatus::NeedsAuth, Some("no credentials"));
        }
        let ignored = rig.send_and_deliver(agent).await;
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert_eq!(
            rig.message(ignored).state,
            MessageState::Queued,
            "the queue waits for a person, it is not spent on a dead token"
        );
        assert_eq!(
            rig.deliveries_of(ignored),
            0,
            "nothing was written to the child"
        );
        assert_eq!(rig.deliveries_of(warm), before);
    }

    /// `live_agents` is what the healthcheck uses to tell a turn in progress
    /// from a wedge (`api::stalled_agents`), so it has to mean "holds a
    /// process", not "has started one at some point". A parked agent counted
    /// as live hides the one failure the system cannot leave on its own.
    #[tokio::test]
    async fn live_agents_counts_processes_not_agents_that_once_had_one() {
        let rig = Rig::new(
            "liveness",
            Spec {
                ttl_ms: 60_000,
                lead_ms: 1_000,
                ..Spec::default()
            },
        );
        let agent = rig.agents[0];
        let first = rig.send_and_deliver(agent).await;
        until("the agent to be running", || rig.answered(first)).await;
        assert!(rig.sup.live_agents().await.contains(&agent));

        rig.sup.stop(agent).await.unwrap();
        assert!(
            !rig.sup.live_agents().await.contains(&agent),
            "a stopped agent holds no process"
        );
    }
}
