use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::event::{
    Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::widgets::ListState;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Semaphore;

use crate::cache::Cache;
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

/// A resolved apply request: the latest loaded version of the selected series.
#[derive(Debug, Clone)]
pub struct ApplyTarget {
    /// Message-id of the latest version's head (cover or standalone).
    pub message_id: String,
    /// Subject shown in the confirmation popup.
    pub subject: String,
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
    /// Per-patch flag: a newer version of this patch exists.
    pub superseded: Vec<bool>,
    /// Keys of the groups the user has expanded.
    expanded: HashSet<String>,
    pub list_state: ListState,
    pub loading_patches: bool,
    pub loading_more: bool,
    pub all_loaded: bool,
    pub refreshing: bool,
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
    /// Active list search query (subject substring), if any.
    pub search: Option<String>,
    /// Whether the search box is currently being typed into.
    pub search_active: bool,
    /// Toggle: show only the latest version of each patch (hide superseded children).
    pub latest_only: bool,
    /// Message-ids the user marked as viewed / not relevant (persisted).
    pub marked: HashSet<String>,
    /// On-disk store for `marked`, scoped to this server + project.
    marks_store: Cache,
    /// Pending `A` (apply) confirmation, if the user pressed `A`.
    pub apply_confirm: Option<ApplyTarget>,
    /// Whether a b4 apply is currently running (drives a spinner).
    pub apply_in_progress: bool,
    /// Handle to the running b4 task, so the apply can be cancelled.
    apply_task: Option<tokio::task::JoinHandle<()>>,
    /// Result of the last apply, shown in a dismissible popup.
    pub apply_result: Option<Result<String, String>>,
}

impl App {
    pub fn new(config: Config, client: LoreClient, tx: UnboundedSender<AppEvent>) -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        let status_sem = Arc::new(Semaphore::new(config.ui.status_concurrency.max(1)));
        // Persisted marks live alongside the thread cache, keyed by host+project.
        // Marks never expire (TTL 0); they are read via load_marks, not get().
        let marks_store = Cache::new(&config.lore.server, &config.lore.project, 0);
        let marked = marks_store.load_marks();
        Self {
            config,
            client,
            tx,
            patches: Vec::new(),
            groups: Vec::new(),
            rows: Vec::new(),
            superseded: Vec::new(),
            expanded: HashSet::new(),
            list_state,
            loading_patches: true,
            loading_more: false,
            all_loaded: false,
            refreshing: false,
            error: None,
            tabs: Vec::new(),
            active_tab: 0,
            should_quit: false,
            tick: 0,
            status_sem,
            requested: HashSet::new(),
            list_height: 0,
            search: None,
            search_active: false,
            latest_only: false,
            marked,
            marks_store,
            apply_confirm: None,
            apply_in_progress: false,
            apply_task: None,
            apply_result: None,
        }
    }

    /// Toggle the "viewed / not relevant" mark on the selected patch and persist
    /// it, so the greyed-out state survives across restarts.
    fn toggle_selected_mark(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let Some(patch) = self.patches.get(row.patch) else {
            return;
        };
        let id = patch.message_id.clone();
        if !self.marked.remove(&id) {
            self.marked.insert(id);
        }
        self.marks_store.save_marks(&self.marked);
        // In latest-only mode a freshly-marked patch should drop out of the list.
        if self.latest_only {
            self.rebuild_view();
        }
    }

    // ----- apply with b4 ---------------------------------------------------

    /// Resolve the selected series to the latest loaded version so `b4` applies
    /// the newest revision (and its `Link:` points at the latest patch/set).
    fn latest_series_target(&self, row: Row) -> Option<ApplyTarget> {
        let head = self.groups.get(row.group).map(|g| g.head)?;
        let (_, key) = series::version_of(&self.patches.get(head)?.subject);

        // Among all group heads sharing the same title, pick the highest version.
        let best = self
            .groups
            .iter()
            .filter_map(|g| {
                let patch = self.patches.get(g.head)?;
                let (version, k) = series::version_of(&patch.subject);
                (k == key).then_some((version, patch))
            })
            .max_by_key(|(version, _)| *version)
            .map(|(_, patch)| patch)
            .unwrap_or(self.patches.get(head)?);

        Some(ApplyTarget {
            message_id: best.message_id.clone(),
            subject: best.subject.clone(),
        })
    }

    /// The canonical lore URL for a message-id, handed to `b4`.
    fn apply_lore_url(&self, message_id: &str) -> String {
        format!(
            "{}/{}/{}/",
            self.config.lore.server, self.config.lore.project, message_id
        )
    }

    /// Open the apply confirmation for the selected series (`A`).
    fn request_apply(&mut self) {
        if self.apply_in_progress {
            return;
        }
        if let Some(row) = self.selected_row() {
            self.apply_confirm = self.latest_series_target(row);
        }
    }

    /// Launch `b4 shazam` in the current directory to apply the confirmed series.
    fn start_apply(&mut self) {
        let Some(target) = self.apply_confirm.take() else {
            return;
        };
        self.apply_in_progress = true;
        let url = self.apply_lore_url(&target.message_id);
        let message_id = target.message_id;
        let tx = self.tx.clone();
        let handle = tokio::spawn(async move {
            // `-n` keeps b4 non-interactive (no prompts fighting the TUI for
            // stdin); `-l` adds a Link: trailer with the lore URL to every patch.
            // `kill_on_drop` ensures cancelling the task also kills b4.
            let output = tokio::process::Command::new("b4")
                .args(["-n", "shazam", "-l", &url])
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output()
                .await;
            let result = match output {
                Ok(out) => {
                    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                    text.push_str(&String::from_utf8_lossy(&out.stderr));
                    if out.status.success() {
                        Ok(text)
                    } else {
                        Err(if text.trim().is_empty() {
                            format!("b4 exited with {}", out.status)
                        } else {
                            text
                        })
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Err("b4 not found — install it and ensure it is on your PATH \
                         (see https://b4.docs.kernel.org)"
                        .to_string())
                }
                Err(e) => Err(format!("failed to run b4: {e}")),
            };
            let _ = tx.send(AppEvent::Applied { message_id, result });
        });
        self.apply_task = Some(handle);
    }

    /// Abort a running b4 apply (Esc/c while applying). `kill_on_drop` on the
    /// command means aborting the task also terminates the b4 process.
    fn cancel_apply(&mut self) {
        if let Some(handle) = self.apply_task.take() {
            handle.abort();
        }
        if self.apply_in_progress {
            self.apply_in_progress = false;
            self.apply_result = Some(Err(
                "apply cancelled — b4 was interrupted; if it had started applying, \
                 run `git am --abort` in your tree"
                    .to_string(),
            ));
        }
    }

    pub fn on_applied(&mut self, _message_id: String, result: Result<String, String>) {
        self.apply_task = None;
        // Ignore a late result that arrives after the user cancelled.
        if !self.apply_in_progress {
            return;
        }
        self.apply_in_progress = false;
        self.apply_result = Some(result);
    }

    /// Kick off the initial patch-list fetch in the background.
    pub fn spawn_initial_load(&self) {
        self.spawn_list_fetch();
    }

    /// Re-fetch the newest page to pick up new patches (R).
    fn refresh(&mut self) {
        if self.loading_patches || self.refreshing {
            return;
        }
        self.refreshing = true;
        self.spawn_list_fetch();
    }

    fn spawn_list_fetch(&self) {
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
        self.refreshing = false;
        match result {
            Ok(patches) => {
                let keep = self.selected_patch_id();
                // Carry over already-known statuses so a refresh doesn't grey the
                // whole list; only genuinely new patches are probed again.
                let previous: HashMap<String, PatchStatus> = self
                    .patches
                    .iter()
                    .map(|p| (p.message_id.clone(), p.status))
                    .collect();
                self.patches = patches;
                for patch in self.patches.iter_mut() {
                    if let Some(&status) = previous.get(&patch.message_id) {
                        patch.status = status;
                    }
                }
                self.all_loaded = self.patches.len() < self.config.ui.page_size;
                self.rebuild_view_keeping(keep);
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
        let changed = self
            .patches
            .iter_mut()
            .find(|p| p.message_id == message_id)
            .map(|patch| {
                if patch.status != status {
                    patch.status = status;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);

        // In latest-only mode a Reviewed/Merged patch should drop out of the
        // list as soon as its status is determined; rebuild to reflect that.
        if changed && self.latest_only {
            self.rebuild_view();
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
        let markers = self.config.status.merged_markers.clone();
        tokio::spawn(async move {
            let _permit = semaphore.acquire_owned().await.ok();
            for attempt in 0..STATUS_FETCH_ATTEMPTS {
                match client.fetch_thread(&message_id).await {
                    Ok(emails) => {
                        let status = crate::lore::status::compute_status(&emails, &markers);
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

    // ----- patch-set tree ----------------------------------------------------

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
            .and_then(|id| self.row_index_for_id(&id))
            .unwrap_or_else(|| self.list_state.selected().unwrap_or(0).min(self.rows.len() - 1));
        self.list_state.select(Some(index));
    }

    /// Find the visible row for a patch by message-id. If the patch itself is
    /// not a visible row (e.g. a collapsed older version that only appears in
    /// the flat search list), fall back to the head row of its group so the
    /// selection lands on the same patch-set rather than a stale index.
    fn row_index_for_id(&self, id: &str) -> Option<usize> {
        // Exact row match first.
        if let Some(pos) = self
            .rows
            .iter()
            .position(|r| self.patches.get(r.patch).is_some_and(|p| p.message_id == id))
        {
            return Some(pos);
        }
        // Otherwise map the patch to its group and select that group's head row.
        let patch_index = self.patches.iter().position(|p| p.message_id == id)?;
        let group_index = self
            .groups
            .iter()
            .position(|g| g.head == patch_index || g.children.contains(&patch_index))?;
        self.rows.iter().position(|r| r.group == group_index && r.depth == 0)
    }

    fn selected_row(&self) -> Option<Row> {
        self.rows.get(self.list_state.selected()?).copied()
    }

    pub(crate) fn rebuild_view(&mut self) {
        let keep = self.selected_patch_id();
        self.rebuild_view_keeping(keep);
    }

    fn rebuild_view_keeping(&mut self, keep: Option<String>) {
        self.superseded =
            series::superseded_flags(self.patches.iter().map(|p| p.subject.as_str()));
        self.groups = series::group(
            self.patches
                .iter()
                .map(|p| (p.subject.as_str(), p.message_id.as_str())),
        );
        self.rebuild_rows();
        self.restore_selection(keep);
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();

        // A live search shows a flat, filtered list (case-insensitive subject).
        let filter = self
            .search
            .as_ref()
            .filter(|q| !q.is_empty())
            .map(|q| q.to_lowercase());
        if let Some(query) = filter {
            for (i, patch) in self.patches.iter().enumerate() {
                if patch.subject.to_lowercase().contains(&query)
                    && !self.is_hidden_by_latest(i)
                {
                    self.rows.push(Row {
                        patch: i,
                        depth: 0,
                        children: 0,
                        expanded: false,
                        group: 0,
                    });
                }
            }
            return;
        }

        for (group_index, group) in self.groups.iter().enumerate() {
            // Skip the whole group when its head is filtered out by latest-only.
            if self.is_hidden_by_latest(group.head) {
                continue;
            }
            let key = self.patches[group.head].message_id.clone();
            let expanded = self.expanded.contains(&key) && !self.latest_only;
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

    /// Whether the patch at `index` should be hidden in latest-only mode: an
    /// older version (superseded), already reviewed/merged, or user-marked.
    fn is_hidden_by_latest(&self, index: usize) -> bool {
        if !self.latest_only {
            return false;
        }
        if self
            .superseded
            .get(index)
            .copied()
            .unwrap_or(false)
        {
            return true;
        }
        if self
            .patches
            .get(index)
            .is_some_and(|p| self.marked.contains(&p.message_id))
        {
            return true;
        }
        matches!(
            self.patches.get(index).map(|p| p.status),
            Some(PatchStatus::Reviewed) | Some(PatchStatus::Merged)
        )
    }

    /// Expand or collapse the patch-set under the selection (Space).
    fn toggle_selected_group(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.depth == 0 && row.children == 0 {
            return; // a standalone patch has nothing to fold
        }
        let key = self.patches[self.groups[row.group].head].message_id.clone();
        if row.depth == 0 {
            if !self.expanded.remove(&key) {
                self.expanded.insert(key);
            }
            self.rebuild_rows();
        } else {
            // On a member row: collapse the set and select its cover.
            self.expanded.remove(&key);
            self.rebuild_rows();
            if let Some(head_row) = self.rows.iter().position(|r| r.group == row.group) {
                self.list_state.select(Some(head_row));
            }
        }
    }

    fn toggle_latest_only(&mut self) {
        self.latest_only = !self.latest_only;
        let keep = self.selected_patch_id();
        self.rebuild_rows();
        self.restore_selection(keep);
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

    fn half_page(&self) -> i32 {
        (self.list_height / 2).max(1) as i32
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

        // A result popup captures the next key to dismiss it.
        if self.apply_result.is_some() {
            self.apply_result = None;
            return;
        }
        // While applying, keys are captured; Esc/c cancels, others are ignored.
        if self.apply_in_progress {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('c')) {
                self.cancel_apply();
            }
            return;
        }
        // The apply confirmation captures keys: y confirms, anything else cancels.
        if self.apply_confirm.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.start_apply(),
                _ => self.apply_confirm = None,
            }
            return;
        }

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
            self.handle_list_key(key, ctrl);
        } else {
            self.handle_thread_key(key, ctrl);
        }
    }

    fn handle_list_key(&mut self, key: KeyEvent, ctrl: bool) {
        if self.search_active {
            self.handle_search_key(key, ctrl);
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc if self.search.is_some() => self.clear_search(),
            KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('/') => self.start_search(),
            KeyCode::Char('d') if ctrl => {
                self.select_by(self.half_page());
                self.maybe_load_more();
            }
            KeyCode::Char('u') if ctrl => self.select_by(-self.half_page()),
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
            KeyCode::Char('m') => self.toggle_selected_mark(),
            KeyCode::Char('A') => self.request_apply(),
            KeyCode::Char('R') => self.refresh(),
            KeyCode::Char('N') => self.toggle_latest_only(),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_selected_thread(),
            _ => {}
        }
    }

    // ----- live search -----------------------------------------------------

    fn handle_search_key(&mut self, key: KeyEvent, ctrl: bool) {
        match key.code {
            KeyCode::Esc => self.clear_search(),
            KeyCode::Enter => self.search_active = false, // commit, keep the filter
            // Erase the last character. Terminals are inconsistent about how
            // they report the backspace key: most send BS/DEL (parsed as
            // KeyCode::Backspace), some send Ctrl-H, and a few report it as
            // Delete. Accept all of them.
            KeyCode::Backspace | KeyCode::Delete => self.search_backspace(),
            KeyCode::Char('h') if ctrl => self.search_backspace(),
            KeyCode::Down => self.select_next(),
            KeyCode::Up => self.select_prev(),
            // Ignore control characters (e.g. a stray DEL delivered as a Char)
            // so they never get inserted into the query.
            KeyCode::Char(c) if !ctrl && !c.is_control() => {
                if let Some(query) = self.search.as_mut() {
                    query.push(c);
                }
                self.apply_search();
            }
            _ => {}
        }
    }

    /// Remove the last character of the search query and refresh the results.
    fn search_backspace(&mut self) {
        if let Some(query) = self.search.as_mut() {
            query.pop();
        }
        self.apply_search();
    }

    fn start_search(&mut self) {
        if self.search.is_none() {
            self.search = Some(String::new());
        }
        self.search_active = true;
        self.apply_search();
    }

    fn clear_search(&mut self) {
        // Remember the patch under the cursor so it stays selected once the
        // filtered list expands back to the full view.
        let keep = self.selected_patch_id();
        self.search = None;
        self.search_active = false;
        self.rebuild_rows();
        self.restore_selection(keep);
        // Scroll so the restored selection sits ~1/4 down the viewport rather
        // than clinging to the top/bottom edge.
        if let Some(selected) = self.list_state.selected() {
            let quarter = (self.list_height / 4) as usize;
            *self.list_state.offset_mut() = selected.saturating_sub(quarter);
        }
    }

    fn apply_search(&mut self) {
        self.rebuild_rows();
        let selection = if self.rows.is_empty() { None } else { Some(0) };
        self.list_state.select(selection);
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
    use crate::config::{CacheConfig, Config, LoreConfig, StatusConfig, UiConfig};
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
            cache: CacheConfig::default(),
        };
        let client = LoreClient::new(&config.lore, 0).unwrap();
        let (tx, _rx) = unbounded_channel();
        let mut app = App::new(config, client, tx);
        // Isolate tests from any persisted marks on disk.
        app.marks_store = Cache::disabled();
        app.marked.clear();
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
    fn patch_set_folds_and_unfolds() {
        let mut app = test_app(0);
        app.patches = vec![
            patch("[PATCH 2/2] two", "msg-3-a@x"),
            patch("[PATCH 1/2] one", "msg-2-a@x"),
            patch("[PATCH 0/2] cover", "msg-1-a@x"),
            patch("[PATCH] standalone", "solo-1-b@x"),
        ];
        app.rebuild_view();

        // The cover + 2 members collapse into one head; standalone stays => 2 rows.
        assert_eq!(app.rows.len(), 2);
        assert_eq!(app.rows[0].depth, 0);
        assert_eq!(app.rows[0].children, 2);

        // Space expands the patch-set, revealing the two members.
        app.list_state.select(Some(0));
        app.toggle_selected_group();
        assert_eq!(app.rows.len(), 4);
        assert_eq!(app.rows[1].depth, 1);
        assert_eq!(app.patches[app.rows[1].patch].message_id, "msg-2-a@x"); // 1/2

        // Space again collapses it.
        app.toggle_selected_group();
        assert_eq!(app.rows.len(), 2);
    }

    #[test]
    fn latest_only_toggle_hides_and_restores_children() {
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

        let mut app = test_app(0);
        app.patches = vec![
            patch("[PATCH 2/2] two", "msg-3-a@x"),
            patch("[PATCH 1/2] one", "msg-2-a@x"),
            patch("[PATCH 0/2] cover", "msg-1-a@x"),
        ];
        app.rebuild_view();

        // Start with the group expanded.
        app.list_state.select(Some(0));
        app.toggle_selected_group();
        assert_eq!(app.rows.len(), 3);
        assert!(app.rows[0].expanded);

        // Toggle latest_only via the N key.
        let key = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        app.handle_crossterm(key('N'));
        assert!(app.latest_only);
        assert_eq!(app.rows.len(), 1); // children hidden
        assert_eq!(app.rows[0].children, 2);

        // Selection should still be valid.
        assert_eq!(app.list_state.selected(), Some(0));

        // Toggle back: children restored, expanded state preserved.
        app.handle_crossterm(key('N'));
        assert!(!app.latest_only);
        assert_eq!(app.rows.len(), 3);
        assert!(app.rows[0].expanded);

        // Collapse, toggle latest_only, toggle back — still collapsed.
        app.toggle_selected_group();
        assert_eq!(app.rows.len(), 1);
        app.handle_crossterm(key('N'));
        app.handle_crossterm(key('N'));
        assert_eq!(app.rows.len(), 1); // still collapsed
    }

    #[test]
    fn latest_only_hides_superseded_standalone_patches() {
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

        let mut app = test_app(0);
        // Two versions of the same standalone patch (no cover letter => two
        // separate groups) + one unrelated standalone patch.
        app.patches = vec![
            patch("[PATCH v2] mm: fix", "b@x"),     // latest, not superseded
            patch("[PATCH] mm: fix", "a@x"),        // v1, superseded
            patch("[PATCH] net: unrelated", "c@x"), // standalone, not superseded
        ];
        app.rebuild_view();

        // No groups are expanded: all three rows are visible by default.
        assert_eq!(app.rows.len(), 3);

        // Pressing N hides the superseded v1, leaving the latest two.
        let key = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        app.handle_crossterm(key('N'));
        assert!(app.latest_only);
        assert_eq!(app.rows.len(), 2);
        let visible: Vec<&str> = app
            .rows
            .iter()
            .map(|r| app.patches[r.patch].message_id.as_str())
            .collect();
        assert!(visible.contains(&"b@x"), "v2 (latest) must be visible");
        assert!(visible.contains(&"c@x"), "unrelated must be visible");
        assert!(
            !visible.contains(&"a@x"),
            "superseded v1 must be hidden, got {visible:?}"
        );

        // Toggling back brings the superseded v1 back.
        app.handle_crossterm(key('N'));
        assert!(!app.latest_only);
        assert_eq!(app.rows.len(), 3);
    }

    #[test]
    fn latest_only_hides_reviewed_and_merged_patches() {
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

        let mut app = test_app(0);
        // Three standalone patches with already-known statuses.
        let mk = |subject: &str, id: &str, status: PatchStatus| PatchEntry {
            subject: subject.into(),
            author_name: "Dev".into(),
            author_email: "d@x".into(),
            message_id: id.into(),
            updated: None,
            status,
        };
        app.patches = vec![
            mk("[PATCH] open", "open@x", PatchStatus::Normal),
            mk("[PATCH] reviewed", "rev@x", PatchStatus::Reviewed),
            mk("[PATCH] merged", "mrg@x", PatchStatus::Merged),
        ];
        app.rebuild_view();
        assert_eq!(app.rows.len(), 3);

        let key = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        app.handle_crossterm(key('N'));
        assert_eq!(app.rows.len(), 1);
        assert_eq!(app.patches[app.rows[0].patch].message_id, "open@x");

        // Toggle back: all three return.
        app.handle_crossterm(key('N'));
        assert_eq!(app.rows.len(), 3);
    }

    #[test]
    fn latest_only_drops_patch_when_status_becomes_merged() {
        // start with latest_only on and an open patch visible; a status update
        // to Merged should remove it from the list.
        let mut app = test_app(0);
        app.patches = vec![patch("[PATCH] open", "a@x")];
        app.patches[0].status = PatchStatus::Normal;
        app.rebuild_view();

        app.latest_only = true;
        app.rebuild_view();
        assert_eq!(app.rows.len(), 1);

        // Simulate an async status probe returning Merged.
        app.on_status_updated("a@x", PatchStatus::Merged);
        assert_eq!(app.rows.len(), 0);
    }

    #[test]
    fn patch_set_rows_map_to_their_own_patches() {
        let mut app = test_app(0);
        app.patches = vec![
            patch("[PATCH 1/1] only", "msg-2-a@x"),
            patch("[PATCH 0/1] cover", "msg-1-a@x"),
        ];
        app.rebuild_view();
        // Expand the cover (keyed by its message-id).
        app.expanded.insert("msg-1-a@x".into());
        app.rebuild_rows();

        // Enter on either row opens that message's own thread.
        assert_eq!(app.patches[app.rows[0].patch].message_id, "msg-1-a@x");
        assert_eq!(app.patches[app.rows[1].patch].message_id, "msg-2-a@x");
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

    #[test]
    fn ctrl_d_and_u_move_half_page_in_list() {
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app(40);
        app.list_height = 10; // half page = 5
        let ctrl = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));

        app.handle_crossterm(ctrl('d'));
        assert_eq!(app.list_state.selected(), Some(5));
        app.handle_crossterm(ctrl('d'));
        assert_eq!(app.list_state.selected(), Some(10));
        app.handle_crossterm(ctrl('u'));
        assert_eq!(app.list_state.selected(), Some(5));
    }

    #[test]
    fn slash_starts_live_search() {
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app(0);
        app.patches = vec![
            patch("[PATCH] mm: fix foo", "a@x"),
            patch("[PATCH] net: bar", "b@x"),
            patch("[PATCH] mm: other", "c@x"),
        ];
        app.rebuild_view();
        let key = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));

        app.handle_crossterm(key('/'));
        assert!(app.search_active);
        app.handle_crossterm(key('m'));
        app.handle_crossterm(key('m'));
        assert_eq!(app.search.as_deref(), Some("mm"));
        assert_eq!(app.rows.len(), 2); // two "mm:" subjects

        app.handle_crossterm(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!app.search_active);
        assert!(app.search.is_none());
        assert_eq!(app.rows.len(), 3);
    }

    #[test]
    fn backspace_edits_the_search_query() {
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app(0);
        app.patches = vec![
            patch("[PATCH] mm: fix foo", "a@x"),
            patch("[PATCH] net: bar", "b@x"),
            patch("[PATCH] mm: other", "c@x"),
        ];
        app.all_loaded = true; // don't kick off network paging in the test
        app.rebuild_view();
        let key = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));

        app.handle_crossterm(key('/'));
        app.handle_crossterm(key('m'));
        app.handle_crossterm(key('e'));
        app.handle_crossterm(key('t'));
        assert_eq!(app.search.as_deref(), Some("met"));

        // Plain Backspace removes the last character and re-filters live.
        app.handle_crossterm(Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)));
        assert_eq!(app.search.as_deref(), Some("me"));
        assert!(app.search_active, "editing must not exit search mode");

        // Ctrl-H (how some terminals report backspace) also erases.
        app.handle_crossterm(Event::Key(KeyEvent::new(
            KeyCode::Char('h'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(app.search.as_deref(), Some("m"));
        assert_eq!(app.rows.len(), 2); // both "mm:" subjects match "m"
    }

    #[test]
    fn esc_from_search_keeps_selected_patch() {
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app(0);
        app.patches = vec![
            patch("[PATCH] mm: fix foo", "a@x"),
            patch("[PATCH] net: bar", "b@x"),
            patch("[PATCH] mm: other", "c@x"),
        ];
        app.rebuild_view();
        let key = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));

        // Search for "mm" -> two rows (a@x, c@x), then move to the second match.
        app.handle_crossterm(key('/'));
        app.handle_crossterm(key('m'));
        app.handle_crossterm(key('m'));
        assert_eq!(app.rows.len(), 2);
        app.handle_crossterm(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        let selected_id = app.selected_patch_id();
        assert_eq!(selected_id.as_deref(), Some("c@x"));

        // Esc restores the full list but keeps the same patch under the cursor.
        app.handle_crossterm(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(app.rows.len(), 3);
        assert_eq!(app.selected_patch_id().as_deref(), Some("c@x"));
    }

    #[test]
    fn esc_from_search_places_selection_at_quarter_height() {
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app(0);
        app.list_height = 20; // quarter = 5
        app.patches = (0..40)
            .map(|i| patch(&format!("[PATCH] item {i}"), &format!("id{i}@x")))
            .collect();
        app.rebuild_view();

        let key = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));

        // Search for a subject deep in the list so it can be scrolled.
        app.handle_crossterm(key('/'));
        for c in "item 30".chars() {
            app.handle_crossterm(key(c));
        }
        assert_eq!(app.selected_patch_id().as_deref(), Some("id30@x"));

        app.handle_crossterm(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        let selected = app.list_state.selected().unwrap();
        assert_eq!(app.patches[app.rows[selected].patch].message_id, "id30@x");
        // Selection sits a quarter of the viewport (5) below the top row.
        assert_eq!(app.list_state.offset(), selected - 5);
    }

    #[test]
    fn esc_from_search_on_collapsed_child_selects_group_head() {
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        // A cover-letter patch-set: parts are collapsed children in the normal
        // view but appear as flat rows in a search. Selecting a child in the
        // search list and pressing Esc must land on that set (its head), not on
        // a stale search-relative index.
        let mut app = test_app(0);
        // Message-ids share a git-send-email series stem so they group as one
        // patch-set (the middle `-<seq>-` is collapsed by `series_stem`).
        app.patches = vec![
            patch("[PATCH 0/2] feature xyz", "20240101-feat-0-abc@x"),
            patch("[PATCH 1/2] feature xyz: part one", "20240101-feat-1-abc@x"),
            patch("[PATCH 2/2] feature xyz: part two", "20240101-feat-2-abc@x"),
        ];
        app.rebuild_view();
        // Collapsed by default: only the cover-letter head is a row.
        assert_eq!(app.rows.len(), 1);
        assert_eq!(app.selected_patch_id().as_deref(), Some("20240101-feat-0-abc@x"));

        let key = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));

        // Search "part two" -> the child is the only match and is selected.
        app.handle_crossterm(key('/'));
        for c in "part two".chars() {
            app.handle_crossterm(key(c));
        }
        assert_eq!(app.rows.len(), 1);
        assert_eq!(app.selected_patch_id().as_deref(), Some("20240101-feat-2-abc@x"));

        // Esc: the child has no row of its own (group collapsed), so selection
        // falls back to the head of its group rather than the old index.
        app.handle_crossterm(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(app.rows.len(), 1);
        assert_eq!(app.selected_patch_id().as_deref(), Some("20240101-feat-0-abc@x"));
    }

    #[test]
    fn mark_toggles_and_hides_in_latest_only() {
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

        let mut app = test_app(0);
        app.patches = vec![patch("[PATCH] a", "a@x"), patch("[PATCH] b", "b@x")];
        app.patches[0].status = PatchStatus::Normal;
        app.patches[1].status = PatchStatus::Normal;
        app.rebuild_view();
        app.list_state.select(Some(0));

        let key = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));

        // Press 'm' to mark the first patch.
        app.handle_crossterm(key('m'));
        assert!(app.marked.contains("a@x"));

        // Press 'm' again to unmark it.
        app.handle_crossterm(key('m'));
        assert!(!app.marked.contains("a@x"));

        // Re-mark, then latest-only should hide it.
        app.handle_crossterm(key('m'));
        app.handle_crossterm(key('N'));
        assert!(app.latest_only);
        assert_eq!(app.rows.len(), 1);
        assert_eq!(app.patches[app.rows[0].patch].message_id, "b@x");
    }

    #[test]
    fn apply_targets_latest_loaded_version() {
        let mut app = test_app(0);
        // Two standalone versions of the same patch (newest first) + unrelated.
        app.patches = vec![
            patch("[PATCH v2] mm: fix", "v2@x"),
            patch("[PATCH] mm: fix", "v1@x"),
            patch("[PATCH] net: other", "n@x"),
        ];
        app.rebuild_view();

        // Select the older v1 row and request apply.
        let v1_row = app
            .rows
            .iter()
            .position(|r| app.patches[r.patch].message_id == "v1@x")
            .unwrap();
        app.list_state.select(Some(v1_row));

        let target = app.latest_series_target(app.selected_row().unwrap()).unwrap();
        assert_eq!(target.message_id, "v2@x", "should resolve to the latest version");

        // The lore URL is built from the resolved (latest) message-id.
        assert_eq!(
            app.apply_lore_url(&target.message_id),
            "https://lore.kernel.org/test/v2@x/"
        );
    }

    #[test]
    fn apply_confirm_opens_and_cancels() {
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

        let mut app = test_app(0);
        app.patches = vec![patch("[PATCH] a", "a@x")];
        app.rebuild_view();
        app.list_state.select(Some(0));

        let key = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));

        // 'A' opens the confirmation.
        app.handle_crossterm(key('A'));
        assert!(app.apply_confirm.is_some());

        // A non-'y' key cancels without applying.
        app.handle_crossterm(key('x'));
        assert!(app.apply_confirm.is_none());
        assert!(!app.apply_in_progress);
    }

    #[tokio::test]
    async fn cancel_apply_stops_progress_and_ignores_late_result() {
        let mut app = test_app(1);
        // Simulate a running apply with a real (long-sleeping) task handle.
        app.apply_in_progress = true;
        app.apply_task = Some(tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }));

        app.cancel_apply();
        assert!(!app.apply_in_progress, "cancel must stop the spinner");
        assert!(matches!(app.apply_result, Some(Err(_))), "cancel is surfaced");
        assert!(app.apply_task.is_none(), "task handle is dropped");

        // A late Applied event (task result arriving after cancel) is ignored.
        app.on_applied("a@x".into(), Ok("applied".into()));
        assert!(
            matches!(app.apply_result, Some(Err(_))),
            "late success must not overwrite the cancel message"
        );
    }

    #[test]
    fn on_applied_failure_clears_progress_and_shows_error() {
        let mut app = test_app(1);
        app.apply_in_progress = true;
        app.on_applied("a@x".into(), Err("b4 not found".into()));
        assert!(!app.apply_in_progress, "spinner must stop after a failure");
        assert!(matches!(app.apply_result, Some(Err(_))), "error is surfaced");
    }

    #[test]
    fn apply_result_dismissed_by_any_key() {
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

        let mut app = test_app(1);
        app.apply_result = Some(Ok("applied 1 patch".into()));

        let key = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        app.handle_crossterm(key);
        assert!(app.apply_result.is_none());
    }

    #[test]
    fn refresh_keeps_known_statuses_and_adds_new() {
        let mut app = test_app(0);
        app.patches = vec![patch("[PATCH] a", "a@x"), patch("[PATCH] b", "b@x")];
        app.patches[0].status = PatchStatus::Merged;
        app.rebuild_view();

        // A refresh returns the still-present a@x plus a brand-new c@x.
        let refreshed = vec![patch("[PATCH] c new", "c@x"), patch("[PATCH] a", "a@x")];
        app.on_patches_loaded(Ok(refreshed));

        assert_eq!(app.patches.len(), 2);
        let a = app.patches.iter().find(|p| p.message_id == "a@x").unwrap();
        assert_eq!(a.status, PatchStatus::Merged); // carried over
        let c = app.patches.iter().find(|p| p.message_id == "c@x").unwrap();
        assert_eq!(c.status, PatchStatus::Unknown); // new -> will be probed
    }
}
