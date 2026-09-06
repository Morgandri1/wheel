//! Git credentials for agent children, supplied out of band.
//!
//! An agent holding `GITHUB_TOKEN` will reach for the shortest thing that
//! works, and the shortest thing that works is
//! `git clone https://x-access-token:$TOKEN@github.com/...`. That writes a live
//! credential into `.git/config`, where it survives the process, is readable by
//! anything that can read the path, and falls out of a plain `git remote -v`.
//! It happened on the production volume (finding 036) and the token had to be
//! revoked.
//!
//! The engine cannot forbid it — an agent is untrusted code that legitimately
//! holds the token — so the fix is to make the safe path the easy one and to
//! clean up after the unsafe one:
//!
//! 1. every child gets `GIT_ASKPASS` pointing at a 0700 helper that reads the
//!    token from its ENVIRONMENT, so a plain `https://github.com/...` remote
//!    authenticates with nothing on disk and nothing in argv (argv is
//!    world-readable through `/proc`);
//! 2. on every start, remotes already poisoned are rewritten to the clean URL,
//!    because otherwise a clone made yesterday keeps yesterday's token for ever.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Environment variables an agent may hold a git credential in.
const TOKEN_VARS: [&str; 2] = ["GITHUB_TOKEN", "GH_TOKEN"];

/// Write the askpass helper into the node's run dir and return its path.
///
/// The helper reads the token from the environment it inherits. It never takes
/// the token as an argument: `/proc/<pid>/cmdline` is world-readable, so a
/// secret on a command line is a secret published to every other uid in the
/// sandbox.
pub fn write_askpass(run_dir: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = run_dir.join("askpass.sh");
    let script = "#!/bin/sh\n\
                  # Supplied by the Wheel engine. Reads the token from the environment;\n\
                  # never accepts it as an argument, because argv is world-readable.\n\
                  case \"$1\" in\n\
                  \x20 *[Uu]sername*) echo \"x-access-token\" ;;\n\
                  \x20 *[Pp]assword*) echo \"${GITHUB_TOKEN:-$GH_TOKEN}\" ;;\n\
                  esac\n";
    std::fs::write(&path, script)
        .with_context(|| format!("writing the git askpass helper at {}", path.display()))?;
    std::fs::set_permissions(&path, PermissionsExt::from_mode(0o700))
        .with_context(|| format!("restricting {}", path.display()))?;
    Ok(path)
}

/// Strip `user:password@` from a git remote URL, keeping everything else.
///
/// Returns `None` when there is nothing to strip, so a caller can tell "clean"
/// from "cleaned" and only rewrite what it must.
pub fn strip_credentials(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let (userinfo, host) = rest.split_once('@')?;
    // An '@' after the first '/' is part of a path, not userinfo.
    if userinfo.contains('/') {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

/// Rewrite any credential-bearing remote under `root` to its clean form.
///
/// Returns how many URLs were rewritten. Existing clones are the reason this
/// exists: fixing how we hand out credentials does nothing about the token
/// already sitting in a `.git/config` from last week.
pub fn sanitise_remotes(root: &Path) -> Result<usize> {
    let mut cleaned = 0;
    for config in git_configs(root, 0) {
        let Ok(text) = std::fs::read_to_string(&config) else {
            continue;
        };
        let mut out = String::with_capacity(text.len());
        let mut changed = false;
        for line in text.lines() {
            let trimmed = line.trim_start();
            let rewritten = trimmed
                .strip_prefix("url = ")
                .and_then(strip_credentials)
                .map(|clean| {
                    changed = true;
                    cleaned += 1;
                    let indent = &line[..line.len() - trimmed.len()];
                    format!("{indent}url = {clean}")
                });
            out.push_str(&rewritten.unwrap_or_else(|| line.to_string()));
            out.push('\n');
        }
        if changed {
            // Written in place: the file's own permissions are git's business,
            // and a temp file plus rename would change ownership under a uid
            // that is not ours.
            std::fs::write(&config, out)
                .with_context(|| format!("rewriting {}", config.display()))?;
            tracing::warn!(
                config = %config.display(),
                "removed a credential from a git remote URL; rotate that token"
            );
        }
    }
    Ok(cleaned)
}

/// Every `.git/config` under `root`, to a bounded depth.
///
/// Bounded because this runs on every agent start and a workspace is a tree an
/// agent controls: an unbounded walk is something an agent can make expensive.
fn git_configs(dir: &Path, depth: usize) -> Vec<PathBuf> {
    const MAX_DEPTH: usize = 6;
    let mut found = Vec::new();
    if depth > MAX_DEPTH {
        return found;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Do not follow symlinks: an agent could point one at another node's
        // directory and have us rewrite files that are not ours.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if path.file_name().is_some_and(|n| n == ".git") {
                let cfg = path.join("config");
                if cfg.is_file() {
                    found.push(cfg);
                }
                continue;
            }
            found.extend(git_configs(&path, depth + 1));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape found live on the production volume (finding 036).
    #[test]
    fn a_credential_is_stripped_from_a_remote_url() {
        let poisoned = "https://x-access-token:github_pat_LIVE@github.com/Morgandri1/wheel.git";
        assert_eq!(
            strip_credentials(poisoned).as_deref(),
            Some("https://github.com/Morgandri1/wheel.git")
        );
    }

    /// A clean URL is left exactly alone, so a rewrite only ever happens to a
    /// file that needs it.
    #[test]
    fn a_clean_remote_is_not_touched() {
        assert_eq!(
            strip_credentials("https://github.com/Morgandri1/wheel.git"),
            None
        );
        assert_eq!(
            strip_credentials("git@github.com:Morgandri1/wheel.git"),
            None
        );
        // An '@' in the PATH is not userinfo and must not be mistaken for it.
        assert_eq!(
            strip_credentials("https://example.com/a/@scope/pkg.git"),
            None
        );
    }

    /// PM's assertion, on disk rather than in a string: after this runs, the
    /// token is not in `.git/config` and would not fall out of `git remote -v`.
    #[test]
    fn an_existing_poisoned_clone_is_repaired_in_place() {
        let root = std::env::temp_dir().join(format!("wheel-gitcreds-{}", std::process::id()));
        let git = root.join("wheel").join(".git");
        std::fs::create_dir_all(&git).unwrap();
        let config = git.join("config");
        std::fs::write(
            &config,
            "[core]\n\trepositoryformatversion = 0\n\
             [remote \"origin\"]\n\
             \turl = https://x-access-token:github_pat_LIVE@github.com/Morgandri1/wheel.git\n\
             \tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        )
        .unwrap();

        assert_eq!(sanitise_remotes(&root).unwrap(), 1);

        let after = std::fs::read_to_string(&config).unwrap();
        assert!(
            !after.contains("github_pat_LIVE"),
            "the token survived:\n{after}"
        );
        assert!(
            after.contains("url = https://github.com/Morgandri1/wheel.git"),
            "{after}"
        );
        // The rest of the file is intact — this repairs a URL, it does not
        // rewrite git's configuration.
        assert!(after.contains("repositoryformatversion = 0"), "{after}");
        assert!(after.contains("fetch = +refs/heads/*"), "{after}");

        // Idempotent: a second start must not keep reporting a repair.
        assert_eq!(sanitise_remotes(&root).unwrap(), 0);

        std::fs::remove_dir_all(&root).ok();
    }

    /// A symlink is not followed: an agent could otherwise point one at another
    /// node's directory and have the engine rewrite files that are not its own.
    #[test]
    fn a_symlinked_directory_is_not_walked() {
        let root = std::env::temp_dir().join(format!("wheel-gitlink-{}", std::process::id()));
        let outside = root.join("outside").join(".git");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("config"),
            "[remote \"origin\"]\n\turl = https://u:p@github.com/x/y.git\n",
        )
        .unwrap();
        let ws = root.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::os::unix::fs::symlink(root.join("outside"), ws.join("link")).unwrap();

        assert_eq!(
            sanitise_remotes(&ws).unwrap(),
            0,
            "the walk followed a symlink out of the workspace"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// The helper must read the token from the environment, never take it as an
    /// argument: `/proc/<pid>/cmdline` is readable by every uid in the sandbox,
    /// so a token in argv is a token published.
    #[test]
    fn the_askpass_helper_reads_the_token_from_the_environment() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("wheel-askpass-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let helper = write_askpass(&dir).unwrap();
        let mode = std::fs::metadata(&helper).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "the helper is readable by other uids");

        let script = std::fs::read_to_string(&helper).unwrap();
        assert!(script.contains("GITHUB_TOKEN"), "{script}");
        assert!(
            !script.contains("github_pat"),
            "a literal token was baked in"
        );

        // It actually answers git's two questions.
        let out = std::process::Command::new(&helper)
            .arg("Password for 'https://x-access-token@github.com': ")
            .env("GITHUB_TOKEN", "tok-from-env")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "tok-from-env");

        let user = std::process::Command::new(&helper)
            .arg("Username for 'https://github.com': ")
            .env("GITHUB_TOKEN", "tok-from-env")
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&user.stdout).trim(),
            "x-access-token"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Named so the sweep is a property rather than a habit: no token variable
    /// this engine knows about may be spelled into a URL by us.
    #[test]
    fn the_engine_never_builds_a_url_containing_a_token() {
        let src = include_str!("mod.rs");
        for var in TOKEN_VARS {
            assert!(
                !src.contains(&format!("{var}@")),
                "the supervisor interpolates {var} into a URL"
            );
        }
        assert!(
            !src.contains("x-access-token:"),
            "a credential URL is built here"
        );
    }
}
