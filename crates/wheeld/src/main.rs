//! Entry point. The decisions live in the library, where tests can reach them.

use anyhow::Result;
use wheeld::config::{Action, Settings, USAGE};

#[tokio::main]
async fn main() -> Result<()> {
    match Settings::parse(std::env::args().skip(1))? {
        Action::PrintUsage => {
            print!("{USAGE}");
            Ok(())
        }
        Action::PrintVersion => {
            println!("wheeld {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Action::Run(settings) => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info,wheeld=debug".into()),
                )
                .init();
            wheeld::run(settings).await
        }
    }
}
