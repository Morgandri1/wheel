// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! How a deployment moves to a new build. Driver #1 is `source`: a git checkout
//! and cargo. Railway and Docker drivers implement the same trait later
//! (docs/proposals/auto-update.md, "Drivers").

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::policy::{Policy, BINARIES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// The target is what runs, or behind it: nothing to do.
    Current,
    FastForward,
    /// Not reachable by a fast-forward from what runs, or the checkout itself
    /// is not an ancestor of the target.
    Diverged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    pub relation: Relation,
    pub clean: bool,
    pub commits: u32,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub dir: PathBuf,
    /// Binary name, and where the build left it.
    pub binaries: Vec<(String, PathBuf)>,
}

impl Staged {
    fn binary(&self, name: &str) -> Result<&Path> {
        self.binaries
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, p)| p.as_path())
            .with_context(|| format!("the staged build has no {name}"))
    }
}

pub trait UpdateDriver: Send + Sync {
    fn name(&self) -> &'static str;
    /// What the checkout is at, for a running build that was never stamped.
    fn head(&self) -> Result<String>;
    fn fetch(&self) -> Result<()>;
    fn target(&self) -> Result<String>;
    fn inspect(&self, running: &str, target: &str) -> Result<Inspection>;
    fn build(&self, target: &str) -> Result<Staged>;
    fn smoke(&self, staged: &Staged, target: &str) -> Result<()>;
    /// Install the staged build, keeping what it replaces.
    fn swap(&self, staged: &Staged) -> Result<()>;
    /// Put back what the last swap replaced.
    fn rollback(&self) -> Result<()>;
    /// After a healthy restart: make the source match what runs.
    fn settle(&self, target: &str) -> Result<()>;
}

/// git, hardened against the checkout's own configuration.
///
/// A hook or an `fsmonitor` command in `.git/config` runs with wheeld's
/// authority on the next fetch, and the checkout is a directory an agent on a
/// shared-uid install can reach. So hooks and fsmonitor are switched off on the
/// command line, where repository config cannot turn them back on, and the
/// fetch names its refspec rather than trusting `remote.origin.fetch`.
pub struct Git {
    repo: PathBuf,
}

impl Git {
    pub fn new(repo: impl Into<PathBuf>) -> Self {
        Self { repo: repo.into() }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.repo)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "protocol.ext.allow=never",
            ])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let out = self
            .command()
            .args(args)
            .output()
            .with_context(|| format!("running git {}", args.join(" ")))?;
        if !out.status.success() {
            bail!(
                "git {} failed ({}): {}",
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn fetch(&self) -> Result<()> {
        self.run(&[
            "fetch",
            "--quiet",
            "--no-tags",
            "origin",
            "+refs/heads/main:refs/remotes/origin/main",
        ])
        .map(drop)
    }

    pub fn commit(&self, rev: &str) -> Result<String> {
        let sha = self.run(&["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")])?;
        wheel_core::Sha::parse(&sha)
            .map(|s| s.as_str().to_string())
            .with_context(|| format!("git named {rev} as {sha:?}, which is not a commit id"))
    }

    pub fn is_ancestor(&self, ancestor: &str, of: &str) -> Result<bool> {
        let status = self
            .command()
            .args(["merge-base", "--is-ancestor", ancestor, of])
            .status()
            .context("running git merge-base")?;
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => bail!("git merge-base --is-ancestor {ancestor} {of} failed ({status})"),
        }
    }

    pub fn changed_paths(&self, from: &str, to: &str) -> Result<Vec<String>> {
        Ok(self
            .run(&["diff", "--name-only", "--no-renames", "-z", from, to])?
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect())
    }

    pub fn count(&self, from: &str, to: &str) -> Result<u32> {
        let n = self.run(&["rev-list", "--count", &format!("{from}..{to}")])?;
        n.parse()
            .with_context(|| format!("git rev-list --count said {n:?}"))
    }

    pub fn is_clean(&self) -> Result<bool> {
        Ok(self
            .run(&["status", "--porcelain", "--untracked-files=normal"])?
            .is_empty())
    }

    /// Exactly the tree of `sha`, and nothing else from the checkout: no
    /// untracked file, no local edit and no `.git` reaches the build.
    pub fn archive(&self, sha: &str, dest: &Path) -> Result<()> {
        let mut git = self
            .command()
            .args(["archive", "--format=tar", sha])
            .stdout(Stdio::piped())
            .spawn()
            .context("running git archive")?;
        let tar = Command::new("tar")
            .args(["-x", "-f", "-", "-C"])
            .arg(dest)
            .stdin(git.stdout.take().context("git archive has no stdout")?)
            .status()
            .context("running tar")?;
        let archived = git.wait().context("waiting for git archive")?;
        if !archived.success() || !tar.success() {
            bail!("extracting {sha}: git archive {archived}, tar {tar}");
        }
        Ok(())
    }

    pub fn fast_forward(&self, sha: &str) -> Result<()> {
        self.run(&["merge", "--ff-only", "--quiet", sha]).map(drop)
    }

    pub fn origin_url(&self) -> Result<String> {
        self.run(&["remote", "get-url", "origin"])
    }
}

pub struct SourceDriver {
    git: Git,
    staging: PathBuf,
    bin_dir: PathBuf,
    cargo: PathBuf,
}

impl SourceDriver {
    pub fn new(policy: &Policy) -> Self {
        Self {
            git: Git::new(&policy.repo),
            staging: policy.staging.clone(),
            bin_dir: policy.bin_dir.clone(),
            cargo: PathBuf::from("cargo"),
        }
    }

    pub fn with_cargo(mut self, cargo: impl Into<PathBuf>) -> Self {
        self.cargo = cargo.into();
        self
    }

    pub fn git(&self) -> &Git {
        &self.git
    }
}

impl UpdateDriver for SourceDriver {
    fn name(&self) -> &'static str {
        "source"
    }

    fn head(&self) -> Result<String> {
        self.git.commit("HEAD")
    }

    fn fetch(&self) -> Result<()> {
        self.git.fetch()
    }

    fn target(&self) -> Result<String> {
        self.git.commit("refs/remotes/origin/main")
    }

    fn inspect(&self, running: &str, target: &str) -> Result<Inspection> {
        let clean = self.git.is_clean()?;
        let known = self.git.commit(running).is_ok();
        if !known {
            return Ok(Inspection {
                relation: Relation::Diverged,
                clean,
                commits: 0,
                paths: Vec::new(),
            });
        }
        if self.git.is_ancestor(target, running)? {
            return Ok(Inspection {
                relation: Relation::Current,
                clean,
                commits: 0,
                paths: Vec::new(),
            });
        }
        let fast_forward =
            self.git.is_ancestor(running, target)? && self.git.is_ancestor("HEAD", target)?;
        Ok(Inspection {
            relation: if fast_forward {
                Relation::FastForward
            } else {
                Relation::Diverged
            },
            clean,
            commits: self.git.count(running, target)?,
            paths: self.git.changed_paths(running, target)?,
        })
    }

    fn build(&self, target: &str) -> Result<Staged> {
        let short = &target[..target.len().min(12)];
        let src = self.staging.join(format!("src-{short}"));
        if src.exists() {
            std::fs::remove_dir_all(&src)
                .with_context(|| format!("clearing {}", src.display()))?;
        }
        std::fs::create_dir_all(&src).with_context(|| format!("creating {}", src.display()))?;
        self.git.archive(target, &src)?;

        // Private to the updater, and reused between updates: builds are one at
        // a time, so nothing else can link against a half-written artefact.
        let target_dir = self.staging.join("target");
        let status = Command::new(&self.cargo)
            .args([
                "build",
                "--release",
                "--locked",
                "-p",
                "wheeld",
                "-p",
                "wheel-cli",
            ])
            .current_dir(&src)
            .env("CARGO_TARGET_DIR", &target_dir)
            .env("WHEEL_BUILD_SHA", target)
            .stdin(Stdio::null())
            .status()
            .with_context(|| format!("running {}", self.cargo.display()))?;
        if !status.success() {
            bail!("cargo build of {target} failed ({status})");
        }

        let release = target_dir.join("release");
        let binaries: Vec<(String, PathBuf)> = BINARIES
            .iter()
            .map(|b| (b.to_string(), release.join(b)))
            .collect();
        for (name, path) in &binaries {
            if !path.is_file() {
                bail!("the build of {target} produced no {name} at {}", path.display());
            }
        }
        Ok(Staged { dir: src, binaries })
    }

    fn smoke(&self, staged: &Staged, target: &str) -> Result<()> {
        let out = Command::new(staged.binary("wheeld")?)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .context("running the new wheeld --version")?;
        let version = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            bail!("the new wheeld --version exited {}", out.status);
        }
        if !version.contains(target) {
            bail!(
                "the new wheeld reports {:?}, not {target}: this is not the commit that was verified",
                version.trim()
            );
        }
        let help = Command::new(staged.binary("wheel")?)
            .arg("--help")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("running the new wheel --help")?;
        if !help.success() {
            bail!("the new wheel --help exited {help}");
        }
        Ok(())
    }

    fn swap(&self, staged: &Staged) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        for (name, built) in &staged.binaries {
            let live = self.bin_dir.join(name);
            let fresh = self.bin_dir.join(format!(".{name}.new"));
            let prev = self.bin_dir.join(format!("{name}.prev"));
            std::fs::copy(built, &fresh)
                .with_context(|| format!("copying {} into place", built.display()))?;
            std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755))?;
            match std::fs::remove_file(&prev) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("removing {}", prev.display())),
            }
            std::fs::hard_link(&live, &prev)
                .with_context(|| format!("keeping {} as {}", live.display(), prev.display()))?;
            // rename(2) is atomic: whoever execs `live` gets the old binary or
            // the new one, never a half-copied file.
            std::fs::rename(&fresh, &live)
                .with_context(|| format!("installing {}", live.display()))?;
        }
        Ok(())
    }

    fn rollback(&self) -> Result<()> {
        let mut restored = 0;
        for name in BINARIES {
            let prev = self.bin_dir.join(format!("{name}.prev"));
            if prev.exists() {
                std::fs::rename(&prev, self.bin_dir.join(name))
                    .with_context(|| format!("restoring {}", prev.display()))?;
                restored += 1;
            }
        }
        if restored == 0 {
            bail!("nothing to roll back to in {}", self.bin_dir.display());
        }
        Ok(())
    }

    fn settle(&self, target: &str) -> Result<()> {
        if self.git.is_clean()? && self.git.is_ancestor("HEAD", target)? {
            self.git.fast_forward(target)?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A bare `origin`, the checkout wheeld updates from, and a second clone to
    /// push from — the board's agents, in other words.
    pub(crate) struct Repos {
        pub root: PathBuf,
        pub checkout: PathBuf,
        pub dev: PathBuf,
    }

    impl Drop for Repos {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.email=t@example.test",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    pub(crate) fn commit(dir: &Path, files: &[(&str, &str)], msg: &str) -> String {
        for (path, body) in files {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", msg]);
        git(dir, &["rev-parse", "HEAD"])
    }

    pub(crate) fn repos() -> (Repos, String) {
        let root = std::env::temp_dir().join(format!("wheeld-git-{}", uuid::Uuid::new_v4()));
        let origin = root.join("origin.git");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        let dev = root.join("dev");
        git(&root, &["clone", "-q", origin.to_str().unwrap(), "dev"]);
        git(&dev, &["checkout", "-q", "-b", "main"]);
        let first = commit(
            &dev,
            &[("Cargo.toml", "[workspace]\n"), ("docs/a.md", "a\n")],
            "first",
        );
        git(&dev, &["push", "-q", "origin", "main"]);
        git(&root, &["clone", "-q", origin.to_str().unwrap(), "checkout"]);
        let checkout = root.join("checkout");
        for d in [&root, &checkout] {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (
            Repos {
                root,
                checkout,
                dev,
            },
            first,
        )
    }

    pub(crate) fn driver(r: &Repos) -> SourceDriver {
        let bin = r.root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        SourceDriver {
            git: Git::new(&r.checkout),
            staging: r.root.join("staging"),
            bin_dir: bin,
            cargo: PathBuf::from("cargo"),
        }
    }

    #[test]
    fn a_fast_forward_reports_its_commits_and_the_paths_it_changes() {
        let (r, first) = repos();
        let d = driver(&r);
        assert_eq!(d.head().unwrap(), first);
        let second = commit(&r.dev, &[("crates/wheel-engine/src/lib.rs", "x")], "engine");
        let third = commit(&r.dev, &[("docs/b.md", "b")], "docs");
        git(&r.dev, &["push", "-q", "origin", "main"]);

        d.fetch().unwrap();
        assert_eq!(d.target().unwrap(), third);
        let i = d.inspect(&first, &third).unwrap();
        assert_eq!(i.relation, Relation::FastForward);
        assert!(i.clean);
        assert_eq!(i.commits, 2);
        assert_eq!(i.paths, vec!["crates/wheel-engine/src/lib.rs", "docs/b.md"]);

        assert_eq!(d.inspect(&third, &third).unwrap().relation, Relation::Current);
        assert_eq!(d.inspect(&third, &second).unwrap().relation, Relation::Current);
        assert!(d.git().origin_url().unwrap().ends_with("origin.git"));
    }

    /// The target must be reachable from what runs by a fast-forward. A rewrite
    /// of main, a running build nobody pushed, or a checkout on another line are
    /// all refused, never merged over.
    #[test]
    fn only_a_fast_forward_is_ever_a_fast_forward() {
        let (r, first) = repos();
        let d = driver(&r);

        let local = commit(&r.checkout, &[("x.txt", "local")], "unpushed");
        commit(&r.dev, &[("crates/wheel-cli/src/main.rs", "y")], "cli");
        git(&r.dev, &["push", "-q", "origin", "main"]);
        d.fetch().unwrap();
        let target = d.target().unwrap();
        assert_eq!(
            d.inspect(&first, &target).unwrap().relation,
            Relation::Diverged,
            "the checkout holds a commit main does not"
        );
        assert_eq!(d.inspect(&local, &target).unwrap().relation, Relation::Diverged);

        git(&r.checkout, &["reset", "-q", "--hard", &first]);
        assert_eq!(d.inspect(&first, &target).unwrap().relation, Relation::FastForward);

        git(&r.dev, &["reset", "-q", "--hard", &first]);
        commit(&r.dev, &[("crates/wheel-core/src/lib.rs", "z")], "rewrite");
        git(&r.dev, &["push", "-q", "--force", "origin", "main"]);
        d.fetch().unwrap();
        let rewritten = d.target().unwrap();
        assert_eq!(
            d.inspect(&target, &rewritten).unwrap().relation,
            Relation::Diverged,
            "a force-pushed main is not a fast-forward of what runs"
        );

        let unknown = "0".repeat(40);
        let i = d.inspect(&unknown, &rewritten).unwrap();
        assert_eq!((i.relation, i.commits), (Relation::Diverged, 0));
    }

    #[test]
    fn a_dirty_checkout_is_reported_as_dirty() {
        let (r, first) = repos();
        let d = driver(&r);
        std::fs::write(r.checkout.join("untracked.txt"), "x").unwrap();
        assert!(!d.inspect(&first, &first).unwrap().clean);
        std::fs::remove_file(r.checkout.join("untracked.txt")).unwrap();
        std::fs::write(r.checkout.join("Cargo.toml"), "edited").unwrap();
        assert!(!d.inspect(&first, &first).unwrap().clean);
    }

    /// A hook planted in the checkout must not run with wheeld's authority.
    /// `reference-transaction` fires on every ref a fetch updates.
    #[test]
    fn a_hook_planted_in_the_checkout_does_not_run() {
        let (r, _) = repos();
        let d = driver(&r);
        let proof = r.root.join("hook-ran");
        let hook = r.checkout.join(".git/hooks/reference-transaction");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", proof.display())).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        commit(&r.dev, &[("crates/wheel-engine/a", "1")], "more");
        git(&r.dev, &["push", "-q", "origin", "main"]);

        d.fetch().unwrap();
        let target = d.target().unwrap();
        d.settle(&target).unwrap();
        assert_eq!(d.head().unwrap(), target, "settle fast-forwards the checkout");
        assert!(!proof.exists(), "a hook in the checkout ran during fetch or settle");
    }

    #[test]
    fn settle_leaves_a_dirty_or_diverged_checkout_alone() {
        let (r, first) = repos();
        let d = driver(&r);
        let target = commit(&r.dev, &[("crates/x/a", "1")], "more");
        git(&r.dev, &["push", "-q", "origin", "main"]);
        d.fetch().unwrap();
        std::fs::write(r.checkout.join("wip.txt"), "operator's work").unwrap();
        d.settle(&target).unwrap();
        assert_eq!(d.head().unwrap(), first);
    }

    /// Stands in for cargo: checks it was run in the archived tree with a
    /// private target dir, then leaves binaries that report `stamp`.
    pub(crate) fn fake_cargo(dir: &Path, stamp: &str) -> PathBuf {
        let path = dir.join("fake-cargo");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nset -e\ntest -f Cargo.toml\ntest ! -e .git\n\
                 case \"$CARGO_TARGET_DIR\" in */staging/target) ;; *) exit 9 ;; esac\n\
                 mkdir -p \"$CARGO_TARGET_DIR/release\"\n\
                 printf '#!/bin/sh\\necho \"wheeld 0.1.0 (%s)\"\\n' \"{stamp}\" > \"$CARGO_TARGET_DIR/release/wheeld\"\n\
                 printf '#!/bin/sh\\nexit 0\\n' > \"$CARGO_TARGET_DIR/release/wheel\"\n\
                 chmod +x \"$CARGO_TARGET_DIR/release/wheeld\" \"$CARGO_TARGET_DIR/release/wheel\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn a_build_uses_exactly_the_target_tree_and_the_smoke_test_proves_the_stamp() {
        let (r, first) = repos();
        std::fs::write(r.checkout.join("untracked-secret"), "x").unwrap();
        let d = driver(&r).with_cargo(fake_cargo(&r.root, &first));

        let staged = d.build(&first).unwrap();
        assert!(staged.dir.join("docs/a.md").exists());
        assert!(!staged.dir.join("untracked-secret").exists(), "only the committed tree");
        d.smoke(&staged, &first).unwrap();

        let other = "f".repeat(40);
        let e = d.smoke(&staged, &other).unwrap_err().to_string();
        assert!(e.contains("not the commit that was verified"), "{e}");

        let again = d.build(&first).unwrap();
        assert_eq!(again.dir, staged.dir, "a rebuild replaces the old source tree");
    }

    #[test]
    fn a_failed_or_empty_build_is_an_error() {
        let (r, first) = repos();
        let failing = r.root.join("false-cargo");
        std::fs::write(&failing, "#!/bin/sh\nexit 101\n").unwrap();
        std::fs::set_permissions(&failing, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = driver(&r).with_cargo(&failing).build(&first).unwrap_err();
        assert!(e.to_string().contains("cargo build"), "{e}");

        let empty = r.root.join("empty-cargo");
        std::fs::write(&empty, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&empty, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = driver(&r).with_cargo(&empty).build(&first).unwrap_err();
        assert!(e.to_string().contains("produced no"), "{e}");
    }

    #[test]
    fn a_swap_keeps_the_previous_binaries_and_rollback_restores_them() {
        let (r, first) = repos();
        let d = driver(&r).with_cargo(fake_cargo(&r.root, &first));
        for b in BINARIES {
            std::fs::write(d.bin_dir.join(b), format!("old {b}")).unwrap();
        }
        let staged = d.build(&first).unwrap();
        d.swap(&staged).unwrap();

        let read = |name: &str| std::fs::read_to_string(d.bin_dir.join(name)).unwrap();
        assert!(read("wheeld").contains(&first));
        assert_eq!(read("wheeld.prev"), "old wheeld");
        assert_eq!(read("wheel.prev"), "old wheel");
        let mode = std::fs::metadata(d.bin_dir.join("wheel")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);

        d.rollback().unwrap();
        assert_eq!(read("wheeld"), "old wheeld");
        assert_eq!(read("wheel"), "old wheel");
        assert!(d.rollback().is_err(), "a second rollback has nothing to restore");
        assert_eq!(d.name(), "source");
    }

    #[test]
    fn staged_names_what_it_does_not_have() {
        let s = Staged {
            dir: PathBuf::new(),
            binaries: vec![],
        };
        assert!(s.binary("wheeld").is_err());
    }
}
