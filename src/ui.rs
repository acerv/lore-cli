use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use crate::app::{App, Row, ThreadTab};
use crate::model::{Email, PatchEntry, PatchStatus};

const SPINNER: [char; 4] = ['|', '/', '-', '\\'];

pub fn render(frame: &mut Frame, app: &mut App) {
    let areas = Layout::vertical([
        Constraint::Length(1), // tab bar
        Constraint::Length(1), // separator rule
        Constraint::Min(0),    // body
        Constraint::Length(1), // status bar
    ])
    .split(frame.area());

    render_tabbar(frame, app, areas[0]);
    frame.render_widget(Block::default().borders(Borders::TOP), areas[1]);

    let tick = app.tick;
    if app.active_tab == 0 {
        render_list(frame, app, areas[2]);
    } else if let Some(tab) = app.tabs.get_mut(app.active_tab - 1) {
        render_thread(frame, tab, areas[2], tick);
    }

    render_statusbar(frame, app, areas[3]);
}

fn render_tabbar(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = std::iter::once(format!(" {} ", app.config.lore.project))
        .chain(app.tabs.iter().map(|t| format!(" {} ", fit(&t.subject, 20))))
        .map(Line::from)
        .collect();

    let tabs = Tabs::new(titles)
        .select(app.active_tab)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    frame.render_widget(tabs, area);
}

// ----- patch list ----------------------------------------------------------

fn render_list(frame: &mut Frame, app: &mut App, area: Rect) {
    app.list_height = area.height;
    if app.loading_patches {
        let spin = SPINNER[(app.tick % 4) as usize];
        frame.render_widget(
            Paragraph::new(format!(" Loading patches {spin}")).style(dim()),
            area,
        );
        return;
    }
    if let Some(err) = &app.error {
        let para = Paragraph::new(format!(" Error: {err}"))
            .style(Style::default().fg(Color::Red))
            .wrap(Wrap { trim: false });
        frame.render_widget(para, area);
        return;
    }
    if app.patches.is_empty() {
        frame.render_widget(Paragraph::new(" No patches found.").style(dim()), area);
        return;
    }

    let width = area.width as usize;
    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| ListItem::new(tree_row(&app.patches[row.patch], row, width)))
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn status_color(status: PatchStatus) -> Color {
    match status {
        PatchStatus::Merged => Color::Green,
        PatchStatus::Reviewed => Color::Yellow,
        PatchStatus::Normal => Color::Reset,
        PatchStatus::Unknown => Color::DarkGray,
    }
}

fn status_marker(status: PatchStatus) -> char {
    match status {
        PatchStatus::Merged => 'M',
        PatchStatus::Reviewed => 'R',
        PatchStatus::Normal | PatchStatus::Unknown => ' ',
    }
}

/// Build one list row for the version tree.
///
/// A head with older versions shows the version count (`▸N` collapsed / `▾N`
/// expanded) in red at the very start, in place of the status marker; nested
/// versions are indented and keep their own status marker.
fn tree_row(patch: &PatchEntry, row: &Row, width: usize) -> Line<'static> {
    const AUTHOR_W: usize = 22;
    const DATE_W: usize = 10;

    let color = status_color(patch.status);
    let date = patch
        .updated
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| " ".repeat(DATE_W));
    let author_raw = if patch.author_name.is_empty() {
        &patch.author_email
    } else {
        &patch.author_name
    };
    let author = fit(&sanitize(author_raw), AUTHOR_W);

    // Leading tag: red version count for a version-tree head, otherwise the
    // status marker in the status color.
    let (tag, tag_style) = if row.depth == 0 && row.children > 0 {
        let arrow = if row.expanded { '▾' } else { '▸' };
        (
            format!("{arrow}{}", row.children + 1),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else {
        (
            status_marker(patch.status).to_string(),
            Style::default().fg(color),
        )
    };

    let prefix = if row.depth > 0 { "  └ " } else { "" };
    let subject_w = width.saturating_sub(39).max(4);
    let subject = fit(&patch.subject, subject_w.saturating_sub(prefix.chars().count()).max(1));
    let subject_cell = format!("{prefix}{subject}");

    Line::from(vec![
        Span::styled(format!("{tag:<2} "), tag_style),
        Span::styled(format!("{subject_cell:<subject_w$} "), Style::default().fg(color)),
        Span::styled(format!("{author:<AUTHOR_W$} "), dim()),
        Span::styled(date, Style::default().fg(Color::Cyan)),
    ])
}

// ----- thread view ----------------------------------------------------------

fn render_thread(frame: &mut Frame, tab: &mut ThreadTab, area: Rect, tick: u64) {
    if tab.loading {
        let spin = SPINNER[(tick % 4) as usize];
        frame.render_widget(
            Paragraph::new(format!(" Loading thread {spin}")).style(dim()),
            area,
        );
        return;
    }
    if let Some(err) = &tab.error {
        let para = Paragraph::new(format!(" Error: {err}"))
            .style(Style::default().fg(Color::Red))
            .wrap(Wrap { trim: false });
        frame.render_widget(para, area);
        return;
    }
    if tab.emails.is_empty() {
        frame.render_widget(Paragraph::new(" (empty thread)").style(dim()), area);
        return;
    }

    let lines = build_thread_lines(&tab.emails, area.width as usize);

    // No wrapping: one row per logical line, so the count is exact and paging
    // never overshoots. Over-long lines are clipped at the right edge.
    tab.content_len = lines.len() as u16;
    tab.viewport_height = area.height;
    let max_scroll = tab.content_len.saturating_sub(area.height);
    tab.scroll = tab.scroll.min(max_scroll);

    frame.render_widget(Paragraph::new(lines).scroll((tab.scroll, 0)), area);
}

fn build_thread_lines(emails: &[Email], width: usize) -> Vec<Line<'static>> {
    let depths = reply_depths(emails);
    let mut lines: Vec<Line> = Vec::new();

    for (i, email) in emails.iter().enumerate() {
        let indent = "  ".repeat(depths[i].min(6));
        if i > 0 {
            lines.push(Line::raw(""));
            lines.push(Line::styled("─".repeat(width.max(1)), dim()));
            lines.push(Line::raw(""));
        }
        lines.push(header_line(&indent, "From : ", &email.from));
        lines.push(header_line(&indent, "Date : ", &email.date));
        lines.push(header_line(&indent, "Subj : ", &email.subject));
        lines.push(Line::raw(""));

        let mut in_diff = false;
        for raw in email.body.lines() {
            let text = sanitize(raw);
            in_diff = next_diff_state(&text, in_diff);
            let style = body_line_style(&text, in_diff);
            let line = if indent.is_empty() {
                Line::styled(text, style)
            } else {
                Line::from(vec![Span::raw(indent.clone()), Span::styled(text, style)])
            };
            lines.push(line);
        }
    }
    lines
}

fn header_line(indent: &str, label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{indent}{label}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(sanitize(value)),
    ])
}

/// Compute a reply-nesting depth per email from `In-Reply-To` links.
fn reply_depths(emails: &[Email]) -> Vec<usize> {
    use std::collections::HashMap;
    let index: HashMap<&str, usize> = emails
        .iter()
        .enumerate()
        .map(|(i, e)| (e.message_id.as_str(), i))
        .collect();
    let mut depths = vec![0usize; emails.len()];
    for i in 0..emails.len() {
        if let Some(parent) = emails[i]
            .in_reply_to
            .as_deref()
            .and_then(|irt| index.get(irt).copied())
        {
            if parent < i {
                depths[i] = depths[parent] + 1;
            }
        }
    }
    depths
}

/// Track whether we are inside a diff hunk (kingi-style stateful detection).
fn next_diff_state(line: &str, in_diff: bool) -> bool {
    if line.starts_with("diff ") || line.starts_with("--- ") || line.starts_with("+++ ") {
        true
    } else if in_diff
        && !line.starts_with("@@")
        && !line.starts_with('+')
        && !line.starts_with('-')
        && !line.starts_with(' ')
        && !line.is_empty()
    {
        false
    } else {
        in_diff
    }
}

/// Light syntax coloring for a body line (diff coloring only inside hunks).
fn body_line_style(line: &str, in_diff: bool) -> Style {
    if in_diff && line.starts_with("--- ") {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if in_diff && line.starts_with("+++ ") {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else if in_diff && line.starts_with("@@") {
        Style::default().fg(Color::Cyan)
    } else if in_diff && line.starts_with('-') {
        Style::default().fg(Color::Red)
    } else if in_diff && line.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if line.trim_start().starts_with('>') {
        Style::default().fg(Color::Blue)
    } else if is_trailer(line.trim_start()) {
        Style::default().fg(Color::Green)
    } else {
        Style::default()
    }
}

fn is_trailer(line: &str) -> bool {
    const TRAILERS: [&str; 7] = [
        "reviewed-by:",
        "signed-off-by:",
        "acked-by:",
        "tested-by:",
        "reported-by:",
        "suggested-by:",
        "fixes:",
    ];
    let lower = line.to_ascii_lowercase();
    TRAILERS.iter().any(|t| lower.starts_with(t))
}

// ----- status bar -----------------------------------------------------------

fn render_statusbar(frame: &mut Frame, app: &App, area: Rect) {
    let widget = if app.active_tab == 0 {
        if let Some(err) = &app.error {
            Paragraph::new(format!(" error: {err}")).style(Style::default().fg(Color::Red))
        } else {
            let mut spans = vec![
                Span::styled(" ↑/↓ move  Enter open  Space fold  Ctrl+n/p tab  q quit", dim()),
                Span::styled("  |  ", dim()),
            ];
            if app.loading_patches {
                let spin = SPINNER[(app.tick % 4) as usize];
                spans.push(Span::styled(
                    format!("loading {spin}"),
                    Style::default().fg(Color::Yellow),
                ));
            } else {
                spans.push(Span::styled(format!("{} patches", app.patches.len()), dim()));
                if app.loading_more {
                    spans.push(Span::styled(" +more", Style::default().fg(Color::Yellow)));
                }
            }
            spans.push(Span::styled("  |  ", dim()));
            spans.push(Span::styled("merged", Style::default().fg(Color::Green)));
            spans.push(Span::styled("  ", dim()));
            spans.push(Span::styled("reviewed", Style::default().fg(Color::Yellow)));
            Paragraph::new(Line::from(spans))
        }
    } else {
        Paragraph::new(Line::from(Span::styled(
            " ↑/↓ scroll  Ctrl+d/u fast  Ctrl+n/p tab  q close",
            dim(),
        )))
    };
    frame.render_widget(widget, area);
}

// ----- helpers --------------------------------------------------------------

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// Expand tabs to 8-column stops and drop other control characters, so the
/// terminal never receives raw control bytes (which desync the display and
/// leave stale glyphs from the previous frame).
fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut col = 0usize;
    for ch in text.chars() {
        match ch {
            '\t' => {
                let spaces = 8 - (col % 8);
                out.extend(std::iter::repeat_n(' ', spaces));
                col += spaces;
            }
            c if c.is_control() => {}
            c => {
                out.push(c);
                col += 1;
            }
        }
    }
    out
}

/// Truncate to `max` display columns (approximated by chars), adding an ellipsis.
fn fit(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::config::{Config, LoreConfig, StatusConfig, UiConfig};
    use crate::lore::LoreClient;
    use crate::model::PatchEntry;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use tokio::sync::mpsc::unbounded_channel;

    fn app_with_statuses(statuses: &[PatchStatus]) -> App {
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
        app.patches = statuses
            .iter()
            .enumerate()
            .map(|(i, &status)| PatchEntry {
                subject: format!("subject number {i}"),
                author_name: "Developer".into(),
                author_email: "dev@example.com".into(),
                message_id: format!("id{i}@example.com"),
                updated: None,
                status,
            })
            .collect();
        app.rebuild_view();
        app
    }

    fn row_has_fg(buffer: &Buffer, y: u16, color: Color) -> bool {
        (0..buffer.area.width).any(|x| buffer.cell((x, y)).is_some_and(|c| c.fg == color))
    }

    fn buffer_text(buffer: &Buffer) -> String {
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if let Some(cell) = buffer.cell((x, y)) {
                    text.push_str(cell.symbol());
                }
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn colors_rows_by_status() {
        let mut app = app_with_statuses(&[
            PatchStatus::Merged,
            PatchStatus::Reviewed,
            PatchStatus::Normal,
            PatchStatus::Unknown,
        ]);
        app.list_state.select(None); // avoid the selection highlight recoloring a row

        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();

        // Body starts at y=2 (y=0 tab bar, y=1 separator), in patch order.
        assert!(row_has_fg(buffer, 2, Color::Green), "merged row should be green");
        assert!(row_has_fg(buffer, 3, Color::Yellow), "reviewed row should be yellow");
        assert!(!row_has_fg(buffer, 4, Color::Green), "normal row must not be green");
        assert!(!row_has_fg(buffer, 4, Color::Yellow), "normal row must not be yellow");
    }

    #[test]
    fn body_line_styling() {
        assert_eq!(body_line_style("> quoted", false).fg, Some(Color::Blue));
        assert_eq!(body_line_style("Reviewed-by: X", false).fg, Some(Color::Green));
        assert_eq!(body_line_style("normal text", false).fg, None);
        assert_eq!(body_line_style("+added", true).fg, Some(Color::Green));
        assert_eq!(body_line_style("-removed", true).fg, Some(Color::Red));
        assert_eq!(body_line_style("- bullet", false).fg, None); // outside a diff
    }

    #[test]
    fn sanitize_expands_tabs_and_drops_controls() {
        assert_eq!(sanitize("a\tb"), "a       b"); // tab from col 1 -> col 8
        assert!(!sanitize("x\ry").contains('\r'));
    }

    #[test]
    fn reply_depths_follow_in_reply_to() {
        let mk = |id: &str, irt: Option<&str>| Email {
            from: "x".into(),
            date: "d".into(),
            subject: "s".into(),
            message_id: id.into(),
            in_reply_to: irt.map(str::to_string),
            body: String::new(),
        };
        let emails = vec![
            mk("root", None),
            mk("a", Some("root")),
            mk("b", Some("a")),
            mk("c", Some("root")),
        ];
        assert_eq!(reply_depths(&emails), vec![0, 1, 2, 1]);
    }

    #[test]
    fn renders_thread_tab_content() {
        let mut app = app_with_statuses(&[PatchStatus::Normal]);
        app.tabs.push(ThreadTab {
            message_id: "id0@example.com".into(),
            subject: "subject number 0".into(),
            emails: vec![Email {
                from: "Alice <alice@example.com>".into(),
                date: "Mon, 1 Jan 2024".into(),
                subject: "[PATCH] thing".into(),
                message_id: "id0@example.com".into(),
                in_reply_to: None,
                body: "Hello\tworld\nReviewed-by: Bob\n".into(),
            }],
            loading: false,
            error: None,
            scroll: 0,
            viewport_height: 0,
            content_len: 0,
        });
        app.active_tab = 1;

        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let text = buffer_text(terminal.backend().buffer());

        assert!(text.contains("Alice"), "thread should show the sender");
        assert!(text.contains("Reviewed-by: Bob"), "thread should show the body");
        assert!(!text.contains('\t'), "tabs must be expanded, never rendered raw");
    }

    #[test]
    fn version_count_shown_in_red_at_start() {
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
        app.patches = vec![
            PatchEntry {
                subject: "[PATCH v2] mm: fix".into(),
                author_name: "Dev".into(),
                author_email: "d@x".into(),
                message_id: "v2@x".into(),
                updated: None,
                status: PatchStatus::Normal,
            },
            PatchEntry {
                subject: "[PATCH] mm: fix".into(),
                author_name: "Dev".into(),
                author_email: "d@x".into(),
                message_id: "v1@x".into(),
                updated: None,
                status: PatchStatus::Normal,
            },
        ];
        app.rebuild_view();
        app.list_state.select(None);

        let mut terminal = Terminal::new(TestBackend::new(80, 6)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();

        // The head row (y=2) shows the version count in red.
        assert!(row_has_fg(buffer, 2, Color::Red), "version count should be red");
        let text = buffer_text(buffer);
        assert!(text.contains("▸2"), "should show 2 versions at the start, got:\n{text}");
    }
}
