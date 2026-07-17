mod config;
mod lore;
mod model;

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

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let config = Config::load(&args.config_path)?;
    let client = lore::LoreClient::new(&config.lore)?;

    let patches = client.fetch_patch_list(0).await?;
    println!(
        "Fetched {} patches from {}/{}\n",
        patches.len(),
        config.lore.server,
        config.lore.project
    );
    for (i, p) in patches.iter().take(20).enumerate() {
        let date = p
            .updated
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        println!(
            "{:>3}. [{:?}] {}\n     {} <{}>  {}  id={}",
            i + 1,
            p.status,
            p.subject,
            p.author_name,
            p.author_email,
            date,
            p.message_id,
        );
    }

    if let Some(first) = patches.first() {
        println!("\n--- thread for: {} ---", first.subject);
        let emails = client.fetch_thread(&first.message_id).await?;
        println!("thread has {} message(s)", emails.len());
        for (i, e) in emails.iter().enumerate() {
            println!(
                "  {}. subj={:?} from={:?} date={:?} id={:?} irt={:?} body={}B",
                i + 1,
                e.subject,
                e.from,
                e.date,
                e.message_id,
                e.in_reply_to,
                e.body.len(),
            );
        }
    }
    Ok(())
}
