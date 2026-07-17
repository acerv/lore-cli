use ratatui::crossterm::event::{Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind};
use ratatui::widgets::ListState;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::event::AppEvent;
use crate::lore::LoreClient;
use crate::model::PatchEntry;

/// The whole application state.
pub struct App {
    pub config: Config,
    pub client: LoreClient,
    tx: UnboundedSender<AppEvent>,
    pub patches: Vec<PatchEntry>,
    pub list_state: ListState,
    pub loading_patches: bool,
    pub error: Option<String>,
    pub should_quit: bool,
    /// Monotonic counter advanced on every tick (drives spinners).
    pub tick: u64,
}

impl App {
    pub fn new(config: Config, client: LoreClient, tx: UnboundedSender<AppEvent>) -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            config,
            client,
            tx,
            patches: Vec::new(),
            list_state,
            loading_patches: true,
            error: None,
            should_quit: false,
            tick: 0,
        }
    }

    /// Kick off the initial patch-list fetch in the background.
    pub fn spawn_initial_load(&self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client.fetch_patch_list(0).await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::PatchesLoaded(result));
        });
    }

    pub fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn on_patches_loaded(&mut self, result: Result<Vec<PatchEntry>, String>) {
        self.loading_patches = false;
        match result {
            Ok(patches) => {
                self.patches = patches;
                let selection = if self.patches.is_empty() { None } else { Some(0) };
                self.list_state.select(selection);
            }
            Err(err) => self.error = Some(err),
        }
    }

    // ----- patch list navigation -------------------------------------------

    fn select_index(&mut self, index: usize) {
        if self.patches.is_empty() {
            self.list_state.select(None);
            return;
        }
        self.list_state.select(Some(index.min(self.patches.len() - 1)));
    }

    pub fn select_next(&mut self) {
        let next = self.list_state.selected().map_or(0, |i| i + 1);
        self.select_index(next);
    }

    pub fn select_prev(&mut self) {
        let prev = self.list_state.selected().map_or(0, |i| i.saturating_sub(1));
        self.select_index(prev);
    }

    pub fn select_first(&mut self) {
        self.select_index(0);
    }

    pub fn select_last(&mut self) {
        self.select_index(usize::MAX);
    }

    pub fn select_by(&mut self, delta: i32) {
        let current = self.list_state.selected().unwrap_or(0) as i32;
        self.select_index((current + delta).max(0) as usize);
    }

    // ----- input -----------------------------------------------------------

    pub fn handle_crossterm(&mut self, event: CrosstermEvent) {
        if let CrosstermEvent::Key(key) = event {
            if key.kind == KeyEventKind::Press {
                self.handle_key(key);
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.select_prev(),
            KeyCode::Home | KeyCode::Char('g') => self.select_first(),
            KeyCode::End | KeyCode::Char('G') => self.select_last(),
            KeyCode::PageDown => self.select_by(10),
            KeyCode::PageUp => self.select_by(-10),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, LoreConfig, UiConfig};
    use crate::model::{PatchEntry, PatchStatus};
    use tokio::sync::mpsc::unbounded_channel;

    fn test_app(patch_count: usize) -> App {
        let config = Config {
            lore: LoreConfig {
                server: "https://lore.kernel.org".into(),
                project: "test".into(),
            },
            ui: UiConfig::default(),
        };
        let client = LoreClient::new(&config.lore).unwrap();
        let (tx, _rx) = unbounded_channel();
        let mut app = App::new(config, client, tx);
        app.loading_patches = false;
        app.patches = (0..patch_count)
            .map(|i| PatchEntry {
                subject: format!("patch {i}"),
                author_name: "Dev".into(),
                author_email: "dev@x".into(),
                message_id: format!("id{i}@x"),
                updated: None,
                status: PatchStatus::Unknown,
            })
            .collect();
        app
    }

    #[test]
    fn navigation_clamps_to_bounds() {
        let mut app = test_app(3);
        assert_eq!(app.list_state.selected(), Some(0));
        app.select_prev();
        assert_eq!(app.list_state.selected(), Some(0));
        app.select_next();
        app.select_next();
        app.select_next();
        assert_eq!(app.list_state.selected(), Some(2));
        app.select_first();
        assert_eq!(app.list_state.selected(), Some(0));
        app.select_last();
        assert_eq!(app.list_state.selected(), Some(2));
    }

    #[test]
    fn navigation_on_empty_list() {
        let mut app = test_app(0);
        app.select_next();
        assert_eq!(app.list_state.selected(), None);
    }
}
