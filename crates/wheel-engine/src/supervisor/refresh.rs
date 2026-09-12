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
            self.arm_renewal(vault, (at.into_inner().unix_timestamp_nanos() / 1_000_000) as i64);
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
            return Err("this project is api-key-only: a refreshable OAuth login is not \
                        permitted here, store an API key instead"
                .into());
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
        std::fs::create_dir_all(&dir)
            .map_err(|e| failed(format!("could not make a directory to renew in: {e}"), false))?;
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
        let expires = next.expires_at().ok_or_else(|| failed("no expiry".into(), true))?;

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
                    Some((v, (at.into_inner().unix_timestamp_nanos() / 1_000_000) as i64))
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
                let stale_and_idle = {
                    let slot = me.slot(agent).await;
                    let guard = slot.lock().await;
                    let stale = guard
                        .as_ref()
                        .is_some_and(|r| me.credential_stale(r.credential_gen.as_ref()));
                    stale && super::current_status(&me.db, agent) == AgentStatus::Idle
                };
                if stale_and_idle {
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
        {
            let slot = self.slot(agent).await;
            let mut guard = slot.lock().await;
            if let Some(mut r) = guard.take() {
                if let Err(e) = r.child.kill().await {
                    tracing::warn!(%agent, error = %e, "killing a child to move it onto a renewed login failed");
                }
            }
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
    ) {
        let Some((vault, spawned_with)) = gen else {
            return;
        };
        if self.session_generation(vault) != spawned_with {
            self.recycle(agent).await;
            return;
        }
        let near_end = spawned_with.is_some_and(|t| {
            let ms = (t.into_inner().unix_timestamp_nanos() / 1_000_000) as i64;
            ms - now_ms() <= self.broker.lead.as_millis() as i64
        });
        if near_end && self.ensure_fresh(vault, Some(spawned_with)).await.is_ok() {
            self.recycle(agent).await;
        }
    }
}
