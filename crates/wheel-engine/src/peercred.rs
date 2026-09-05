//! Peer-credential admission for the unix-socket control plane.
//!
//! In process mode the socket IS the tenant boundary: every project's engine
//! lives on one shared kernel, and reaching another project's socket is
//! reaching its board, its vault and its agents. The socket is already `0600`
//! inside a `0700` directory, so this check is the second lock on the same
//! door — and it is the one that still holds if the first is ever loosened by
//! a umask, a bad chmod, a restored backup, or a host that recreates the
//! directory with different modes.
//!
//! Permitted: the uid the engine runs as (its own project), and root. Root is
//! not a concession — the host proxies API traffic to this socket and runs
//! privileged in process mode, and a root that wanted the data could read the
//! socket, the database and `/proc` regardless.

use std::io;

use axum::serve::Listener;
use tokio::net::{unix::SocketAddr, UnixListener, UnixStream};

pub struct PeerCredListener {
    inner: UnixListener,
    owner_uid: u32,
}

impl PeerCredListener {
    pub fn new(inner: UnixListener) -> Self {
        Self {
            inner,
            owner_uid: nix_getuid(),
        }
    }

    fn admits(&self, uid: u32) -> bool {
        uid == self.owner_uid || uid == 0
    }
}

/// The engine's own effective uid.
fn nix_getuid() -> u32 {
    // SAFETY: getuid() is always successful and touches no memory we own.
    unsafe { libc::getuid() }
}

impl Listener for PeerCredListener {
    type Io = UnixStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (stream, addr) = match self.inner.accept().await {
                Ok(pair) => pair,
                // Matches what axum's own listeners do: a failed accept is not
                // a reason to stop serving every future connection.
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    continue;
                }
            };

            match stream.peer_cred() {
                Ok(cred) if self.admits(cred.uid()) => return (stream, addr),
                Ok(cred) => {
                    // Loud: on a shared kernel this is another tenant, or a
                    // process that should not be able to see us at all.
                    tracing::warn!(
                        peer_uid = cred.uid(),
                        peer_pid = ?cred.pid(),
                        owner_uid = self.owner_uid,
                        "refused a control-plane connection from another uid"
                    );
                }
                // No credentials, no admission. Failing open here would make
                // the whole check decorative on any platform that surprises us.
                Err(e) => {
                    tracing::warn!(error = %e, "refused a connection whose peer credentials could not be read");
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener() -> PeerCredListener {
        let path = std::env::temp_dir().join(format!("wheel-peercred-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let l = PeerCredListener::new(UnixListener::bind(&path).unwrap());
        let _ = std::fs::remove_file(&path);
        l
    }

    #[tokio::test]
    async fn the_owning_uid_and_root_are_admitted_and_nobody_else_is() {
        let l = listener();
        assert!(l.admits(l.owner_uid), "the engine's own uid must be let in");
        assert!(l.admits(0), "root proxies API traffic to this socket");

        // Every other uid is another tenant on a shared kernel.
        for uid in [1u32, 1000, 10001, 20000, 20064, u32::MAX] {
            if uid == l.owner_uid {
                continue;
            }
            assert!(
                !l.admits(uid),
                "uid {uid} must not reach this control plane"
            );
        }
    }
}
