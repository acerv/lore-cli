use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

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
use crate::series::{self, Group};

/// How many times a status probe is attempted before giving up (leaves the
/// patch grey). A retry recovers from transient network blips.
const STATUS_FETCH_ATTEMPTS: usize = 2;
const STATUS_RETRY_DELAY: Duration = Duration::from_millis(750);

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

/// One visible line in the patch list (a flattened version tree).
#[derive(Debug, Clone, Copy)]
pub struct Row {
    /// Index into `App::patches`.
    pub patch: usize,
    /// 0 for a head/standalone row, 1 for a nested older version.
    pub depth: u8,
    /// Number of nested older versions (heads only).
    pub children: usize,
    /// Whether this head is currently expanded.
    pub expanded: bool,
    /// Index into `App::groups`.
    pub group: usize,
}

/// The whole application state.
pub struct App {
    pub config: Config,
    pub client: LoreClient,
    tx: UnboundedSender<AppEvent>,
    pub patches: Vec<PatchEntry>,
    /// Version groups derived from `patches`.
    pub groups: Vec<Group>,
    /// Flattened visible rows (respecting expand/collapse).
    pub rows: Vec<Row>,
    /// Keys of the groups the user has expanded.
    expanded: HashSet<String>,
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
    /// Message-ids already scheduled for a status probe (dedup).
    requested: HashSet<String>,
    /// Height of the list viewport captured on the last render.
    pub list_height: u16,
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
            groups: Vec::new(),
            rows: Vec::new(),
            expanded: HashSet::new(),
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
            requested: HashSet::new(),
            list_height: 0,
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
                self.requested.clear();
                self.rebuild_view();
            }
            Err(err) => self.error = Some(err),
        }
    }

    pub fn on_more_loaded(&mut self, result: Result<Vec<PatchEntry>, String>) {
        self.loading_more = false;
        // A failed "load more" is non-fatal: just allow retrying later.
        if let Ok(mut more) = result {
            if more.len() < self.config.ui.page_size {
                self.all_loaded = true;
            }
            if more.is_empty() {
                return;
            }
            self.patches.append(&mut more);
            self.rebuild_view();
        }
    }

    pub fn on_status_updated(&mut self, message_id: &str, status: PatchStatus) {
        if let Some(patch) = self.patches.iter_mut().find(|p| p.message_id == message_id) {
            patch.status = status;
        }
    }

    /// Schedule status probes for the rows in (or just below) the viewport, so a
    /// cold start only fetches what the user can actually see.
    pub fn probe_visible(&mut self) {
        for message_id in self.visible_probe_targets() {
            self.requested.insert(message_id.clone());
            self.spawn_status_probe(message_id);
        }
    }

    /// Message-ids of visible (plus one screen of look-ahead) patches whose
    /// status is still unknown and not already scheduled.
    fn visible_probe_targets(&self) -> Vec<String> {
        if self.rows.is_empty() {
            return Vec::new();
        }
        let offset = self.list_state.offset().min(self.rows.len());
        let end = (offset + self.list_height as usize * 2).min(self.rows.len());
        self.rows[offset..end]
            .iter()
            .filter_map(|row| self.patches.get(row.patch))
            .filter(|p| p.status == PatchStatus::Unknown && !self.requested.contains(&p.message_id))
            .map(|p| p.message_id.clone())
            .collect()
    }

    fn spawn_status_probe(&self, message_id: String) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        let semaphore = self.status_sem.clone();
        let marker = self.config.status.merged_marker.clone();
        tokio::spawn(async move {
            let _permit = semaphore.acquire_owned().await.ok();
            for attempt in 0..STATUS_FETCH_ATTEMPTS {
                match client.fetch_thread(&message_id).await {
                    Ok(emails) => {
                        let status = crate::lore::status::compute_status(&emails, &marker);
                        let _ = tx.send(AppEvent::StatusUpdated { message_id, status });
                        return;
                    }
                    // Retry transient failures; a timeout releases the permit
                    // so the pool keeps flowing either way.
                    Err(_) if attempt + 1 < STATUS_FETCH_ATTEMPTS => {
                        tokio::time::sleep(STATUS_RETRY_DELAY).await;
                    }
                    Err(_) => {}
                }
            }
        });
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
            if selected + 1 >= self.rows.len() {
                self.load_more();
            }
        }
    }

    // ----- version tree ----------------------------------------------------

    /// Rebuild the version grouping and visible rows from `patches`, preserving
    /// the selected patch and the expand/collapse state.
    pub(crate) fn rebuild_view(&mut self) {
        let selected_id = self.selected_patch_id();
        self.groups = series::group(self.patches.iter().map(|p| p.subject.as_str()));
        self.rebuild_rows();
        self.restore_selection(selected_id);
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (group_index, group) in self.groups.iter().enumerate() {
            let expanded = self.expanded.contains(&group.key);
            self.rows.push(Row {
                patch: group.head,
                depth: 0,
                children: group.children.len(),
                expanded,
                group: group_index,
            });
            if expanded {
                for &child in &group.children {
                    self.rows.push(Row {
                        patch: child,
                        depth: 1,
                        children: 0,
                        expanded: false,
                        group: group_index,
                    });
                }
            }
        }
    }

    fn selected_patch_id(&self) -> Option<String> {
        let row = self.rows.get(self.list_state.selected()?)?;
        Some(self.patches.get(row.patch)?.message_id.clone())
    }

    fn restore_selection(&mut self, id: Option<String>) {
        if self.rows.is_empty() {
            self.list_state.select(None);
            return;
        }
        let index = id
            .and_then(|id| {
                self.rows
                    .iter()
                    .position(|r| self.patches.get(r.patch).is_some_and(|p| p.message_id == id))
            })
            .unwrap_or_else(|| self.list_state.selected().unwrap_or(0).min(self.rows.len() - 1));
        self.list_state.select(Some(index));
    }

    fn selected_row(&self) -> Option<Row> {
        self.rows.get(self.list_state.selected()?).copied()
    }

    /// Expand or collapse the version tree under the selection (Space).
    fn toggle_selected_group(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let key = self.groups[row.group].key.clone();
        if row.depth == 0 {
            if row.children == 0 {
                return; // a standalone patch has nothing to fold
            }
            if !self.expanded.remove(&key) {
                self.expanded.insert(key);
            }
            self.rebuild_rows();
        } else {
            // On a nested version: collapse the group and select its head.
            self.expanded.remove(&key);
            self.rebuild_rows();
            if let Some(head_row) = self.rows.iter().position(|r| r.group == row.group) {
                self.list_state.select(Some(head_row));
            }
        }
    }

    // ----- patch list navigation -------------------------------------------

    fn select_index(&mut self, index: usize) {
        if self.rows.is_empty() {
            self.list_state.select(None);
            return;
        }
        self.list_state.select(Some(index.min(self.rows.len() - 1)));
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
        let Some(row) = self.selected_row() else {
            return;
        };
        let Some(patch) = self.patches.get(row.patch) else {
            return;
        };
        let message_id = patch.message_id.clone();
        let subject = patch.subject.clone();

        if let Some(pos) = self.tabs.iter().position(|t| t.message_id == message_id) {
            self.active_tab = pos + 1;
            return;
        }

        self.tabs.push(ThreadTab::new(message_id.clone(), subject));
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
            KeyCode::Char(' ') => self.toggle_selected_group(),
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
        app.rebuild_view();
        app
    }

    fn patch(subject: &str, id: &str) -> PatchEntry {
        PatchEntry {
            subject: subject.into(),
            author_name: "Dev".into(),
            author_email: "dev@x".into(),
            message_id: id.into(),
            updated: None,
            status: PatchStatus::Unknown,
        }
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

    #[test]
    fn version_tree_folds_and_unfolds() {
        let mut app = test_app(0);
        app.patches = vec![
            patch("[PATCH v2] mm: fix foo", "v2@x"),
            patch("[PATCH] mm: fix foo", "v1@x"),
            patch("[PATCH] other: bar", "other@x"),
        ];
        app.rebuild_view();

        // v2+v1 collapse into a single head; "other" stands alone => 2 rows.
        assert_eq!(app.rows.len(), 2);
        assert_eq!(app.rows[0].depth, 0);
        assert_eq!(app.rows[0].children, 1);

        // Space expands the tree, revealing the nested v1.
        app.list_state.select(Some(0));
        app.toggle_selected_group();
        assert_eq!(app.rows.len(), 3);
        assert_eq!(app.rows[1].depth, 1);
        assert_eq!(app.patches[app.rows[1].patch].message_id, "v1@x");

        // Space again collapses it.
        app.toggle_selected_group();
        assert_eq!(app.rows.len(), 2);
    }

    #[test]
    fn nested_and_head_rows_map_to_their_own_patches() {
        let mut app = test_app(0);
        app.patches = vec![
            patch("[PATCH v2] mm: fix foo", "v2@x"),
            patch("[PATCH] mm: fix foo", "v1@x"),
        ];
        app.rebuild_view();
        app.expanded.insert("mm: fix foo".into());
        app.rebuild_rows();

        // Enter on either row opens that version's own thread.
        assert_eq!(app.patches[app.rows[0].patch].message_id, "v2@x");
        assert_eq!(app.patches[app.rows[1].patch].message_id, "v1@x");
    }

    #[test]
    fn probes_only_visible_and_lookahead_rows() {
        let mut app = test_app(20);
        app.list_height = 3;
        // offset 0, viewport 3 + one screen look-ahead => rows 0..6.
        let targets = app.visible_probe_targets();
        assert_eq!(targets.len(), 6);

        // Already-scheduled rows are not returned again.
        for id in &targets {
            app.requested.insert(id.clone());
        }
        assert!(app.visible_probe_targets().is_empty());
    }
}
