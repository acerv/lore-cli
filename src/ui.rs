use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::model::PatchEntry;

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
        let msg = format!(" Loading patches {spin}");
        frame.render_widget(Paragraph::new(msg), area);
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
        .map(|patch| ListItem::new(format_row(patch, text_width)))
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_helpbar(frame: &mut Frame, area: Rect) {
    let help = " ↑/↓ move   Enter open   q quit ";
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

/// Format a single patch into an aligned "subject | author | date" row.
fn format_row(patch: &PatchEntry, width: usize) -> String {
    const AUTHOR_W: usize = 22;
    const DATE_W: usize = 10;

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

    // subject gets the remaining width (minus two single-space separators).
    let subject_w = width.saturating_sub(AUTHOR_W + DATE_W + 2).max(4);
    let subject = fit(&patch.subject, subject_w);

    format!("{subject:<subject_w$} {author:<AUTHOR_W$} {date}")
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
