//! Secrets that persist across restarts, and the environment the composed services boot from.

use anyhow::{Context, Result};
use base64::Engine as _;
use std::path::{Path, PathBuf};

/// The long-lived secrets a local install needs, generated once and kept.
///
/// They must survive a restart or the install breaks itself: the master key decrypts every
/// project's engine secret and vault key, so a fresh one on every boot would orphan every board.
pub struct Keys {
    /// Encrypts per-project secrets at rest, and derives the session signing key.
    pub master_key: String,
    /// Authenticates the API to the host. Regenerated freely — it is only meaningful while both
    /// halves of this process are alive.
    pub host_secret: String,
}

impl Keys {
    /// Load the master key from `<data_dir>/master.key`, creating it on first run.
    pub fn load_or_create(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join("master.key");
        let master_key = match std::fs::read_to_string(&path) {
            Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
            _ => {
                let k = random_base64_32();
                write_private(&path, &k).with_context(|| format!("writing {}", path.display()))?;
                tracing::info!(path = %path.display(), "generated a master key");
                k
            }
        };
        Ok(Self {
            master_key,
            // Never persisted: it authenticates one half of this process to the other, and both
            // halves die together. A file on disk would only be one more secret to leak.
            host_secret: random_base64_32(),
        })
    }
}

/// Write a secret readable only by its owner, and create it that way rather than fixing the mode
/// afterwards — between create and chmod, the file is world-readable.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(contents.as_bytes())?;
    Ok(())
}

fn random_base64_32() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::STANDARD.encode(buf)
}

/// The data directory, created if missing, private to its owner.
pub fn prepare_data_dir(data_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("creating {}", data_dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Project data, vault ciphertext and the master key live here.
        std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("securing {}", data_dir.display()))?;
    }
    Ok(data_dir.to_path_buf())
}

/// Environment defaults for the composed services.
///
/// Returned as pairs rather than applied, so the choices are testable: this is where `wheeld`
/// decides what a local install is, and every one of these is a decision someone will need to
/// audit later.
///
/// Only defaults — anything already set in the environment wins, so a container can override.
pub fn composed_env(data_dir: &Path, keys: &Keys, host_url: &str) -> Vec<(&'static str, String)> {
    vec![
        // Local accounts, always. A local install has no identity provider, and the alternative —
        // a dev bypass — is exactly what must never be reachable from a listening socket.
        ("AUTH_MODE", "local".into()),
        // Prod, not dev: `dev` is what unlocks the HS256 bypass, and `wheeld` binds a real port.
        // Choosing the stricter environment means a local install refuses the same footguns the
        // deployed one refuses.
        ("WHEEL_ENV", "prod".into()),
        ("API_MASTER_KEY", keys.master_key.clone()),
        ("WHEEL_HOST_SECRET", keys.host_secret.clone()),
        ("WHEEL_HOST_URL", host_url.to_string()),
        ("WHEEL_DATA_DIR", data_dir.display().to_string()),
        ("SANDBOX_BACKEND", "process".into()),
    ]
}

/// Apply defaults without overriding anything the operator set.
pub fn apply_defaults(vars: &[(&'static str, String)]) {
    for (k, v) in vars {
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("wheeld-test-{}", std::process::id()));
        let p = p.join(format!("{:?}", std::time::SystemTime::now()).replace([' ', ':'], "_"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// The master key decrypts every project's secrets. A new one on each boot would silently
    /// orphan every board on the machine, so this is the single most important thing to persist.
    #[test]
    fn the_master_key_survives_a_restart() {
        let dir = tempdir();
        let first = Keys::load_or_create(&dir).unwrap().master_key;
        let second = Keys::load_or_create(&dir).unwrap().master_key;
        assert_eq!(first, second);
        assert!(!first.is_empty());
    }

    #[test]
    fn a_generated_master_key_is_32_bytes_of_base64() {
        let dir = tempdir();
        let k = Keys::load_or_create(&dir).unwrap().master_key;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&k)
            .unwrap();
        assert_eq!(bytes.len(), 32);
    }

    #[cfg(unix)]
    #[test]
    fn the_master_key_is_written_unreadable_by_anyone_else() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir();
        Keys::load_or_create(&dir).unwrap();
        let mode = std::fs::metadata(dir.join("master.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "master.key is readable by others: {mode:o}"
        );
    }

    /// The host secret authenticates one half of this process to the other and both halves die
    /// together, so it is generated per boot and never written down.
    #[test]
    fn the_host_secret_is_fresh_every_boot_and_not_persisted() {
        let dir = tempdir();
        let a = Keys::load_or_create(&dir).unwrap().host_secret;
        let b = Keys::load_or_create(&dir).unwrap().host_secret;
        assert_ne!(a, b);
        assert!(!dir.join("host.secret").exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_data_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().join("nested");
        prepare_data_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "data dir is reachable by others: {mode:o}");
    }

    /// The security-relevant half of what "local install" means. If any of these change, someone
    /// should have to change this test and say why.
    #[test]
    fn a_local_install_uses_local_accounts_and_the_strict_environment() {
        let dir = PathBuf::from("/tmp/wheeld-x");
        let keys = Keys {
            master_key: "mk".into(),
            host_secret: "hs".into(),
        };
        let env: std::collections::HashMap<_, _> = composed_env(&dir, &keys, "http://127.0.0.1:1")
            .into_iter()
            .collect();

        assert_eq!(env["AUTH_MODE"], "local");
        assert_eq!(
            env["WHEEL_ENV"], "prod",
            "dev is what unlocks the HS256 bypass"
        );
        assert_eq!(env["SANDBOX_BACKEND"], "process");
        assert!(!env.contains_key("AUTH_DEV_SECRET"));
        assert!(!env.contains_key("CLERK_JWKS_URL"));
    }

    #[test]
    fn defaults_never_override_what_the_operator_set() {
        std::env::set_var("WHEELD_TEST_KEY", "operator");
        apply_defaults(&[("WHEELD_TEST_KEY", "default".into())]);
        assert_eq!(std::env::var("WHEELD_TEST_KEY").unwrap(), "operator");

        std::env::remove_var("WHEELD_TEST_KEY");
        apply_defaults(&[("WHEELD_TEST_KEY", "default".into())]);
        assert_eq!(std::env::var("WHEELD_TEST_KEY").unwrap(), "default");
        std::env::remove_var("WHEELD_TEST_KEY");
    }
}
