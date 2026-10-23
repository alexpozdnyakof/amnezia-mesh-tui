use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::{App, ProbeState};

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(f.area());

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " awg-mesh-tui ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " server: {} (ssh: {}) ",
            app.server_name, app.server.ssh_alias
        )),
    ]));
    f.render_widget(title, chunks[0]);

    let mut items = Vec::new();
    match &app.probe_state {
        ProbeState::Idle => items.push(ListItem::new("status: idle")),
        ProbeState::Running => items.push(ListItem::new("status: probing…")),
        ProbeState::Done => {
            items.push(ListItem::new(Span::styled(
                "status: connected",
                Style::default().fg(Color::Green),
            )));
        }
    }
    if let Some(p) = &app.probe {
        items.push(match (&p.docker_path, &p.docker_version) {
            (Some(path), Some(ver)) => ListItem::new(format!("docker: {path} (server {ver})")),
            (Some(path), None) => ListItem::new(format!("docker: {path} (daemon unreachable)")),
            _ => ListItem::new(Span::styled(
                "docker: not found",
                Style::default().fg(Color::Red),
            )),
        });
        items.push(match &p.cli_version {
            Some(v) => ListItem::new(format!("awg-mesh CLI: {v}")),
            None => ListItem::new(Span::styled(
                "awg-mesh CLI: not found (bootstrap candidate)",
                Style::default().fg(Color::Yellow),
            )),
        });
        for note in &p.notes {
            items.push(ListItem::new(Span::styled(
                format!("note: {note}"),
                Style::default().fg(Color::Yellow),
            )));
        }
    }

    let body = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" connection / preflight "),
    );
    f.render_widget(body, chunks[1]);

    let hints = Paragraph::new(" q quit · r re-probe ")
        .block(Block::default().borders(Borders::ALL).title(" keys "));
    f.render_widget(hints, chunks[2]);
}
