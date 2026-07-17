//! lore-cli: a terminal UI for browsing lore/public-inbox patches.
// NOTE: dead_code is allowed while the app is wired up incrementally across
// commits; this attribute is removed at the polish step once every module is
// in use.
#![allow(dead_code)]

mod app;
mod config;
mod event;
mod lore;
mod model;
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

/// Spawn a blocking thread that forwards terminal input over the channel.
fn spawn_input_reader(tx: UnboundedSender<AppEvent>) {
    std::thread::spawn(move || loop {
        match ratatui::crossterm::event::read() {
            Ok(event) => {
                if tx.send(AppEvent::Input(event)).is_err() {
                    break;
                }
            }
            Err(_) => break,
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
    let client = lore::LoreClient::new(&config.lore)?;

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
            AppEvent::ThreadLoaded { message_id, result } => {
                app.on_thread_loaded(message_id, result)
            }
        }
        if app.should_quit {
            break;
        }
        terminal.draw(|frame| ui::render(frame, app))?;
    }
    Ok(())
}
