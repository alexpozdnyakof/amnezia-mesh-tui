use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table, Wrap,
};
use ratatui::Frame;

use crate::app::{App, ProbeState, Screen};
use crate::bootstrap::{BOOTSTRAP_DIR, BOOTSTRAP_REPO};
use crate::peers::{format_age, format_rate, PeerStatus};

const DIM: Style = Style::new().fg(Color::DarkGray);
const GOOD: Style = Style::new().fg(Color::Green);
const WARN: Style = Style::new().fg(Color::Yellow);
const BAD: Style = Style::new().fg(Color::Red);

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(f.area());

    draw_header(f, app, chunks[0]);
    match app.screen {
        Screen::Preflight => draw_preflight(f, app, chunks[1]),
        Screen::Discovery => draw_discovery(f, app, chunks[1]),
        Screen::Inspect => draw_inspect(f, app, chunks[1]),
        Screen::Dashboard => draw_dashboard(f, app, chunks[1]),
    }
    draw_hints(f, app, chunks[2]);
}

fn draw_header(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
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
    f.render_widget(title, area);
}

fn draw_preflight(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let mut items: Vec<ListItem> = Vec::new();
    match &app.probe_state() {
        ProbeState::Idle => items.push(ListItem::new("status: idle")),
        ProbeState::Running => items.push(ListItem::new("status: probing…")),
        ProbeState::Done => items.push(ListItem::new(Span::styled("status: connected", GOOD))),
    }
    if let Some(p) = &app.probe {
        items.push(match (&p.docker_path, &p.docker_version) {
            (Some(path), Some(ver)) => ListItem::new(format!("docker: {path} (server {ver})")),
            (Some(path), None) => ListItem::new(format!("docker: {path} (daemon unreachable)")),
            _ => ListItem::new(Span::styled("docker: not found", BAD)),
        });
        items.push(match &p.cli_version {
            Some(v) => ListItem::new(format!("awg-mesh CLI: {v}")),
            None => ListItem::new(Span::styled(
                "awg-mesh CLI: not found (bootstrap candidate)",
                WARN,
            )),
        });
        for note in &p.notes {
            items.push(ListItem::new(Span::styled(format!("note: {note}"), WARN)));
        }
    }

    let b = &app.bootstrap;
    items.push(ListItem::new(""));
    if b.running {
        items.push(ListItem::new(Span::styled(
            format!("bootstrap: cloning {BOOTSTRAP_REPO} → {BOOTSTRAP_DIR}…"),
            WARN,
        )));
    } else if b.confirm_pending {
        items.push(ListItem::new(Span::styled(
            format!(
                "bootstrap will clone {BOOTSTRAP_REPO}\n             into {BOOTSTRAP_DIR} on the server"
            ),
            WARN,
        )));
        items.push(ListItem::new(Span::styled(
            "press B to confirm, Esc to cancel",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    }
    if let Some(m) = &b.message {
        items.push(ListItem::new(Span::styled(format!("bootstrap: {m}"), GOOD)));
    }
    if let Some(e) = &b.error {
        items.push(ListItem::new(Span::styled(
            format!("bootstrap error: {e}"),
            BAD,
        )));
    }

    let body = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" connection / preflight "),
    );
    f.render_widget(body, area);
}

fn draw_discovery(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let d = &app.discovery;
    if d.running && d.containers.is_empty() {
        let body = Paragraph::new("scanning docker containers…")
            .block(Block::default().borders(Borders::ALL).title(" discovery "));
        f.render_widget(body, area);
        return;
    }

    let mut items = Vec::new();
    if let Some(e) = &d.error {
        items.push(ListItem::new(Span::styled(format!("error: {e}"), BAD)));
    }
    let has_candidates = d.containers.iter().any(|c| c.candidate);
    if !d.containers.is_empty() && !has_candidates {
        items.push(ListItem::new(Span::styled(
            "no AmneziaWG containers found — check the image name or use init",
            WARN,
        )));
    }
    for c in &d.containers {
        let (marker, style) = if c.candidate {
            ("◆", GOOD)
        } else {
            ("·", DIM)
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("{marker} "), style),
            Span::styled(
                format!("{:<14} ", c.id.chars().take(12).collect::<String>()),
                DIM,
            ),
            Span::styled(
                format!("{:<24}", c.name),
                if c.candidate { Style::default() } else { DIM },
            ),
            Span::styled(format!("{:<40} ", c.image), DIM),
            Span::styled(&c.status, if c.running { GOOD } else { DIM }),
        ])));
    }
    if d.running {
        items.push(ListItem::new(Span::styled("re-scanning…", WARN)));
    }

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " discovery — {} container(s), ◆ = migration candidate ",
            d.containers.len()
        )))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut state =
        ListState::default().with_selected((!d.containers.is_empty()).then_some(d.selected));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_inspect(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let i = &app.inspect;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" inspect: {} ", i.container));

    if i.running && i.summary.is_none() {
        let body = Paragraph::new("reading configuration (read-only)…").block(block);
        f.render_widget(body, area);
        return;
    }

    let mut text: Vec<Line> = Vec::new();
    match &i.summary {
        Some(s) => {
            let mut head = vec![
                Span::styled("source: ", DIM),
                Span::raw(&s.source),
                Span::styled("   image: ", DIM),
                Span::raw(&s.image),
            ];
            if let Some(err) = &i.error {
                head.push(Span::styled(format!("   error: {err}"), BAD));
            }
            text.push(Line::from(head));
            text.push(Line::from(vec![
                Span::styled("subnet: ", DIM),
                Span::raw(&s.subnet),
                Span::styled("   mtu: ", DIM),
                Span::raw(&s.mtu),
                Span::styled("   listen: ", DIM),
                Span::raw(&s.listen_port),
                Span::styled("   keepalive: ", DIM),
                Span::raw(&s.server_keepalive),
                Span::styled("   obfuscation params: ", DIM),
                Span::raw(s.obfuscation_params.to_string()),
            ]));
            text.push(Line::from(vec![
                Span::styled("server pubkey: ", DIM),
                Span::raw(&s.server_pub),
            ]));
            text.push(Line::from(""));
            text.push(Line::from(Span::styled(
                format!("peers ({}):", s.peers.len()),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for p in &s.peers {
                let created = p.created.as_deref().unwrap_or("—");
                text.push(Line::from(vec![
                    Span::styled("  ● ", GOOD),
                    Span::raw(format!("{:<32} ", p.name)),
                    Span::styled(format!("{:<18} ", p.ip), DIM),
                    Span::styled(created, DIM),
                ]));
            }
            for w in &s.warnings {
                text.push(Line::from(Span::styled(format!("warning: {w}"), WARN)));
            }
        }
        None => {
            if let Some(e) = &i.error {
                text.push(Line::from(Span::styled(format!("error: {e}"), BAD)));
            } else {
                text.push(Line::from("no data"));
            }
        }
    }

    let body = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
    f.render_widget(body, area);
}

fn draw_hints(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let hints: &str = match app.screen {
        Screen::Preflight => {
            if app.bootstrap.confirm_pending {
                " B confirm bootstrap · Esc cancel "
            } else {
                " q quit · r re-probe · d discover containers · b bootstrap CLI · g dashboard "
            }
        }
        Screen::Discovery => " ↑↓ select · Enter inspect · d re-scan · Esc back ",
        Screen::Inspect => " Esc back · q quit ",
        Screen::Dashboard => " Esc back · auto-refresh 3s ",
    };
    let hints = Paragraph::new(hints).block(Block::default().borders(Borders::ALL).title(" keys "));
    f.render_widget(hints, area);
}

fn draw_dashboard(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let d = &app.dashboard;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(5),
        ])
        .split(area);

    let services = match &d.services_error {
        Some(e) => Line::from(Span::styled(format!("services: {e}"), BAD)),
        None if d.services.is_empty() => Line::from(Span::styled(
            "services: no amnezia-mesh stack containers (not initialized yet?)".to_string(),
            DIM,
        )),
        None => Line::from(
            d.services
                .iter()
                .map(|c| {
                    Span::styled(
                        format!(" {}: {} ", c.name, c.status),
                        if c.running { GOOD } else { WARN },
                    )
                })
                .collect::<Vec<_>>(),
        ),
    };
    f.render_widget(
        Paragraph::new(services).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" services (amnezia-mesh stack) "),
        ),
        chunks[0],
    );

    let mut status_spans = Vec::new();
    match (&d.doctor, &d.doctor_error) {
        (Some(report), _) => {
            let failed = report.failed();
            if report.ok && failed.is_empty() {
                status_spans.push(Span::styled(
                    format!(
                        " ✓ doctor: {}/{} checks ok ",
                        report.checks.len(),
                        report.checks.len()
                    ),
                    GOOD,
                ));
            } else {
                let first = failed.first();
                status_spans.push(Span::styled(
                    format!(
                        " ✗ doctor: {} failed ({}: {}) ",
                        failed.len().max(1),
                        first.map(|c| c.name.as_str()).unwrap_or("?"),
                        first.map(|c| c.detail.as_str()).unwrap_or("")
                    ),
                    BAD,
                ));
            }
        }
        (None, Some(e)) => status_spans.push(Span::styled(format!(" doctor: {e} "), WARN)),
        (None, None) => status_spans.push(Span::styled(" doctor: polling… ", DIM)),
    }
    if let Some(e) = &d.peers_error {
        status_spans.push(Span::styled(format!(" peers: {e} "), BAD));
    }
    f.render_widget(
        Paragraph::new(Line::from(status_spans))
            .block(Block::default().borders(Borders::ALL).title(" status ")),
        chunks[1],
    );

    let header = Row::new(["NAME", "IP", "STATUS", "HANDSHAKE", "ENDPOINT", "RX", "TX"])
        .style(Style::default().fg(Color::DarkGray));
    let rows = d.peers.iter().map(|p| {
        let (icon, style) = match p.status {
            PeerStatus::Live => ("●", GOOD),
            PeerStatus::Stale => ("◐", WARN),
            PeerStatus::Dead => ("○", DIM),
        };
        Row::new(vec![
            Cell::from(p.name.clone()),
            Cell::from(p.ip.clone()),
            Cell::from(Span::styled(format!("{icon} {}", p.status.as_str()), style)),
            Cell::from(
                p.handshake
                    .map(format_age)
                    .unwrap_or_else(|| "never".into()),
            ),
            Cell::from(p.endpoint.clone().unwrap_or_else(|| "—".into())),
            Cell::from(format_rate(p.rx_rate)),
            Cell::from(format_rate(p.tx_rate)),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(22),
            Constraint::Length(14),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Min(20),
            Constraint::Length(12),
            Constraint::Length(12),
        ],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" peers — {} · refresh 3s ", d.peers.len())),
    )
    .header(header);
    f.render_widget(table, chunks[2]);
}
