// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The seam between an engine and whatever updates its deployment
//! (docs/proposals/auto-update.md).
//!
//! An engine with no hook is one whose deployment does not update itself, and
//! every surface says so. `wheeld` supplies a hook in-process. A multi-process
//! host would implement [`EngineControl`] over control-plane routes instead;
//! the operations are the same five.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
};

use rusqlite::Connection;
use uuid::Uuid;
use wheel_core::{MessageSender, UpdateNotice};

use crate::{
    db::{board, messages},
    supervisor::Supervisor,
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The agent asking. Only an agent: an endpoint can trigger a script, so a
/// script that could ask would hand the internet a restart lever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requester {
    pub project: Uuid,
    pub node: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestOutcome {
    Accepted(Option<UpdateNotice>),
    AlreadyRequested(Option<UpdateNotice>),
    NothingToDo,
    Refused(String),
}

pub trait UpdateHook: Send + Sync {
    /// A read of cached state; may start a rate-limited check in the background.
    fn notice(&self) -> Option<UpdateNotice>;
    fn request(&self, from: Requester) -> RequestOutcome;
    /// Called on every engine start, so the updater can drain it and reach its agents.
    fn attach(&self, project: Uuid, engine: Weak<dyn EngineControl>);
}

/// What an updater may do to a running engine. Nothing here can end a turn.
///
/// Stopping the agents is NOT here on purpose: that is
/// [`Supervisor::shutdown`]'s, which the daemon already runs on its way out
/// (`api/headless-first`). The updater's job is only to bring the board to a
/// quiet point and then let that shutdown happen — so there is one path that
/// stops an agent, not two.
pub trait EngineControl: Send + Sync {
    /// Stop starting turns. A turn already in flight runs to completion.
    fn pause(&self);
    /// Names of agents that are mid-turn or mid-spawn.
    fn busy(&self) -> BoxFuture<'_, Vec<String>>;
    /// Start turns again and deliver what queued meanwhile: the abort path.
    fn resume(&self) -> BoxFuture<'_, ()>;
    /// Queue a `system`-type message to an agent and deliver it.
    fn post_system(&self, agent: Uuid, body: String) -> BoxFuture<'_, bool>;
}

pub struct SupervisorControl {
    supervisor: Arc<Supervisor>,
    db: Arc<Mutex<Connection>>,
}

impl SupervisorControl {
    pub fn new(supervisor: Arc<Supervisor>, db: Arc<Mutex<Connection>>) -> Self {
        Self { supervisor, db }
    }
}

impl EngineControl for SupervisorControl {
    fn pause(&self) {
        self.supervisor.pause_turns();
    }

    fn busy(&self) -> BoxFuture<'_, Vec<String>> {
        Box::pin(async move {
            let ids = self.supervisor.busy_agents().await;
            let Ok(conn) = self.db.lock() else {
                return ids.iter().map(Uuid::to_string).collect();
            };
            ids.into_iter()
                .map(|id| {
                    board::get(&conn, id)
                        .ok()
                        .flatten()
                        .map(|n| n.name.to_string())
                        .unwrap_or_else(|| id.to_string())
                })
                .collect()
        })
    }

    fn resume(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.supervisor.resume_turns().await })
    }

    fn post_system(&self, agent: Uuid, body: String) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            let queued = match self.db.lock() {
                Ok(conn) => messages::enqueue(&conn, MessageSender::System, agent, body, None)
                    .map_err(|e| tracing::warn!(%agent, error = %e, "could not queue an update notice"))
                    .is_ok(),
                Err(_) => false,
            };
            if queued {
                if let Err(e) = self.supervisor.deliver(agent).await {
                    tracing::warn!(%agent, error = %e, "queued an update notice but could not deliver it yet");
                }
            }
            queued
        })
    }
}
