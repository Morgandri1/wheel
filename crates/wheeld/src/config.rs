//! Settings for the single-process daemon: two flags, and defaults for everything else.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

pub const USAGE: &str = "\
wheeld — Wheel in one process: API, sandbox host, and per-project engines.

USAGE:
    wheeld [--data-dir <path>] [--bind <addr>]

OPTIONS:
    --data-dir <path>   Where boards, secrets and project data live.
                        Default: $WHEEL_DATA_DIR, else ~/.wheel
    --bind <addr>       Address to serve on. Default: $BIND_ADDR, else 0.0.0.0:8080
    -h, --help          Print this message
    -V, --version       Print the version

Everything else is configured for you: local email/password accounts, a sqlite
store, and one sandboxed engine per project. Open http://localhost:8080 and sign up.
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub data_dir: PathBuf,
    pub bind: String,
}

/// What `main` should do, decided from the arguments before anything is started.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Run(Settings),
    PrintUsage,
    PrintVersion,
}

impl Settings {
    /// Parse arguments (excluding argv[0]).
    ///
    /// Environment variables supply the defaults, so `wheeld` with no flags works and a
    /// containerised deployment can configure it the way it configures everything else.
    pub fn parse<I, S>(args: I) -> Result<Action>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut data_dir: Option<PathBuf> = None;
        let mut bind: Option<String> = None;
        let mut it = args.into_iter().map(|s| s.as_ref().to_string());

        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => return Ok(Action::PrintUsage),
                "-V" | "--version" => return Ok(Action::PrintVersion),
                "--data-dir" => {
                    let v = it.next().context("--data-dir needs a path")?;
                    data_dir = Some(PathBuf::from(v));
                }
                "--bind" => {
                    bind = Some(it.next().context("--bind needs an address")?);
                }
                other => {
                    if let Some(v) = other.strip_prefix("--data-dir=") {
                        data_dir = Some(PathBuf::from(v));
                    } else if let Some(v) = other.strip_prefix("--bind=") {
                        bind = Some(v.to_string());
                    } else {
                        bail!("unknown argument {other:?}\n\n{USAGE}");
                    }
                }
            }
        }

        let data_dir = data_dir
            .or_else(|| std::env::var("WHEEL_DATA_DIR").ok().map(PathBuf::from))
            .map(Ok)
            .unwrap_or_else(default_data_dir)?;
        let bind = bind
            .or_else(|| std::env::var("BIND_ADDR").ok())
            .unwrap_or_else(|| "0.0.0.0:8080".to_string());

        Ok(Action::Run(Settings { data_dir, bind }))
    }
}

/// `~/.wheel`, or an error that says what to pass instead.
///
/// Falling back to the working directory would scatter a `.wheel` beside whatever the user happened
/// to be in, and each one would look like a different install with different projects.
fn default_data_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .context("no HOME to put the data directory in — pass --data-dir")?;
    Ok(PathBuf::from(home).join(".wheel"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Settings {
        match Settings::parse(args).unwrap() {
            Action::Run(s) => s,
            other => panic!("expected a run, got {other:?}"),
        }
    }

    /// Every case that reads or writes the environment, in one test.
    ///
    /// Environment variables are process-global and Rust runs tests in parallel threads, so these
    /// as separate `#[test]`s race: one clears `WHEEL_DATA_DIR` while another sets it, and which
    /// one wins depends on timing. It failed that way once here before being noticed, which is the
    /// worst kind of test — it passes on rerun and teaches you to ignore it. Sequencing the cases
    /// inside a single test makes the interference impossible rather than unlikely. Same reasoning,
    /// and same shape, as wheel-api's config_interlock.
    #[test]
    fn configuration_comes_from_flags_then_environment_then_defaults() {
        fn clear() {
            std::env::remove_var("WHEEL_DATA_DIR");
            std::env::remove_var("BIND_ADDR");
        }

        // Nothing set: the defaults are a working configuration on their own, which is the whole
        // promise of "zero flags".
        clear();
        std::env::set_var("HOME", "/home/someone");
        let s = run(&[]);
        assert_eq!(s.data_dir, PathBuf::from("/home/someone/.wheel"));
        assert_eq!(s.bind, "0.0.0.0:8080");

        // The environment supplies defaults when no flags are given.
        std::env::set_var("WHEEL_DATA_DIR", "/from/env");
        std::env::set_var("BIND_ADDR", "0.0.0.0:9999");
        let s = run(&[]);
        assert_eq!(s.data_dir, PathBuf::from("/from/env"));
        assert_eq!(s.bind, "0.0.0.0:9999");

        // ...and a flag overrides it.
        let s = run(&["--data-dir", "/from/flag", "--bind", "127.0.0.1:1234"]);
        assert_eq!(s.data_dir, PathBuf::from("/from/flag"));
        assert_eq!(s.bind, "127.0.0.1:1234");

        // The --flag=value form is equivalent.
        clear();
        let s = run(&["--data-dir=/x", "--bind=[::1]:80"]);
        assert_eq!(s.data_dir, PathBuf::from("/x"));
        assert_eq!(s.bind, "[::1]:80");

        // No HOME and no flag: there is nothing to derive a data directory from, so the error has
        // to name the flag that fixes it rather than invent a location.
        clear();
        std::env::remove_var("HOME");
        let e = Settings::parse::<[&str; 0], &str>([]).unwrap_err();
        assert!(format!("{e:#}").contains("--data-dir"), "{e:#}");

        clear();
        std::env::set_var("HOME", "/home/someone");
    }

    #[test]
    fn help_and_version_are_actions_not_side_effects() {
        assert_eq!(Settings::parse(["--help"]).unwrap(), Action::PrintUsage);
        assert_eq!(Settings::parse(["-h"]).unwrap(), Action::PrintUsage);
        assert_eq!(Settings::parse(["-V"]).unwrap(), Action::PrintVersion);
    }

    /// A mistyped flag must not be ignored: silently running with a default the user did not ask
    /// for is how data ends up in the wrong directory.
    #[test]
    fn an_unknown_flag_is_refused_and_the_message_says_how_to_use_it() {
        let e = Settings::parse(["--datadir", "/x"]).unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("--datadir"), "{msg}");
        assert!(msg.contains("USAGE"), "{msg}");
    }

    #[test]
    fn a_flag_without_its_value_is_an_error() {
        assert!(Settings::parse(["--data-dir"]).is_err());
        assert!(Settings::parse(["--bind"]).is_err());
    }
}

#[cfg(test)]
mod ready_line_tests {
    use super::super::displayable;

    /// The first line a new user reads. `0.0.0.0:8080` is not something a browser can open.
    #[test]
    fn the_ready_line_gives_an_address_a_browser_can_open() {
        assert_eq!(displayable("0.0.0.0:8080"), "localhost:8080");
        assert_eq!(displayable("[::]:8080"), "localhost:8080");
        assert_eq!(displayable("127.0.0.1:8099"), "127.0.0.1:8099");
    }
}
