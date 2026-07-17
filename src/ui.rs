use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use crate::app::{App, ThreadTab};
use crate::model::{Email, PatchEntry, PatchStatus};

const SPINNER: [char; 4] = ['|', '/', '-', '\\'];

pub fn render(frame: &mut Frame, app: &mut App) {
    let areas = Layout::vertical([
        Constraint::Length(1), // tab bar
        Constraint::Min(0),    // body
        Constraint::Length(1), // help bar
    ])
    .split(frame.area());

    render_tabbar(frame, app, areas[0]);

    let tick = app.tick;
    if app.active_tab == 0 {
        render_list(frame, app, areas[1]);
    } else if let Some(tab) = app.tabs.get_mut(app.active_tab - 1) {
        render_thread(frame, tab, areas[1], tick);
    }

    render_helpbar(frame, app, areas[2]);
}

fn render_tabbar(frame: &mut Frame, app: &App, area: Rect) {
    let patches_title = if app.loading_patches {
        " Patches ".to_string()
    } else {
        format!(" Patches ({}) ", app.patches.len())
    };
    let mut titles: Vec<Line> = vec![Line::from(patches_title)];
    for tab in &app.tabs {
        titles.push(Line::from(format!(" {} ", fit(&tab.subject, 24))));
    }

    let tabs = Tabs::new(titles)
        .select(app.active_tab)
        .style(Style::default().bg(Color::Blue).fg(Color::Gray))
        .highlight_style(
            Style::default()
                .bg(Color::White)
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
        .divider("");
    frame.render_widget(tabs, area);
}

// ----- patch list ----------------------------------------------------------

fn render_list(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.loading_patches {
        let spin = SPINNER[(app.tick % 4) as usize];
        frame.render_widget(Paragraph::new(format!(" Loading patches {spin}")), area);
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
        frame.render_widget(Paragraph::new(" No patches found."), area);
        return;
    }

    let text_width = area.width.saturating_sub(2) as usize; // room for highlight symbol
    let items: Vec<ListItem> = app
        .patches
        .iter()
        .map(|patch| {
            ListItem::new(format_row(patch, text_width))
                .style(Style::default().fg(status_color(patch.status)))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
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
        PatchStatus::Normal => ' ',
        PatchStatus::Unknown => '·',
    }
}

fn format_row(patch: &PatchEntry, width: usize) -> String {
    const AUTHOR_W: usize = 22;
    const DATE_W: usize = 10;

    let marker = status_marker(patch.status);
    let date = patch
        .updated
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| " ".repeat(DATE_W));
    let author_raw = if patch.author_name.is_empty() {
        &patch.author_email
    } else {
        &patch.author_name
    };
    let author = fit(author_raw, AUTHOR_W);

    // marker(1) + ' '(1) + subject + ' '(1) + author + ' '(1) + date
    let subject_w = width.saturating_sub(2 + AUTHOR_W + DATE_W + 2).max(4);
    let subject = fit(&patch.subject, subject_w);

    format!("{marker} {subject:<subject_w$} {author:<AUTHOR_W$} {date}")
}

// ----- thread view ----------------------------------------------------------

fn render_thread(frame: &mut Frame, tab: &mut ThreadTab, area: Rect, tick: u64) {
    if tab.loading {
        let spin = SPINNER[(tick % 4) as usize];
        frame.render_widget(Paragraph::new(format!(" Loading thread {spin}")), area);
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
        frame.render_widget(Paragraph::new(" (empty thread)"), area);
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
    let label = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line> = Vec::new();

    for (i, email) in emails.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
            lines.push(Line::styled("─".repeat(width.max(1)), label));
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(vec![
            Span::styled("From: ", label),
            Span::styled(
                email.from.clone(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Date: ", label),
            Span::raw(email.date.clone()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Subj: ", label),
            Span::raw(email.subject.clone()),
        ]));
        lines.push(Line::raw(""));
        for raw in email.body.lines() {
            lines.push(style_body_line(raw));
        }
    }
    lines
}

/// Apply light syntax coloring to a single body line.
fn style_body_line(raw: &str) -> Line<'static> {
    let trimmed = raw.trim_start();
    let style = if trimmed.starts_with('>') {
        Style::default().fg(Color::Blue) // quoted text
    } else if raw.to_ascii_lowercase().contains("merged, thanks") {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else if is_trailer(trimmed) {
        Style::default().fg(Color::Green)
    } else if raw.starts_with("@@") {
        Style::default().fg(Color::Cyan)
    } else if raw.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if raw.starts_with('-') && !raw.starts_with("-- ") {
        Style::default().fg(Color::Red)
    } else {
        Style::default()
    };
    Line::styled(raw.to_string(), style)
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

// ----- help bar -------------------------------------------------------------

fn render_helpbar(frame: &mut Frame, app: &App, area: Rect) {
    let dim = Style::default().fg(Color::DarkGray);
    let line = if app.active_tab == 0 {
        Line::from(vec![
            Span::styled(" ↑/↓ move  Enter open  Ctrl+n/p tab  q quit", dim),
            Span::raw("    "),
            Span::styled("■ merged", Style::default().fg(Color::Green)),
            Span::raw("  "),
            Span::styled("■ reviewed", Style::default().fg(Color::Yellow)),
            Span::raw("  "),
            Span::styled("■ loading", dim),
        ])
    } else {
        Line::from(Span::styled(
            " ↑/↓ scroll  Ctrl+d/u fast  Ctrl+n/p tab  q close",
            dim,
        ))
    };
    frame.render_widget(Paragraph::new(line), area);
}

// ----- helpers --------------------------------------------------------------

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
    use crate::config::{Config, LoreConfig, UiConfig};
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

        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();

        // Rows start at y=1 (y=0 is the tab bar), in patch order.
        assert!(row_has_fg(buffer, 1, Color::Green), "merged row should be green");
        assert!(row_has_fg(buffer, 2, Color::Yellow), "reviewed row should be yellow");
        assert!(row_has_fg(buffer, 4, Color::DarkGray), "unknown row should be dim");
    }

    #[test]
    fn body_line_styling() {
        assert_eq!(style_body_line("> quoted").style.fg, Some(Color::Blue));
        assert_eq!(style_body_line("Reviewed-by: X").style.fg, Some(Color::Green));
        assert_eq!(style_body_line("Merged, thanks!").style.fg, Some(Color::Green));
        assert_eq!(style_body_line("normal text").style.fg, None);
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
                body: "Hello world\nReviewed-by: Bob\n".into(),
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
    }
}
