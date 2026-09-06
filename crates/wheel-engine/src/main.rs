//! `wheel-engine` — one process per project, inside its sandbox.
//!
//! Deliberately thin: everything it does lives in the library, so `wheeld`
//! and this binary cannot diverge in what an engine actually is.

use std::process::ExitCode;

use wheel_engine::Config;

fn main() -> ExitCode {
    // Misconfiguration must fail loudly and immediately with a one-line reason:
    // the host reads a non-zero exit as "this sandbox is broken", which is far
    // better than an engine that boots half-configured and accepts traffic.
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wheel-engine: {e}");
            return ExitCode::from(2);
        }
    };

    let json_logs = cfg.json_logs;
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("wheel-engine: cannot start tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    // The binary owns the process, so the binary owns the logging.
    wheel_engine::init_tracing(json_logs);

    match runtime.block_on(wheel_engine::serve(cfg)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wheel-engine: {e:#}");
            ExitCode::FAILURE
        }
    }
}
