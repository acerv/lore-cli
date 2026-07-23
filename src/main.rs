//! lore-cli: a terminal UI for browsing lore/public-inbox patches.

mod app;
mod cache;
mod config;
mod event;
mod lore;
mod model;
mod series;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::app::App;
use crate::config::Config;
use crate::event::AppEvent;

/// Parsed command-line options.
struct Args {
    config_path: PathBuf,
}

fn parse_args() -> Result<Args> {
    let mut config_path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => {
                let path = args.next().context("--config requires a path argument")?;
                config_path = Some(PathBuf::from(path));
            }
            "--help" | "-h" => {
                println!("Usage: lore-cli [--config <path>]");
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}"),
        }
    }
    let config_path = match config_path {
        Some(path) => path,
        None => default_config_path()?,
    };
    Ok(Args { config_path })
}

/// Resolve the default config location.
///
/// Prefers the platform config directory (e.g. `~/.config/lore-cli/config.toml`
/// on Linux), matching where the cache lives. Falls back to `./config.toml` in
/// the current directory if that file exists but the config-dir one does not,
/// preserving the previous behaviour for in-repo runs.
fn default_config_path() -> Result<PathBuf> {
    let cwd_config = PathBuf::from("config.toml");
    if let Some(dirs) = directories::ProjectDirs::from("", "", "lore-cli") {
        let config = dirs.config_dir().join("config.toml");
        if config.exists() {
            return Ok(config);
        }
        // Fall back to a local config.toml when present (e.g. running from the
        // repo checkout), otherwise point users at the config dir location.
        if cwd_config.exists() {
            return Ok(cwd_config);
        }
        return Ok(config);
    }
    Ok(cwd_config)
}

/// Spawn a blocking thread that forwards terminal input over the channel.
fn spawn_input_reader(tx: UnboundedSender<AppEvent>) {
    std::thread::spawn(move || {
        while let Ok(event) = ratatui::crossterm::event::read() {
            if tx.send(AppEvent::Input(event)).is_err() {
                break;
            }
        }
    });
}

/// Spawn a task that emits a periodic tick used for loading animations.
fn spawn_ticker(tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        loop {
            interval.tick().await;
            if tx.send(AppEvent::Tick).is_err() {
                break;
            }
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let config = Config::load(&args.config_path)?;
    let client = lore::LoreClient::new(&config.lore, config.cache.max_age_secs)?;

    let (tx, rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut terminal = ratatui::init();

    spawn_input_reader(tx.clone());
    spawn_ticker(tx.clone());

    let mut app = App::new(config, client, tx);
    app.spawn_initial_load();

    let result = run(&mut terminal, &mut app, rx).await;

    ratatui::restore();
    result
}

async fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut rx: UnboundedReceiver<AppEvent>,
) -> Result<()> {
    terminal.draw(|frame| ui::render(frame, app))?;
    while let Some(event) = rx.recv().await {
        match event {
            AppEvent::Input(input) => app.handle_crossterm(input),
            AppEvent::Tick => app.on_tick(),
            AppEvent::PatchesLoaded(result) => app.on_patches_loaded(result),
            AppEvent::MoreLoaded(result) => app.on_more_loaded(result),
            AppEvent::StatusUpdated { message_id, status } => {
                app.on_status_updated(&message_id, status)
            }
            AppEvent::ThreadLoaded { message_id, result } => {
                app.on_thread_loaded(message_id, result)
            }
            AppEvent::Applied { message_id, result } => app.on_applied(message_id, result),
        }
        if app.should_quit {
            break;
        }
        terminal.draw(|frame| ui::render(frame, app))?;
        app.probe_visible();
    }
    Ok(())
}
