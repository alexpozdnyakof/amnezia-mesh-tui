use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine};
use ratatui::widgets::{
    Axis, Block, Borders, Cell, Chart, Dataset, GraphType, List, ListItem, ListState, Paragraph,
    Row, Table, Wrap,
};
use ratatui::Frame;

use crate::app::{App, LogView, PeerGeo, ProbeState, Screen};
use crate::bootstrap::{BOOTSTRAP_DIR, BOOTSTRAP_REPO};
use crate::chart::{
    downsample, entries_end, presence_heatmap, traffic_series, HeatCell, TrafficPoint,
    BUCKET_MINUTES, HEATMAP_HOURS,
};
use crate::docker::ContainerInfo;
use crate::doctor::DoctorCheck;
use crate::geo::{centroid, fit_bounds, geo_label, GeoInfo};
use crate::logs::STATS_LOG_PATH;
use crate::peers::{
    format_age, format_bytes, format_rate, short_pubkey, PeerStatus, ResolvedPeer, REVOKE_AFTER,
};
use crate::profile;
use crate::wizard::{FieldKind, RunOutcome};

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
        Screen::Map => draw_map(f, app, chunks[1]),
        Screen::Log => draw_log(f, app, chunks[1]),
        Screen::Secret => draw_secret(f, app, chunks[1]),
        Screen::Wizard => draw_wizard(f, app, chunks[1]),
        Screen::Run => draw_run(f, app, chunks[1]),
    }
    draw_hints(f, app, chunks[2]);
}

fn draw_header(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let mut spans = vec![
        Span::styled(
            " awg-mesh-tui ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " {} · server: {} (ssh: {}) ",
            screen_label(app.screen),
            app.server_name,
            app.server.ssh_alias
        )),
    ];
    if app.read_only() {
        spans.push(Span::styled(
            " read-only ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn screen_label(screen: Screen) -> &'static str {
    match screen {
        Screen::Preflight => "preflight",
        Screen::Discovery => "discovery",
        Screen::Inspect => "inspect",
        Screen::Dashboard => "dashboard",
        Screen::Map => "map",
        Screen::Log => "logs",
        Screen::Secret => "key",
        Screen::Wizard => "setup",
        Screen::Run => "run",
    }
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
            text.push(Line::from(""));
            text.push(Line::from(Span::styled(
                "preview only — the container is unchanged. a opens the adopt form.",
                WARN,
            )));
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

fn draw_wizard(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let form = &app.wizard.form;
    let mut lines = Vec::new();
    if let crate::wizard::Kind::Adopt { container } = &form.kind {
        lines.push(Line::from(vec![
            Span::styled("container  ", DIM),
            Span::raw(container.clone()),
            Span::styled("   stopped during adopt, kept for rollback", DIM),
        ]));
        lines.push(Line::from(""));
    }
    for (i, field) in form.fields.iter().enumerate() {
        let marker = if i == form.selected { "> " } else { "  " };
        let value = match field.kind {
            FieldKind::Toggle => field.value.clone(),
            FieldKind::Text if form.editing && i == form.selected => format!("{}_", field.value),
            FieldKind::Text if field.value.is_empty() => "—".to_string(),
            FieldKind::Text => field.value.clone(),
        };
        let style = if i == form.selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(marker, GOOD),
            Span::styled(format!("{:<12}", field.label), DIM),
            Span::styled(value, style),
        ]));
    }
    if let Some(err) = &form.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(format!("error: {err}"), BAD)));
    }
    if form.confirm_pending {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(form.confirm_hint(), WARN)));
    }
    let body = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", form.kind.title())),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(body, area);
}

fn draw_run(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let run = &app.run;
    let (suffix, color) = match run.outcome {
        None if run.running => ("live", Color::Cyan),
        None => ("waiting", Color::DarkGray),
        Some(RunOutcome::Success) => ("up", Color::Green),
        Some(RunOutcome::RolledBack) => ("rolled back", Color::Yellow),
        Some(RunOutcome::Failed) => ("failed", Color::Red),
    };
    let banner = run.outcome.map(|outcome| {
        let (text, style) = match outcome {
            RunOutcome::Success => ("amnezia-mesh is up. Enter opens the dashboard.", GOOD),
            RunOutcome::RolledBack => (
                "Adopt rolled back. The previous container is running again; nothing was deleted.",
                WARN,
            ),
            RunOutcome::Failed => (
                "The CLI stopped with an error. Details are in the log.",
                BAD,
            ),
        };
        Paragraph::new(Span::styled(text, style)).block(Block::default().borders(Borders::ALL))
    });
    let chunks = if banner.is_some() {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(3)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3)])
            .split(area)
    };
    if let Some(banner) = banner {
        f.render_widget(banner, chunks[0]);
    }
    let log_area = *chunks.last().unwrap_or(&area);
    let height = log_area.height.saturating_sub(2) as usize;
    let end = (run.scroll + 1).min(run.lines.len());
    let start = end.saturating_sub(height.max(1));
    let body: Vec<Line> = run.lines[start..end]
        .iter()
        .map(|line| Line::from(line.as_str()))
        .collect();
    let log = Paragraph::new(body)
        .block(Block::default().borders(Borders::ALL).title(Span::styled(
            format!(" {} · {suffix} ", run.title),
            Style::default().fg(color),
        )))
        .wrap(Wrap { trim: false });
    f.render_widget(log, log_area);
}

fn draw_hints(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let hints: String = if app.screen == Screen::Log {
        let filter = app
            .log
            .filter
            .as_deref()
            .map(|fp| log_peer_name(app, fp))
            .unwrap_or_else(|| "all".to_string());
        format!(
            " ↑↓ PgUp/PgDn scroll · G bottom · f follow:{} · Tab filter:{} · v view:{} · Esc back ",
            if app.log.follow { "on" } else { "off" },
            filter,
            match app.log.view {
                LogView::List => "list",
                LogView::Chart => "chart",
                LogView::Heat => "heat",
            },
        )
    } else if let Some(input) = &app.dashboard.input {
        format!(
            " {}: {}_ · Enter confirm · Esc cancel ",
            input.prompt, input.value
        )
    } else if app.screen == Screen::Wizard && app.wizard.form.editing {
        " type · Enter done · Esc cancel edit ".to_string()
    } else if app.screen == Screen::Wizard && app.wizard.form.confirm_pending {
        " r run · Esc cancel ".to_string()
    } else if let Some(name) = &app.dashboard.confirm {
        format!(" revoke {name}? devices lose access · y confirm · any key cancel ")
    } else {
        let s: &str = match app.screen {
            Screen::Preflight => {
                if app.bootstrap.confirm_pending {
                    " B confirm bootstrap · Esc cancel "
                } else {
                    " q quit · r re-probe · d discover · i init · b bootstrap · g dashboard · l logs "
                }
            }
            Screen::Discovery => " ↑↓ select · Enter inspect · d re-scan · Esc back ",
            Screen::Inspect => {
                if app.read_only() {
                    " READ-ONLY · Esc back "
                } else if app.inspect.summary.is_some() {
                    " a adopt · Esc back "
                } else {
                    " Esc back "
                }
            }
            Screen::Wizard => {
                " ↑↓ select · Enter edit · Space toggle · h detect host · r run · Esc back "
            }
            Screen::Run => match app.run.outcome {
                Some(RunOutcome::Success) => " Enter dashboard · ↑↓ scroll · Esc back ",
                Some(_) => " ↑↓ scroll · Esc back ",
                None => " running — wait for the CLI ",
            },
            Screen::Dashboard => {
                if app.read_only() {
                    " READ-ONLY · ↑↓ select · l logs · m map · Esc back "
                } else {
                    " ↑↓ select · a add · r rename · x revoke · c 14d · l logs · m map · Esc back "
                }
            }
            Screen::Map => " Esc back · auto-refresh 3s · l logs ",
            Screen::Secret => " s save .vpn · p save QR · Esc close (secret cleared) ",
            Screen::Log => unreachable!(),
        };
        s.to_string()
    };
    let hints = Paragraph::new(hints).block(Block::default().borders(Borders::ALL).title(" keys "));
    f.render_widget(hints, area);
}

fn log_peer_name(app: &App, pubkey: &str) -> String {
    app.log
        .names
        .get(pubkey)
        .cloned()
        .unwrap_or_else(|| short_pubkey(pubkey))
}

/// "2026-10-26T20:38:42+07:00" → "10-26 20:38:42"
fn short_ts(at: &str) -> String {
    if at.len() >= 19 {
        format!("{} {}", &at[5..10], &at[11..19])
    } else {
        at.to_string()
    }
}

fn draw_log(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    match app.log.view {
        LogView::List => draw_log_list(f, app, area),
        LogView::Chart => draw_log_chart(f, app, area),
        LogView::Heat => draw_log_heat(f, app, area),
    }
}

fn draw_log_list(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let log = &app.log;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(area);

    let wd = match log.events.last() {
        None => Line::from(Span::styled("watchdog: no restart events".to_string(), DIM)),
        Some(ev) => Line::from(vec![
            Span::styled("watchdog: ", DIM),
            Span::styled(format!("{} × {}", ev.kind.as_str(), log.events.len()), WARN),
            Span::styled(
                format!("  last: {} {} ", short_ts(&ev.at), ev.container),
                DIM,
            ),
        ]),
    };
    f.render_widget(
        Paragraph::new(wd).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" availability "),
        ),
        chunks[0],
    );

    let filter_label = log.filter.as_deref().map(|fp| log_peer_name(app, fp));
    let mut items: Vec<ListItem> = Vec::new();
    if let Some(e) = &log.error {
        items.push(ListItem::new(Span::styled(format!("error: {e}"), BAD)));
    }
    for entry in log
        .entries
        .iter()
        .filter(|e| log.filter.as_deref().is_none_or(|fp| e.peer == fp))
    {
        let hs = entry.handshake.as_deref().unwrap_or("never");
        let hs_style = if entry.handshake.is_some() {
            Style::default()
        } else {
            DIM
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("{}  ", short_ts(&entry.at)), DIM),
            Span::styled(
                format!("{:<20} ", log_peer_name(app, &entry.peer)),
                Style::default(),
            ),
            Span::styled(
                format!("{:<22} ", entry.endpoint.as_deref().unwrap_or("—")),
                DIM,
            ),
            Span::styled(format!("{:<28} ", hs), hs_style),
            Span::styled(
                format!(
                    "↓{} ↑{}",
                    format_bytes(entry.rx.unwrap_or(0) as f64),
                    format_bytes(entry.tx.unwrap_or(0) as f64)
                ),
                GOOD,
            ),
        ])));
    }
    let selected = if log.follow {
        items.len().saturating_sub(1)
    } else {
        log.scroll.min(items.len().saturating_sub(1))
    };
    let title = format!(
        " stats log — {} entries · filter: {} · {} ",
        items.len(),
        filter_label.as_deref().unwrap_or("all"),
        STATS_LOG_PATH,
    );
    let is_empty = items.is_empty();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("> ");
    let mut state = ListState::default().with_selected((!is_empty).then_some(selected));
    f.render_stateful_widget(list, chunks[1], &mut state);
}

fn draw_log_chart(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let log = &app.log;
    let Some(fp) = log.filter.as_deref() else {
        let hint = Paragraph::new(Span::styled(
            " Tab — select a peer to chart (filter: all)".to_string(),
            WARN,
        ))
        .block(Block::default().borders(Borders::ALL).title(" rx/tx "));
        f.render_widget(hint, area);
        return;
    };
    let name = log_peer_name(app, fp);
    let series = traffic_series(&log.entries, fp);
    let points = downsample(&series, (area.width as usize).saturating_sub(8));
    if points.is_empty() {
        let hint = Paragraph::new(Span::styled(
            " no counter data for this peer yet".to_string(),
            DIM,
        ))
        .block(Block::default().borders(Borders::ALL).title(" rx/tx "));
        f.render_widget(hint, area);
        return;
    }
    let n = points.len();
    let max_y = points
        .iter()
        .fold(0.0_f64, |m, p| m.max(p.rx).max(p.tx))
        .max(1.0);
    let data_rx: Vec<(f64, f64)> = points
        .iter()
        .enumerate()
        .map(|(i, p)| (i as f64, p.rx))
        .collect();
    let data_tx: Vec<(f64, f64)> = points
        .iter()
        .enumerate()
        .map(|(i, p)| (i as f64, p.tx))
        .collect();
    let fmt_time = |p: &TrafficPoint| p.ts.format("%m-%d %H:%M").to_string();
    let x_labels = vec![
        fmt_time(&points[0]),
        fmt_time(&points[n / 2]),
        fmt_time(&points[n - 1]),
    ];
    let y_top = format_bytes(max_y);
    let chart = Chart::new(vec![
        Dataset::default()
            .name("rx")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(GOOD)
            .data(&data_rx),
        Dataset::default()
            .name("tx")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Cyan))
            .data(&data_tx),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" rx/tx — {name} · {n} samples ")),
    )
    .x_axis(
        Axis::default()
            .bounds([0.0, (n - 1).max(1) as f64])
            .labels(x_labels),
    )
    .y_axis(
        Axis::default()
            .bounds([0.0, max_y * 1.1])
            .labels(vec!["0".to_string(), y_top]),
    );
    f.render_widget(chart, area);
}

fn draw_log_heat(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let log = &app.log;
    let Some(now) = entries_end(&log.entries) else {
        let hint = Paragraph::new(Span::styled(" no log data yet".to_string(), DIM)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" presence · 24h "),
        );
        f.render_widget(hint, area);
        return;
    };
    let rows = presence_heatmap(&log.entries, now);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("window: ", DIM),
        Span::styled(
            format!(
                "{} → {} · buckets of {} min ",
                (now - chrono::Duration::hours(HEATMAP_HOURS)).format("%m-%d %H:%M"),
                now.format("%m-%d %H:%M"),
                BUCKET_MINUTES
            ),
            Style::default(),
        ),
        Span::styled("█ online · ", GOOD),
        Span::styled("· offline", DIM),
    ]));
    for row in &rows {
        let mut spans = vec![Span::styled(
            format!("{:<20} ", log_peer_name(app, &row.peer)),
            Style::default(),
        )];
        for cell in &row.cells {
            spans.push(match cell {
                HeatCell::Online => Span::styled("█", GOOD),
                HeatCell::Offline => Span::styled("·", DIM),
                HeatCell::NoData => Span::raw(" "),
            });
        }
        lines.push(Line::from(spans));
    }
    let body = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" presence · last 24h "),
    );
    f.render_widget(body, area);
}

fn draw_dashboard(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let d = &app.dashboard;
    let alerts = dashboard_alerts(
        d.peers_error.as_deref(),
        d.services_error.as_deref(),
        d.doctor_error.as_deref(),
    );
    let doctor_h = match &d.doctor {
        Some(report) if !report.failed().is_empty() => 5,
        Some(_) => 4,
        None => 3,
    };
    let revoke_selected = d
        .peers
        .get(d.selected.min(d.peers.len().saturating_sub(1)))
        .is_some_and(|p| p.revoke_candidate);
    let detail_h = 3 + u16::from(d.notice.is_some()) + u16::from(revoke_selected);
    let mut constraints = Vec::new();
    if !alerts.is_empty() {
        // One content row per alert, plus the border. A joined line hid
        // every error after the first on a normal terminal width.
        constraints.push(Constraint::Length(alerts.len() as u16 + 2));
    }
    constraints.extend([
        Constraint::Length(3),
        Constraint::Length(doctor_h),
        Constraint::Min(5),
        Constraint::Length(detail_h),
    ]);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let mut i = 0;
    if !alerts.is_empty() {
        let lines: Vec<Line> = alerts
            .iter()
            .cloned()
            .map(|text| Line::from(Span::styled(text, BAD)))
            .collect();
        f.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" error ")),
            chunks[i],
        );
        i += 1;
    }

    let services = services_line(&d.services, d.services_error.is_some());
    f.render_widget(
        Paragraph::new(services).block(Block::default().borders(Borders::ALL).title(" services ")),
        chunks[i],
    );
    i += 1;

    let doctor_lines = match &d.doctor {
        Some(report) => {
            let (top, bottom) = doctor_check_lines(&report.checks);
            let mut lines = vec![
                Line::from(check_spans(&top)),
                Line::from(check_spans(&bottom)),
            ];
            let failed = doctor_failures(&report.checks);
            if !failed.is_empty() {
                lines.push(Line::from(Span::styled(failed, BAD)));
            }
            lines
        }
        None => vec![Line::from(Span::styled(
            if d.doctor_error.is_some() {
                "unavailable — the error is in the banner"
            } else {
                "polling…"
            },
            if d.doctor_error.is_some() { WARN } else { DIM },
        ))],
    };
    let ok_n = d.doctor.as_ref().map(|r| r.checks.len()).unwrap_or(0);
    let bad_n = d.doctor.as_ref().map(|r| r.failed().len()).unwrap_or(0);
    // A stored report stays on screen when the next poll fails. The title
    // says so; the banner has the SSH error.
    let stale = if d.doctor.is_some() && d.doctor_error.is_some() {
        " · refresh failed"
    } else {
        ""
    };
    let doctor_title = match &d.doctor {
        Some(report) if bad_n == 0 && report.ok => format!(" doctor · {ok_n}/{ok_n}{stale} "),
        Some(_) if bad_n == 0 => format!(" doctor · not ok{stale} "),
        Some(_) => format!(" doctor · {bad_n} failed{stale} "),
        None => " doctor ".to_string(),
    };
    f.render_widget(
        Paragraph::new(doctor_lines)
            .block(Block::default().borders(Borders::ALL).title(doctor_title)),
        chunks[i],
    );
    i += 1;

    if d.peers.is_empty() {
        let msg = if d.peers_error.is_some() {
            "peer list unavailable — the error is in the banner"
        } else if app.read_only() {
            "no peers yet"
        } else {
            "no peers yet — a adds one"
        };
        f.render_widget(
            Paragraph::new(Span::styled(msg, DIM)).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" peers — 0 · refresh 3s "),
            ),
            chunks[i],
        );
    } else {
        let (live, stale, dead) = peer_counts(&d.peers);
        let revoke_n = d.peers.iter().filter(|p| p.revoke_candidate).count();
        let revoke_bit = if revoke_n == 0 {
            String::new()
        } else {
            format!(" · {revoke_n} revoke")
        };
        let header = Row::new(["NAME", "IP", "STATUS", "HANDSHAKE", "ENDPOINT", "RX", "TX"])
            .style(Style::default().fg(Color::DarkGray));
        let rows = d.peers.iter().map(|p| {
            let (label, style) = if p.revoke_candidate {
                ("✗ revoke".to_string(), BAD)
            } else {
                let (icon, style) = peer_mark(p.status);
                (format!("{icon} {}", p.status.as_str()), style)
            };
            Row::new(vec![
                Cell::from(p.name.clone()),
                Cell::from(p.ip.clone()),
                Cell::from(Span::styled(label, style)),
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
                Constraint::Length(18),
                Constraint::Length(16),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Min(8),
                Constraint::Length(12),
                Constraint::Length(12),
            ],
        )
        .block(Block::default().borders(Borders::ALL).title(format!(
            " peers — {} · {live} live · {stale} stale · {dead} dead{revoke_bit} ",
            d.peers.len()
        )))
        .header(header)
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
        let selected = Some(d.selected.min(d.peers.len() - 1));
        let mut tstate = ratatui::widgets::TableState::default().with_selected(selected);
        f.render_stateful_widget(table, chunks[i], &mut tstate);
    }
    i += 1;

    let mut detail = Vec::new();
    if let Some((msg, is_err)) = &d.notice {
        detail.push(Line::from(Span::styled(
            msg.clone(),
            if *is_err { BAD } else { GOOD },
        )));
    }
    let peer_line = d
        .peers
        .get(d.selected.min(d.peers.len().saturating_sub(1)))
        .map(selected_summary);
    detail.push(Line::from(match peer_line {
        Some(text) => Span::raw(text),
        None => Span::styled("no peer selected", DIM),
    }));
    if revoke_selected {
        let days = REVOKE_AFTER.as_secs() / 86400;
        detail.push(Line::from(Span::styled(
            format!("silent {days} days — c revokes"),
            WARN,
        )));
    }
    f.render_widget(
        Paragraph::new(detail).block(Block::default().borders(Borders::ALL).title(" selected ")),
        chunks[i],
    );
}

fn dashboard_alerts(
    peers_error: Option<&str>,
    services_error: Option<&str>,
    doctor_error: Option<&str>,
) -> Vec<String> {
    let mut alerts = Vec::new();
    if let Some(e) = peers_error {
        alerts.push(format!("peers: {e}"));
    }
    if let Some(e) = services_error {
        alerts.push(format!("services: {e}"));
    }
    if let Some(e) = doctor_error {
        alerts.push(format!("doctor: {e}"));
    }
    alerts
}

fn services_line(services: &[ContainerInfo], failed: bool) -> Line<'static> {
    if failed {
        return Line::from(Span::styled("unavailable", BAD));
    }
    if services.is_empty() {
        return Line::from(Span::styled(
            "no amnezia-mesh containers — init or adopt first",
            DIM,
        ));
    }
    let mut spans = Vec::new();
    for (i, c) in services.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        let (icon, style) = if c.running {
            ("●", GOOD)
        } else {
            ("○", WARN)
        };
        spans.push(Span::styled(
            format!("{icon} {}  {}", c.name, c.status),
            style,
        ));
    }
    Line::from(spans)
}

fn doctor_check_lines(checks: &[DoctorCheck]) -> (Vec<&DoctorCheck>, Vec<&DoctorCheck>) {
    if checks.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mid = checks.len().div_ceil(2);
    (
        checks[..mid].iter().collect(),
        checks[mid..].iter().collect(),
    )
}

fn check_spans(checks: &[&DoctorCheck]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, c) in checks.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let (mark, style) = if c.ok { ("✓", GOOD) } else { ("✗", BAD) };
        spans.push(Span::styled(format!("{mark} {}", c.name), style));
    }
    spans
}

fn doctor_failures(checks: &[DoctorCheck]) -> String {
    checks
        .iter()
        .filter(|c| !c.ok)
        .map(|c| format!("{}: {}", c.name, c.detail))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn peer_counts(peers: &[ResolvedPeer]) -> (usize, usize, usize) {
    let mut live = 0;
    let mut stale = 0;
    let mut dead = 0;
    for p in peers {
        match p.status {
            PeerStatus::Live => live += 1,
            PeerStatus::Stale => stale += 1,
            PeerStatus::Dead => dead += 1,
        }
    }
    (live, stale, dead)
}

fn peer_mark(status: PeerStatus) -> (&'static str, Style) {
    match status {
        PeerStatus::Live => ("●", GOOD),
        PeerStatus::Stale => ("◐", WARN),
        PeerStatus::Dead => ("○", DIM),
    }
}

/// MagicDNS name when the peer name is a real mesh name. Short-pubkey
/// fallbacks (`peer-` + the key's own characters) are not names.
fn magic_dns(name: &str) -> Option<String> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    ok.then(|| format!("{name}.awg"))
}

fn selected_summary(p: &ResolvedPeer) -> String {
    let who = match magic_dns(&p.name) {
        Some(dns) => format!("{}  {dns}", p.name),
        None => p.name.clone(),
    };
    let hs = p
        .handshake
        .map(format_age)
        .unwrap_or_else(|| "never".into());
    let ep = p.endpoint.clone().unwrap_or_else(|| "—".into());
    format!(
        "{who}  {}  {}  handshake {hs}  endpoint {ep}  rx {}  tx {}",
        p.ip,
        p.status.as_str(),
        format_rate(p.rx_rate),
        format_rate(p.tx_rate)
    )
}

fn map_status_color(status: PeerStatus) -> Color {
    match status {
        PeerStatus::Live => Color::LightGreen,
        PeerStatus::Stale => Color::Green,
        PeerStatus::Dead => Color::DarkGray,
    }
}

fn draw_map(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(30), Constraint::Length(46)])
        .split(area);

    let peers_with_geo: Vec<(&PeerGeo, &GeoInfo)> = app
        .dashboard
        .geo
        .iter()
        .filter_map(|g| g.geo.as_ref().map(|i| (g, i)))
        .collect();
    let server_coord = app.server.server_lat.zip(app.server.server_lon);
    let known: Vec<(f64, f64)> = peers_with_geo.iter().map(|(_, i)| (i.lat, i.lon)).collect();
    let hub = server_coord.or_else(|| centroid(&known));

    // side panel: hub info, legend, per-peer geo table (fallback view)
    let mut lines: Vec<Line> = Vec::new();
    match &hub {
        Some((lat, lon)) => lines.push(Line::from(vec![
            Span::styled("hub: ", DIM),
            Span::styled(format!("{lat:.2}°, {lon:.2}°"), GOOD),
            Span::styled(
                if server_coord.is_some() {
                    " (configured)"
                } else {
                    " (centroid of peers)"
                },
                DIM,
            ),
        ])),
        None => lines.push(Line::from(Span::styled(
            "hub: no coordinates yet".to_string(),
            DIM,
        ))),
    }
    if let Some(e) = &app.geo_error {
        lines.push(Line::from(Span::styled(format!("geoip: {e}"), WARN)));
    }
    lines.push(Line::from(vec![
        Span::styled("●", Style::default().fg(Color::LightGreen)),
        Span::styled(" live  ", DIM),
        Span::styled("●", Style::default().fg(Color::Green)),
        Span::styled(" stale  ", DIM),
        Span::styled("●", Style::default().fg(Color::DarkGray)),
        Span::styled(" dead ", DIM),
        Span::styled("◉ hub", GOOD),
    ]));
    lines.push(Line::from(""));
    for g in &app.dashboard.geo {
        let label = g
            .geo
            .as_ref()
            .map(geo_label)
            .unwrap_or_else(|| "no coords".to_string());
        lines.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(map_status_color(g.status))),
            Span::styled(format!("{:<18} ", g.name), Style::default()),
            Span::styled(format!("{} ", g.endpoint_ip), DIM),
            Span::styled(label, DIM),
        ]));
    }
    let panel = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" connections "),
    );
    f.render_widget(panel, chunks[1]);

    // canvas map
    if app.geo_db.is_none() {
        let hint = Paragraph::new(Span::styled(
            " geoip db not loaded — put GeoLite2-City.mmdb (or IP2Location \
             LITE) into the path from the panel, or set mmdb_path in config"
                .to_string(),
            WARN,
        ))
        .block(Block::default().borders(Borders::ALL).title(" map "));
        f.render_widget(hint, chunks[0]);
        return;
    }
    if hub.is_none() || peers_with_geo.is_empty() {
        let hint = Paragraph::new(Span::styled(
            " no peers with known coordinates yet".to_string(),
            DIM,
        ))
        .block(Block::default().borders(Borders::ALL).title(" map "));
        f.render_widget(hint, chunks[0]);
        return;
    }
    let hub = hub.expect("checked above");
    let mut all = known.clone();
    all.push(hub);
    let ((lat_min, lat_max), (lon_min, lon_max)) = fit_bounds(&all).expect("non-empty");
    let canvas = Canvas::default()
        .marker(symbols::Marker::Braille)
        .x_bounds([lon_min, lon_max])
        .y_bounds([lat_min, lat_max])
        .paint(move |ctx| {
            // 10° grid for orientation (equirectangular, distortion at
            // high latitudes is acceptable for an admin overview)
            let mut lon = (lon_min / 10.0).ceil() * 10.0;
            while lon <= lon_max {
                ctx.draw(&CanvasLine {
                    x1: lon,
                    y1: lat_min,
                    x2: lon,
                    y2: lat_max,
                    color: Color::Rgb(30, 40, 30),
                });
                lon += 10.0;
            }
            let mut lat = (lat_min / 10.0).ceil() * 10.0;
            while lat <= lat_max {
                ctx.draw(&CanvasLine {
                    x1: lon_min,
                    y1: lat,
                    x2: lon_max,
                    y2: lat,
                    color: Color::Rgb(30, 40, 30),
                });
                lat += 10.0;
            }
            for (_, info) in &peers_with_geo {
                ctx.draw(&CanvasLine {
                    x1: hub.1,
                    y1: hub.0,
                    x2: info.lon,
                    y2: info.lat,
                    color: Color::DarkGray,
                });
            }
            for (g, info) in &peers_with_geo {
                ctx.print(
                    info.lon,
                    info.lat,
                    Span::styled("●", Style::default().fg(map_status_color(g.status))),
                );
            }
            ctx.print(
                hub.1,
                hub.0,
                Span::styled(
                    "◉",
                    Style::default()
                        .fg(Color::LightGreen)
                        .add_modifier(Modifier::BOLD),
                ),
            );
        })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" connection map · hub-and-spoke "),
        );
    f.render_widget(canvas, chunks[0]);
}

fn draw_secret(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let Some(s) = &app.secret else { return };
    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::styled("peer: ", DIM),
        Span::styled(s.name.clone(), GOOD),
        Span::styled("   ip: ", DIM),
        Span::raw(s.ip.clone()),
    ])];
    match &s.view {
        Some(view) => {
            let who = if view.description.is_empty() {
                view.host.clone()
            } else {
                view.description.clone()
            };
            lines.push(Line::from(Span::styled(
                who,
                Style::default().add_modifier(Modifier::BOLD),
            )));
            let host = if view.port.is_empty() {
                view.host.clone()
            } else {
                format!("{}:{}", view.host, view.port)
            };
            lines.push(Line::from(vec![
                Span::styled("host ", DIM),
                Span::raw(host),
                Span::styled("   client ", DIM),
                Span::raw(view.client_ip.clone()),
                Span::styled("   dns ", DIM),
                Span::raw(view.dns.clone()),
            ]));
        }
        None => lines.push(Line::from(Span::styled(
            "could not read a vpn:// summary",
            WARN,
        ))),
    }

    let inner_w = area.width.saturating_sub(2);
    let inner_h = area.height.saturating_sub(2);
    let qr_rows = inner_h.saturating_sub(lines.len() as u16 + 4);
    let url = profile::find_vpn_url(&s.profile);
    let qr = url.and_then(|u| profile::terminal_qr(u, inner_w, qr_rows));
    if let Some(qr) = &qr {
        lines.push(Line::from(""));
        for row in qr.lines() {
            lines.push(Line::from(row.to_string()));
        }
    } else if url.is_some() {
        lines.push(Line::from(Span::styled(
            "terminal QR is too dense for this pane — p saves a scannable PNG",
            WARN,
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " .vpn profile — import in the Amnezia app:",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    let budget = (inner_h as usize).saturating_sub(lines.len());
    for l in s.profile.lines().take(budget) {
        lines.push(Line::from(Span::styled(l.to_string(), GOOD)));
    }
    if let Some(p) = &s.saved_to {
        lines.push(Line::from(Span::styled(format!("saved: {p}"), GOOD)));
    }
    if let Some(p) = &s.qr_saved {
        lines.push(Line::from(Span::styled(format!("qr: {p}"), GOOD)));
    }
    if let Some(err) = &s.qr_error {
        lines.push(Line::from(Span::styled(err.clone(), BAD)));
    }
    let body = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" new peer — shown once · {} ", s.name)),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(body, area);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::{Config, ServerConfig};
    use crate::doctor::DoctorReport;

    fn check(name: &str, ok: bool, detail: &str) -> DoctorCheck {
        DoctorCheck {
            name: name.into(),
            ok,
            detail: detail.into(),
        }
    }

    fn eight_checks() -> Vec<DoctorCheck> {
        vec![
            check("docker-daemon", true, "running"),
            check("initialized", true, "state found"),
            check("awg-container", true, "up"),
            check("dns-container", true, "up"),
            check("listen-port", true, "51820"),
            check("ip-forward", true, "on"),
            check("magicdns", true, "resolving"),
            check("port-published", false, "udp port not published"),
        ]
    }

    fn peer(name: &str, status: PeerStatus) -> ResolvedPeer {
        ResolvedPeer {
            name: name.into(),
            ip: "100.64.0.2".into(),
            pubkey: "abc".into(),
            status,
            handshake: Some(Duration::from_secs(90)),
            endpoint: Some("203.0.113.5:51820".into()),
            rx_rate: 512.0,
            tx_rate: 1536.0,
            revoke_candidate: false,
        }
    }

    fn container(name: &str, running: bool) -> ContainerInfo {
        ContainerInfo {
            id: "abc123".into(),
            name: name.into(),
            image: "amnezia-awg".into(),
            status: if running {
                "Up 2 hours".into()
            } else {
                "Exited".into()
            },
            running,
            candidate: false,
        }
    }

    fn line_text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    fn test_app() -> App {
        let server = ServerConfig {
            ssh_alias: "vps".into(),
            cli_path: "awg-mesh".into(),
            container: "amnezia-mesh-awg".into(),
            login_shell: "sh".into(),
            read_only: Some(true),
            ..ServerConfig::default()
        };
        let config = Config {
            active_server: "vps".into(),
            servers: std::collections::BTreeMap::new(),
            post_mutation_hook: None,
        };
        let mut app = App::new(
            "vps".into(),
            server,
            config,
            std::path::PathBuf::from("/tmp/awg-mesh-tui-test"),
        );
        app.screen = Screen::Dashboard;
        app
    }

    fn frame_text(app: &App) -> String {
        let backend = ratatui::backend::TestBackend::new(120, 36);
        let mut term = ratatui::Terminal::new(backend).expect("terminal");
        term.draw(|f| draw(f, app)).expect("draw");
        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn doctor_lines_keep_every_check_and_name_failures() {
        let checks = eight_checks();
        let (top, bottom) = doctor_check_lines(&checks);
        assert_eq!(top.len(), 4);
        assert_eq!(bottom.len(), 4);
        let shown: Vec<&str> = top
            .iter()
            .chain(bottom.iter())
            .map(|c| c.name.as_str())
            .collect();
        let expected: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(shown, expected);
        assert!(top.iter().all(|c| c.ok));
        assert!(!bottom.last().unwrap().ok);

        let (one, rest) = doctor_check_lines(&checks[..1]);
        assert_eq!(one.len(), 1);
        assert!(rest.is_empty());
        let (empty_a, empty_b) = doctor_check_lines(&[]);
        assert!(empty_a.is_empty() && empty_b.is_empty());

        assert_eq!(
            doctor_failures(&checks),
            "port-published: udp port not published"
        );
        assert!(doctor_failures(&checks[..7]).is_empty());
    }

    #[test]
    fn alerts_list_peers_then_services_then_doctor() {
        assert!(dashboard_alerts(None, None, None).is_empty());
        assert_eq!(
            dashboard_alerts(Some("ssh timed out"), None, Some("reset")),
            vec![
                "peers: ssh timed out".to_string(),
                "doctor: reset".to_string()
            ]
        );
        assert_eq!(
            dashboard_alerts(Some("a"), Some("b"), Some("c")),
            vec![
                "peers: a".to_string(),
                "services: b".to_string(),
                "doctor: c".to_string()
            ]
        );
    }

    #[test]
    fn magic_dns_matches_mesh_names_only() {
        assert_eq!(magic_dns("phone"), Some("phone.awg".into()));
        assert_eq!(magic_dns("phone-2"), Some("phone-2.awg".into()));
        assert_eq!(
            magic_dns(&"n".repeat(32)),
            Some(format!("{}.awg", "n".repeat(32)))
        );
        assert_eq!(magic_dns(""), None);
        assert_eq!(magic_dns(&"n".repeat(33)), None);
        assert_eq!(magic_dns("Phone"), None);
        assert_eq!(magic_dns("peer-AbC+def1"), None);
        assert_eq!(magic_dns("peer-abc/def="), None);
    }

    #[test]
    fn peer_counts_and_selected_summary() {
        let peers = vec![
            peer("phone", PeerStatus::Live),
            peer("laptop", PeerStatus::Stale),
            peer("old", PeerStatus::Dead),
            peer("older", PeerStatus::Dead),
        ];
        assert_eq!(peer_counts(&peers), (1, 1, 2));
        let summary = selected_summary(&peers[0]);
        assert_eq!(
            summary,
            "phone  phone.awg  100.64.0.2  live  handshake 1m 30s  endpoint 203.0.113.5:51820  rx 512 B/s  tx 1.5 KiB/s"
        );
        let mut unnamed = peers[0].clone();
        unnamed.name = "peer-AbC+def1".into();
        unnamed.status = PeerStatus::Dead;
        unnamed.handshake = None;
        unnamed.endpoint = None;
        assert_eq!(
            selected_summary(&unnamed),
            "peer-AbC+def1  100.64.0.2  dead  handshake never  endpoint —  rx 512 B/s  tx 1.5 KiB/s"
        );
    }

    #[test]
    fn services_line_states() {
        assert_eq!(
            line_text(&services_line(&[], false)),
            "no amnezia-mesh containers — init or adopt first"
        );
        assert_eq!(line_text(&services_line(&[], true)), "unavailable");
        let line = services_line(
            &[
                container("amnezia-mesh-awg", true),
                container("amnezia-mesh-dns", false),
            ],
            false,
        );
        assert_eq!(
            line_text(&line),
            "● amnezia-mesh-awg  Up 2 hours   ○ amnezia-mesh-dns  Exited"
        );
    }

    #[test]
    fn dashboard_frame_shows_checks_peers_and_ssh_banner() {
        let mut app = test_app();
        app.dashboard.services = vec![
            container("amnezia-mesh-awg", true),
            container("amnezia-mesh-dns", true),
        ];
        app.dashboard.doctor = Some(DoctorReport {
            ok: false,
            checks: eight_checks(),
        });
        app.dashboard.doctor_error = Some("connection reset by peer".into());
        app.dashboard.peers_error = Some("ssh: connect to host vps port 22: timed out".into());
        app.dashboard.peers = vec![
            peer("phone", PeerStatus::Live),
            peer("laptop", PeerStatus::Stale),
        ];
        app.dashboard.selected = 0;
        app.dashboard.notice = Some(("renamed tablet → phone".into(), false));

        let view = frame_text(&app);
        assert!(view.contains("read-only"), "{view}");
        assert!(
            view.contains("doctor · 1 failed · refresh failed"),
            "{view}"
        );
        for name in [
            "docker-daemon",
            "initialized",
            "awg-container",
            "dns-container",
            "listen-port",
            "ip-forward",
            "magicdns",
            "port-published",
        ] {
            assert!(view.contains(name), "missing {name}\n{view}");
        }
        assert!(
            view.contains("port-published: udp port not published"),
            "{view}"
        );
        assert!(
            view.contains("peers: ssh: connect to host vps port 22: timed out"),
            "{view}"
        );
        assert!(view.contains("doctor: connection reset by peer"), "{view}");
        assert!(view.contains("amnezia-mesh-awg"), "{view}");
        assert!(view.contains("amnezia-mesh-dns"), "{view}");
        assert!(view.contains("phone"), "{view}");
        assert!(view.contains("100.64.0.2"), "{view}");
        assert!(view.contains("203.0.113.5:51820"), "{view}");
        assert!(view.contains("phone.awg"), "{view}");
        assert!(view.contains("renamed tablet → phone"), "{view}");
        assert!(view.contains("1 live · 1 stale · 0 dead"), "{view}");
    }

    #[test]
    fn dashboard_frame_empty_and_doctor_down() {
        let mut app = test_app();
        app.server.read_only = Some(false);
        app.dashboard.doctor_error = Some("ssh: Connection refused".into());
        let view = frame_text(&app);
        assert!(!view.contains("read-only"), "{view}");
        assert!(view.contains("doctor: ssh: Connection refused"), "{view}");
        assert!(
            view.contains("unavailable — the error is in the banner"),
            "{view}"
        );
        assert!(
            view.contains("no amnezia-mesh containers — init or adopt first"),
            "{view}"
        );
        assert!(view.contains("no peers yet — a adds one"), "{view}");
        assert!(view.contains("no peer selected"), "{view}");
    }

    #[test]
    fn dashboard_marks_a_fourteen_day_peer_for_revoke() {
        let mut app = test_app();
        app.server.read_only = Some(false);
        let mut old = peer("tablet", PeerStatus::Dead);
        old.revoke_candidate = true;
        old.handshake = Some(Duration::from_secs(14 * 86400));
        app.dashboard.peers = vec![old];
        app.dashboard.selected = 0;
        let view = frame_text(&app);
        assert!(view.contains("✗ revoke"), "{view}");
        assert!(view.contains("1 revoke"), "{view}");
        assert!(view.contains("silent 14 days — c revokes"), "{view}");
        assert!(view.contains("c 14d"), "{view}");
    }

    #[test]
    fn secret_screen_shows_the_public_summary() {
        let value = serde_json::json!({
            "description": "192.0.2.10 (laptop exit)",
            "hostName": "192.0.2.10",
            "dns1": "100.64.0.1",
            "containers": [{
                "awg": {
                    "port": "443",
                    "last_config": "{\"client_ip\":\"100.64.0.7\",\"client_priv_key\":\"SECRETKEY\"}"
                }
            }]
        });
        let url = profile::encode_profile(&value).unwrap();
        let mut app = test_app();
        app.screen = Screen::Secret;
        app.secret = Some(crate::app::SecretState::issued(
            "laptop".into(),
            "100.64.0.7".into(),
            url,
        ));
        let view = frame_text(&app);
        assert!(view.contains("192.0.2.10 (laptop exit)"), "{view}");
        assert!(view.contains("192.0.2.10:443"), "{view}");
        assert!(view.contains("100.64.0.7"), "{view}");
        assert!(view.contains("100.64.0.1"), "{view}");
        assert!(!view.contains("SECRETKEY"), "{view}");
        assert!(view.contains("p save QR"), "{view}");
    }
}
