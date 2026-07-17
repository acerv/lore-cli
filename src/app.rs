use std::sync::Arc;

use ratatui::crossterm::event::{
    Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::widgets::ListState;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::event::AppEvent;
use crate::lore::LoreClient;
use crate::model::{Email, PatchEntry, PatchStatus};

/// An open thread, shown in its own tab.
pub struct ThreadTab {
    pub message_id: String,
    pub subject: String,
    pub emails: Vec<Email>,
    pub loading: bool,
    pub error: Option<String>,
    pub scroll: u16,
    /// Visible height captured on the last render (for paging/clamping).
    pub viewport_height: u16,
    /// Total wrapped line count captured on the last render.
    pub content_len: u16,
}

impl ThreadTab {
    fn new(message_id: String, subject: String) -> Self {
        Self {
            message_id,
            subject,
            emails: Vec::new(),
            loading: true,
            error: None,
            scroll: 0,
            viewport_height: 0,
            content_len: 0,
        }
    }

    fn max_scroll(&self) -> u16 {
        self.content_len.saturating_sub(self.viewport_height)
    }

    pub fn scroll_lines(&mut self, delta: i32) {
        let max = self.max_scroll() as i32;
        self.scroll = (self.scroll as i32 + delta).clamp(0, max) as u16;
    }

    fn scroll_to_end(&mut self) {
        self.scroll = self.max_scroll();
    }
}

/// The whole application state.
pub struct App {
    pub config: Config,
    pub client: LoreClient,
    tx: UnboundedSender<AppEvent>,
    pub patches: Vec<PatchEntry>,
    pub list_state: ListState,
    pub loading_patches: bool,
    pub loading_more: bool,
    pub all_loaded: bool,
    pub error: Option<String>,
    /// Open thread tabs. Tab index 0 is the patch list; tab N is `tabs[N - 1]`.
    pub tabs: Vec<ThreadTab>,
    pub active_tab: usize,
    pub should_quit: bool,
    /// Monotonic counter advanced on every tick (drives spinners).
    pub tick: u64,
    /// Limits how many status probes hit the server at once.
    status_sem: Arc<Semaphore>,
}

impl App {
    pub fn new(config: Config, client: LoreClient, tx: UnboundedSender<AppEvent>) -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        let status_sem = Arc::new(Semaphore::new(config.ui.status_concurrency.max(1)));
        Self {
            config,
            client,
            tx,
            patches: Vec::new(),
            list_state,
            loading_patches: true,
            loading_more: false,
            all_loaded: false,
            error: None,
            tabs: Vec::new(),
            active_tab: 0,
            should_quit: false,
            tick: 0,
            status_sem,
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
                self.all_loaded = self.patches.len() < self.config.ui.page_size;
                let selection = if self.patches.is_empty() { None } else { Some(0) };
                self.list_state.select(selection);
                self.spawn_status_fetches(0);
            }
            Err(err) => self.error = Some(err),
        }
    }

    pub fn on_more_loaded(&mut self, result: Result<Vec<PatchEntry>, String>) {
        self.loading_more = false;
        match result {
            Ok(mut more) => {
                if more.len() < self.config.ui.page_size {
                    self.all_loaded = true;
                }
                if more.is_empty() {
                    return;
                }
                let start = self.patches.len();
                self.patches.append(&mut more);
                self.spawn_status_fetches(start);
            }
            // A failed "load more" is non-fatal: just allow retrying later.
            Err(_) => {}
        }
    }

    pub fn on_status_updated(&mut self, message_id: &str, status: PatchStatus) {
        if let Some(patch) = self.patches.iter_mut().find(|p| p.message_id == message_id) {
            patch.status = status;
        }
    }

    /// Probe patches from `start` onward (bounded by a semaphore) to color them.
    fn spawn_status_fetches(&self, start: usize) {
        let marker = self.config.status.merged_marker.clone();
        for patch in self.patches.iter().skip(start) {
            if patch.status != PatchStatus::Unknown {
                continue;
            }
            let client = self.client.clone();
            let tx = self.tx.clone();
            let semaphore = self.status_sem.clone();
            let message_id = patch.message_id.clone();
            let marker = marker.clone();
            tokio::spawn(async move {
                let _permit = semaphore.acquire_owned().await.ok();
                if let Ok(emails) = client.fetch_thread(&message_id).await {
                    let status = crate::lore::status::compute_status(&emails, &marker);
                    let _ = tx.send(AppEvent::StatusUpdated { message_id, status });
                }
            });
        }
    }

    pub fn on_thread_loaded(&mut self, message_id: String, result: Result<Vec<Email>, String>) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.message_id == message_id) else {
            return;
        };
        tab.loading = false;
        match result {
            Ok(emails) => tab.emails = emails,
            Err(err) => tab.error = Some(err),
        }
    }

    /// Request the next page of patches, if there is one and none is in flight.
    fn load_more(&mut self) {
        if self.loading_patches || self.loading_more || self.all_loaded {
            return;
        }
        self.loading_more = true;
        let client = self.client.clone();
        let tx = self.tx.clone();
        let offset = self.patches.len();
        tokio::spawn(async move {
            let result = client
                .fetch_patch_list(offset)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::MoreLoaded(result));
        });
    }

    /// Load another page once the selection nears the end of the list.
    fn maybe_load_more(&mut self) {
        if let Some(selected) = self.list_state.selected() {
            if selected + 1 >= self.patches.len() {
                self.load_more();
            }
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

    fn select_next(&mut self) {
        let next = self.list_state.selected().map_or(0, |i| i + 1);
        self.select_index(next);
    }

    fn select_prev(&mut self) {
        let prev = self.list_state.selected().map_or(0, |i| i.saturating_sub(1));
        self.select_index(prev);
    }

    fn select_first(&mut self) {
        self.select_index(0);
    }

    fn select_last(&mut self) {
        self.select_index(usize::MAX);
    }

    fn select_by(&mut self, delta: i32) {
        let current = self.list_state.selected().unwrap_or(0) as i32;
        self.select_index((current + delta).max(0) as usize);
    }

    // ----- tabs ------------------------------------------------------------

    fn tab_count(&self) -> usize {
        self.tabs.len() + 1 // +1 for the patch list
    }

    fn next_tab(&mut self) {
        self.active_tab = (self.active_tab + 1) % self.tab_count();
    }

    fn prev_tab(&mut self) {
        self.active_tab = (self.active_tab + self.tab_count() - 1) % self.tab_count();
    }

    fn active_thread_mut(&mut self) -> Option<&mut ThreadTab> {
        if self.active_tab == 0 {
            None
        } else {
            self.tabs.get_mut(self.active_tab - 1)
        }
    }

    /// Open the selected patch's thread, focusing an existing tab if present.
    fn open_selected_thread(&mut self) {
        let Some(index) = self.list_state.selected() else {
            return;
        };
        let Some(patch) = self.patches.get(index) else {
            return;
        };
        let message_id = patch.message_id.clone();

        if let Some(pos) = self.tabs.iter().position(|t| t.message_id == message_id) {
            self.active_tab = pos + 1;
            return;
        }

        self.tabs
            .push(ThreadTab::new(message_id.clone(), patch.subject.clone()));
        self.active_tab = self.tabs.len();
        self.spawn_thread_fetch(message_id);
    }

    fn spawn_thread_fetch(&self, message_id: String) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .fetch_thread(&message_id)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::ThreadLoaded { message_id, result });
        });
    }

    fn close_active_tab(&mut self) {
        if self.active_tab == 0 {
            return;
        }
        let index = self.active_tab - 1;
        if index < self.tabs.len() {
            self.tabs.remove(index);
        }
        self.active_tab -= 1; // fall back to the previous tab (or the list)
    }

    fn scroll_active(&mut self, delta: i32) {
        if let Some(tab) = self.active_thread_mut() {
            tab.scroll_lines(delta);
        }
    }

    fn scroll_active_half(&mut self, down: bool) {
        if let Some(tab) = self.active_thread_mut() {
            let step = (tab.viewport_height / 2).max(1) as i32;
            tab.scroll_lines(if down { step } else { -step });
        }
    }

    fn scroll_active_page(&mut self, down: bool) {
        if let Some(tab) = self.active_thread_mut() {
            let step = tab.viewport_height.saturating_sub(1).max(1) as i32;
            tab.scroll_lines(if down { step } else { -step });
        }
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
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Tab switching works from any view.
        if ctrl {
            match key.code {
                KeyCode::Char('n') => {
                    self.next_tab();
                    return;
                }
                KeyCode::Char('p') => {
                    self.prev_tab();
                    return;
                }
                _ => {}
            }
        }

        if self.active_tab == 0 {
            self.handle_list_key(key);
        } else {
            self.handle_thread_key(key, ctrl);
        }
    }

    fn handle_list_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                self.maybe_load_more();
            }
            KeyCode::Up | KeyCode::Char('k') => self.select_prev(),
            KeyCode::Home | KeyCode::Char('g') => self.select_first(),
            KeyCode::End | KeyCode::Char('G') => {
                self.select_last();
                self.maybe_load_more();
            }
            KeyCode::PageDown => {
                self.select_by(10);
                self.maybe_load_more();
            }
            KeyCode::PageUp => self.select_by(-10),
            KeyCode::Char('m') => self.load_more(),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_selected_thread(),
            _ => {}
        }
    }

    fn handle_thread_key(&mut self, key: KeyEvent, ctrl: bool) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.close_active_tab(),
            KeyCode::Char('d') if ctrl => self.scroll_active_half(true),
            KeyCode::Char('u') if ctrl => self.scroll_active_half(false),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_active(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_active(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_active_page(true),
            KeyCode::PageUp => self.scroll_active_page(false),
            KeyCode::Home | KeyCode::Char('g') => {
                if let Some(tab) = self.active_thread_mut() {
                    tab.scroll = 0;
                }
            }
            KeyCode::End | KeyCode::Char('G') => {
                if let Some(tab) = self.active_thread_mut() {
                    tab.scroll_to_end();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, LoreConfig, StatusConfig, UiConfig};
    use crate::model::PatchStatus;
    use tokio::sync::mpsc::unbounded_channel;

    fn test_app(patch_count: usize) -> App {
        let config = Config {
            lore: LoreConfig {
                server: "https://lore.kernel.org".into(),
                project: "test".into(),
            },
            ui: UiConfig::default(),
            status: StatusConfig::default(),
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

    fn push_tab(app: &mut App, id: &str) {
        app.tabs.push(ThreadTab::new(id.into(), id.into()));
        app.active_tab = app.tabs.len();
    }

    #[test]
    fn navigation_clamps_to_bounds() {
        let mut app = test_app(3);
        app.select_prev();
        assert_eq!(app.list_state.selected(), Some(0));
        app.select_next();
        app.select_next();
        app.select_next();
        assert_eq!(app.list_state.selected(), Some(2));
        app.select_last();
        assert_eq!(app.list_state.selected(), Some(2));
    }

    #[test]
    fn navigation_on_empty_list() {
        let mut app = test_app(0);
        app.select_next();
        assert_eq!(app.list_state.selected(), None);
    }

    #[test]
    fn tab_cycling_wraps_over_list_and_threads() {
        let mut app = test_app(2);
        push_tab(&mut app, "a");
        push_tab(&mut app, "b");
        assert_eq!(app.active_tab, 2);
        app.next_tab();
        assert_eq!(app.active_tab, 0); // wrapped to the patch list
        app.prev_tab();
        assert_eq!(app.active_tab, 2);
        app.prev_tab();
        assert_eq!(app.active_tab, 1);
    }

    #[test]
    fn closing_tab_falls_back_to_previous() {
        let mut app = test_app(2);
        push_tab(&mut app, "a");
        push_tab(&mut app, "b");
        app.active_tab = 1; // focus tab "a"
        app.close_active_tab();
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_tab, 0); // back to the list
    }

    #[test]
    fn scroll_is_clamped_to_content() {
        let mut tab = ThreadTab::new("id".into(), "s".into());
        tab.viewport_height = 10;
        tab.content_len = 25;
        tab.scroll_lines(1000);
        assert_eq!(tab.scroll, 15); // 25 - 10
        tab.scroll_lines(-1000);
        assert_eq!(tab.scroll, 0);
    }

    #[test]
    fn load_more_respects_all_loaded() {
        let mut app = test_app(3);
        app.all_loaded = true;
        app.load_more();
        assert!(!app.loading_more, "must not start a fetch when fully loaded");
    }
}
