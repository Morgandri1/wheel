//! Entry point. Everything it does lives in the library, where tests can reach it.

fn main() -> anyhow::Result<()> {
    wheeld::cli_main(std::env::args().skip(1))
}
