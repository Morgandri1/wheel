// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `WHEEL_AUTO_UPDATE` and its companions: deployment-level, read once at boot,
//! and settable through no API (docs/proposals/auto-update.md, "Policy").
//!
//! Same tier as `WHEEL_HARNESS_AUTH`: a policy a project owner or an agent could
//! flip would not be a policy. A value wheeld does not understand fails boot and
//! names the variable, because a typo read as a default is a silent downgrade.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const ENV_MODE: &str = "WHEEL_AUTO_UPDATE";
pub const ENV_REPO: &str = "WHEEL_UPDATE_REPO";
pub const ENV_TOKEN: &str = "WHEEL_UPDATE_GITHUB_TOKEN";
pub const ENV_GITHUB_REPO: &str = "WHEEL_UPDATE_GITHUB_REPO";
pub const ENV_REQUIRED: &str = "WHEEL_UPDATE_REQUIRED_CHECKS";
pub const ENV_BIN_DIR: &str = "WHEEL_UPDATE_BIN_DIR";
pub const ENV_STAGING: &str = "WHEEL_UPDATE_STAGING";
pub const ENV_FETCH: &str = "WHEEL_UPDATE_FETCH_SECS";
pub const ENV_DRAIN: &str = "WHEEL_UPDATE_DRAIN_SECS";
pub const ENV_HEALTH: &str = "WHEEL_UPDATE_HEALTH_SECS";
pub const ENV_COOLDOWN: &str = "WHEEL_UPDATE_COOLDOWN_SECS";
pub const ENV_RESTART: &str = "WHEEL_UPDATE_RESTART";

/// What an update installs, and what must already be in the bin dir.
pub const BINARIES: [&str; 2] = ["wheeld", "wheel"];

const MIN_FETCH_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    Prompt,
    Auto,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Prompt => "prompt",
            Mode::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    /// Re-exec in place: same pid, so a supervisor sees nothing happen.
    Exec,
    /// Exit 75 for a supervisor with `Restart=always`.
    Exit,
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub mode: Mode,
    pub repo: PathBuf,
    pub github_token: Option<String>,
    pub github_repo: Option<String>,
    pub required_checks: Vec<String>,
    pub bin_dir: PathBuf,
    pub staging: PathBuf,
    pub fetch_every: Duration,
    pub drain_for: Duration,
    pub health_for: Duration,
    pub cooldown: Duration,
    pub restart: Restart,
}

pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

impl Policy {
    /// `None` when the policy is off, which is the default.
    pub fn from_env(data_dir: &Path) -> Result<Option<Policy>> {
        let exe = std::env::current_exe().context("locating the running wheeld")?;
        let exe_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
        Self::from_vars(&|k| std::env::var(k).ok(), data_dir, &exe_dir)
    }

    pub fn from_vars(get: Lookup, data_dir: &Path, exe_dir: &Path) -> Result<Option<Policy>> {
        let mode = mode(get)?;
        if mode == Mode::Off {
            return Ok(None);
        }

        let repo = PathBuf::from(set(get, ENV_REPO).with_context(|| {
            format!(
                "{ENV_MODE}={} needs {ENV_REPO}: the source checkout to update from",
                mode.as_str()
            )
        })?);
        if !repo.join(".git").exists() {
            bail!("{ENV_REPO}={} is not a git checkout", repo.display());
        }
        not_shared(&repo, ENV_REPO)?;

        let bin_dir = set(get, ENV_BIN_DIR)
            .map(PathBuf::from)
            .unwrap_or_else(|| exe_dir.to_path_buf());
        for bin in BINARIES {
            if !bin_dir.join(bin).is_file() {
                bail!(
                    "{ENV_BIN_DIR}={} has no `{bin}`: an update replaces wheeld and wheel together, \
                     so both must live there",
                    bin_dir.display()
                );
            }
        }
        not_shared(&bin_dir, ENV_BIN_DIR)?;

        let staging = match set(get, ENV_STAGING) {
            Some(s) => PathBuf::from(s),
            None => default_staging(get)?,
        };
        if !staging.is_absolute() {
            bail!("{ENV_STAGING}={} must be an absolute path", staging.display());
        }
        let data = data_dir
            .canonicalize()
            .unwrap_or_else(|_| data_dir.to_path_buf());
        let staged = staging.canonicalize().unwrap_or_else(|_| staging.clone());
        if staged.starts_with(&data) {
            bail!(
                "{ENV_STAGING}={} is inside the data directory: builds must stay off the data volume",
                staging.display()
            );
        }

        let fetch_every = secs(get, ENV_FETCH, 300)?;
        if fetch_every < Duration::from_secs(MIN_FETCH_SECS) {
            bail!("{ENV_FETCH} must be at least {MIN_FETCH_SECS}");
        }

        let restart = match set(get, ENV_RESTART).as_deref() {
            None | Some("exec") => Restart::Exec,
            Some("exit") => Restart::Exit,
            Some(other) => bail!("{ENV_RESTART}={other:?} is not exec or exit"),
        };

        let required_checks: Vec<String> = set(get, ENV_REQUIRED)
            .unwrap_or_else(|| "make check".into())
            .split(',')
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .collect();

        Ok(Some(Policy {
            mode,
            repo,
            github_token: set(get, ENV_TOKEN),
            github_repo: set(get, ENV_GITHUB_REPO),
            required_checks,
            bin_dir,
            staging,
            fetch_every,
            drain_for: secs(get, ENV_DRAIN, 600)?,
            health_for: secs(get, ENV_HEALTH, 60)?,
            cooldown: secs(get, ENV_COOLDOWN, 600)?,
            restart,
        }))
    }
}

pub fn mode(get: Lookup) -> Result<Mode> {
    match set(get, ENV_MODE).as_deref() {
        None | Some("off") => Ok(Mode::Off),
        Some("prompt") => Ok(Mode::Prompt),
        Some("auto") => Ok(Mode::Auto),
        Some(other) => bail!(
            "{ENV_MODE}={other:?} is not a value wheeld understands (want off, prompt or auto)"
        ),
    }
}

fn set(get: Lookup, key: &str) -> Option<String> {
    get(key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn secs(get: Lookup, key: &str, default: u64) -> Result<Duration> {
    match set(get, key) {
        None => Ok(Duration::from_secs(default)),
        Some(v) => v
            .parse::<u64>()
            .map(Duration::from_secs)
            .with_context(|| format!("{key}={v:?} must be a whole number of seconds")),
    }
}

fn default_staging(get: Lookup) -> Result<PathBuf> {
    if let Some(cache) = set(get, "XDG_CACHE_HOME") {
        return Ok(PathBuf::from(cache).join("wheel-update"));
    }
    let home = set(get, "HOME")
        .with_context(|| format!("no HOME to stage builds under — set {ENV_STAGING}"))?;
    Ok(PathBuf::from(home).join(".cache").join("wheel-update"))
}

/// T7: a checkout or bin dir another user can write is one where anyone chooses
/// what wheeld becomes on its next update.
fn not_shared(path: &Path, key: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .with_context(|| format!("reading {key}={}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o022 != 0 {
        bail!(
            "{key}={} is writable by other users (mode {:o}), so an update would build or install \
             whatever they put there",
            path.display(),
            mode & 0o777
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    struct Dirs {
        root: PathBuf,
        repo: PathBuf,
        bin: PathBuf,
        data: PathBuf,
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    fn dirs() -> Dirs {
        let root = std::env::temp_dir().join(format!("wheeld-policy-{}", uuid::Uuid::new_v4()));
        let (repo, bin, data) = (root.join("repo"), root.join("bin"), root.join("data"));
        for d in [&repo.join(".git"), &bin, &data] {
            std::fs::create_dir_all(d).unwrap();
        }
        for d in [&root, &repo, &bin, &data] {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        for b in BINARIES {
            std::fs::write(bin.join(b), "").unwrap();
        }
        Dirs {
            root,
            repo,
            bin,
            data,
        }
    }

    fn parse(d: &Dirs, vars: &[(&str, &str)]) -> Result<Option<Policy>> {
        let mut env: HashMap<String, String> = [
            (ENV_MODE, "prompt"),
            (ENV_REPO, d.repo.to_str().unwrap()),
            ("HOME", d.root.to_str().unwrap()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        for (k, v) in vars {
            env.insert(k.to_string(), v.to_string());
        }
        Policy::from_vars(&|k| env.get(k).cloned(), &d.data, &d.bin)
    }

    fn refusal(d: &Dirs, vars: &[(&str, &str)]) -> String {
        format!("{:#}", parse(d, vars).unwrap_err())
    }

    #[test]
    fn unset_blank_and_off_all_mean_off() {
        let d = dirs();
        for v in ["", "  ", "off"] {
            assert!(parse(&d, &[(ENV_MODE, v)]).unwrap().is_none(), "{v:?}");
        }
        let none: HashMap<String, String> = HashMap::new();
        assert!(Policy::from_vars(&|k| none.get(k).cloned(), &d.data, &d.bin)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_value_wheeld_does_not_understand_fails_boot_and_names_the_variable() {
        let d = dirs();
        for bad in ["on", "yes", "PROMPT", "automatic"] {
            let e = refusal(&d, &[(ENV_MODE, bad)]);
            assert!(e.contains(ENV_MODE) && e.contains(bad), "{e}");
        }
    }

    #[test]
    fn the_defaults_are_a_working_prompt_policy() {
        let d = dirs();
        let p = parse(&d, &[]).unwrap().unwrap();
        assert_eq!(p.mode, Mode::Prompt);
        assert_eq!(p.bin_dir, d.bin);
        assert_eq!(p.staging, d.root.join(".cache/wheel-update"));
        assert_eq!(p.required_checks, vec!["make check".to_string()]);
        assert_eq!(p.fetch_every, Duration::from_secs(300));
        assert_eq!(p.drain_for, Duration::from_secs(600));
        assert_eq!(p.health_for, Duration::from_secs(60));
        assert_eq!(p.cooldown, Duration::from_secs(600));
        assert_eq!(p.restart, Restart::Exec);
        assert_eq!(p.github_token, None);
    }

    #[test]
    fn every_knob_is_read() {
        let d = dirs();
        let cache = d.root.join("xdg");
        let p = parse(
            &d,
            &[
                (ENV_MODE, "auto"),
                (ENV_TOKEN, " ghs_x "),
                (ENV_GITHUB_REPO, "o/r"),
                (ENV_REQUIRED, "make check, integration ,,"),
                ("XDG_CACHE_HOME", cache.to_str().unwrap()),
                (ENV_FETCH, "120"),
                (ENV_DRAIN, "30"),
                (ENV_HEALTH, "5"),
                (ENV_COOLDOWN, "0"),
                (ENV_RESTART, "exit"),
            ],
        )
        .unwrap()
        .unwrap();
        assert_eq!(p.mode, Mode::Auto);
        assert_eq!(p.github_token.as_deref(), Some("ghs_x"));
        assert_eq!(p.github_repo.as_deref(), Some("o/r"));
        assert_eq!(p.required_checks, vec!["make check", "integration"]);
        assert_eq!(p.staging, cache.join("wheel-update"));
        assert_eq!(p.fetch_every, Duration::from_secs(120));
        assert_eq!(p.drain_for, Duration::from_secs(30));
        assert_eq!(p.health_for, Duration::from_secs(5));
        assert_eq!(p.cooldown, Duration::ZERO);
        assert_eq!(p.restart, Restart::Exit);
        assert_eq!(Mode::Auto.as_str(), "auto");
    }

    #[test]
    fn a_policy_without_a_usable_checkout_is_refused() {
        let d = dirs();
        let e = refusal(&d, &[(ENV_REPO, "")]);
        assert!(e.contains(ENV_REPO), "{e}");
        let e = refusal(&d, &[(ENV_REPO, d.bin.to_str().unwrap())]);
        assert!(e.contains("not a git checkout"), "{e}");
    }

    /// T7: another user's write access to the checkout or the bin dir is a
    /// choice of what wheeld becomes.
    #[test]
    fn a_checkout_or_bin_dir_other_users_can_write_is_refused() {
        let d = dirs();
        std::fs::set_permissions(&d.repo, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(refusal(&d, &[]).contains("writable by other users"));
        std::fs::set_permissions(&d.repo, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&d.bin, std::fs::Permissions::from_mode(0o775)).unwrap();
        let e = refusal(&d, &[]);
        assert!(e.contains(ENV_BIN_DIR) && e.contains("writable"), "{e}");
    }

    #[test]
    fn a_bin_dir_missing_either_binary_is_refused() {
        let d = dirs();
        std::fs::remove_file(d.bin.join("wheel")).unwrap();
        assert!(refusal(&d, &[]).contains("no `wheel`"));
    }

    #[test]
    fn builds_stay_off_the_data_volume() {
        let d = dirs();
        let inside = d.data.join("staging");
        let e = refusal(&d, &[(ENV_STAGING, inside.to_str().unwrap())]);
        assert!(e.contains("data directory"), "{e}");
        let e = refusal(&d, &[(ENV_STAGING, "relative/path")]);
        assert!(e.contains("absolute"), "{e}");
    }

    #[test]
    fn numbers_and_the_restart_mode_are_checked_not_guessed() {
        let d = dirs();
        assert!(refusal(&d, &[(ENV_FETCH, "59")]).contains("at least 60"));
        let e = refusal(&d, &[(ENV_DRAIN, "ten")]);
        assert!(e.contains(ENV_DRAIN) && e.contains("ten"), "{e}");
        assert!(refusal(&d, &[(ENV_RESTART, "reboot")]).contains(ENV_RESTART));
    }

    #[test]
    fn with_no_home_and_no_staging_the_error_names_the_fix() {
        let d = dirs();
        let env: HashMap<String, String> = [
            (ENV_MODE.to_string(), "prompt".to_string()),
            (ENV_REPO.to_string(), d.repo.display().to_string()),
        ]
        .into_iter()
        .collect();
        let e = Policy::from_vars(&|k| env.get(k).cloned(), &d.data, &d.bin).unwrap_err();
        assert!(format!("{e:#}").contains(ENV_STAGING));
    }
}
