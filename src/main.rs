mod config;

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::config::Config;

/// Parsed command-line options.
struct Args {
    config_path: PathBuf,
}

fn parse_args() -> Result<Args> {
    let mut config_path = PathBuf::from("config.toml");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => {
                let path = args.next().context("--config requires a path argument")?;
                config_path = PathBuf::from(path);
            }
            "--help" | "-h" => {
                println!("Usage: lore-cli [--config <path>]");
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}"),
        }
    }
    Ok(Args { config_path })
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let config = Config::load(&args.config_path)?;
    println!(
        "Loaded config: server={} project={} (page_size={}, status_concurrency={})",
        config.lore.server, config.lore.project, config.ui.page_size, config.ui.status_concurrency,
    );
    Ok(())
}
