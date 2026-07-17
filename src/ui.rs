use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::model::{PatchEntry, PatchStatus};

const SPINNER: [char; 4] = ['|', '/', '-', '\\'];

pub fn render(frame: &mut Frame, app: &mut App) {
    let areas = Layout::vertical([
        Constraint::Length(1), // title / tab bar
        Constraint::Min(0),    // body
        Constraint::Length(1), // help bar
    ])
    .split(frame.area());

    render_titlebar(frame, app, areas[0]);
    render_list(frame, app, areas[1]);
    render_helpbar(frame, areas[2]);
}

fn render_titlebar(frame: &mut Frame, app: &mut App, area: Rect) {
    let count = if app.loading_patches {
        String::new()
    } else {
        format!("  ({} patches)", app.patches.len())
    };
    let title = format!(" lore-cli — {}{} ", app.config.lore.project, count);
    let bar = Paragraph::new(title).style(
        Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(bar, area);
}

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

fn render_helpbar(frame: &mut Frame, area: Rect) {
    let dim = Style::default().fg(Color::DarkGray);
    let line = Line::from(vec![
        Span::styled(" ↑/↓ move  Enter open  q quit", dim),
        Span::raw("    "),
        Span::styled("■ merged", Style::default().fg(Color::Green)),
        Span::raw("  "),
        Span::styled("■ reviewed", Style::default().fg(Color::Yellow)),
        Span::raw("  "),
        Span::styled("■ loading", dim),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// Terminal color used for a patch row of the given status.
fn status_color(status: PatchStatus) -> Color {
    match status {
        PatchStatus::Merged => Color::Green,
        PatchStatus::Reviewed => Color::Yellow,
        PatchStatus::Normal => Color::Reset,
        PatchStatus::Unknown => Color::DarkGray,
    }
}

/// Single-character marker shown before a patch (color carries the main signal).
fn status_marker(status: PatchStatus) -> char {
    match status {
        PatchStatus::Merged => 'M',
        PatchStatus::Reviewed => 'R',
        PatchStatus::Normal => ' ',
        PatchStatus::Unknown => '·',
    }
}

/// Format a patch into an aligned "marker subject | author | date" row.
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

        // Rows start at y=1 (y=0 is the title bar), in patch order.
        assert!(row_has_fg(buffer, 1, Color::Green), "merged row should be green");
        assert!(row_has_fg(buffer, 2, Color::Yellow), "reviewed row should be yellow");
        assert!(row_has_fg(buffer, 4, Color::DarkGray), "unknown row should be dim");
    }
}
