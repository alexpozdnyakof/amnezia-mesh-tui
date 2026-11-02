mod app;
mod bootstrap;
mod chart;
mod config;
mod docker;
mod doctor;
mod geo;
mod inspect;
mod install;
mod logs;
mod mutate;
mod peers;
mod probe;
mod profile;
mod show;
mod stats;
mod transport;
mod ui;
mod watchdog;
mod wizard;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::Mutex;

use app::{App, Job, Screen};
use config::{Config, ServerConfig};
use transport::Transport;
use wizard::RunOutcome;

#[derive(Parser)]
#[command(
    name = "awg-mesh-tui",
    about = "TUI admin panel and SSH installer for amnezia-mesh"
)]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Configured server name (see `servers` in the config file).
    #[arg(long)]
    server: Option<String>,

    /// Config file path (default: ~/.config/awg-mesh-tui/config.toml).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Run the preflight probe once, print the result, exit (no TUI).
    #[arg(long)]
    probe_once: bool,

    /// Probe, list docker containers once, print, exit (no TUI).
    #[arg(long)]
    discover_once: bool,

    /// Run `adopt --inspect` on a container once, print, exit (no TUI).
    #[arg(long, value_name = "CONTAINER")]
    inspect_once: Option<String>,

    /// Print the live peer table once (awg show), exit (no TUI).
    #[arg(long)]
    peers_once: bool,

    /// Use a canned in-process transport (TUI development without a server).
    #[arg(long, hide = true)]
    mock: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Copy the bundled Linux awg-mesh CLI onto a server over SSH.
    Install(InstallArgs),
}

#[derive(Parser, Debug)]
struct InstallArgs {
    /// SSH alias or user@host. Omit on a terminal to be asked.
    #[arg(long)]
    ssh: Option<String>,

    /// Private key passed to `ssh -i`. Omit to use the agent and ~/.ssh/config.
    #[arg(long)]
    identity: Option<PathBuf>,

    /// Directory that contains `linux_amd64/` and `linux_arm64/` payloads.
    #[arg(long)]
    payload_dir: Option<PathBuf>,

    /// Do not write the server into the panel config.
    #[arg(long)]
    no_save: bool,

    /// Config file to update (default: ~/.config/awg-mesh-tui/config.toml).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Print one JSON object on stdout and do not prompt.
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(Cmd::Install(install)) = args.cmd {
        return install_cmd(install).await;
    }
    let cfg_path = args.config.clone().unwrap_or_else(config::default_path);
    let cfg = Config::load(&cfg_path)
        .with_context(|| format!("cannot load config {}", cfg_path.display()))?;

    let name = args
        .server
        .clone()
        .or_else(|| (!cfg.active_server.is_empty()).then(|| cfg.active_server.clone()))
        .unwrap_or_default();
    let server = cfg.servers.get(&name).cloned();
    let server = match server {
        Some(s) => s,
        None => bail!(
            "no server '{}' in {}\n\n\
             copy the CLI onto a VPS first:\n\
             awg-mesh-tui install --ssh user@host\n\n\
             or add an entry:\n\
             active_server = \"prod\"\n\n\
             [servers.prod]\n\
             ssh_alias = \"my-vps\"\n",
            name,
            cfg_path.display()
        ),
    };

    let transport = if args.mock {
        transport::mock_canned()
    } else {
        Transport::for_server(&server)
    };

    if args.probe_once {
        return probe_once(&name, &server, &transport).await;
    }
    if args.discover_once {
        return discover_once(&name, &server, &transport).await;
    }
    if let Some(container) = &args.inspect_once {
        return inspect_once(container, &server, &transport).await;
    }
    if args.peers_once {
        return peers_once(&server, &transport).await;
    }

    run_tui(&name, server, cfg, cfg_path, transport).await
}

async fn install_cmd(args: InstallArgs) -> Result<()> {
    let ssh = match args.ssh {
        Some(s) => s,
        None if args.json => {
            return install_fail(true, "--ssh is required with --json");
        }
        None if !std::io::IsTerminal::is_terminal(&std::io::stdin()) => {
            return install_fail(false, "pass --ssh (there is no terminal to ask)");
        }
        None => match install::prompt_ssh_target() {
            Ok(s) => s,
            Err(e) => return install_fail(false, &e),
        },
    };
    let mut opts = install::InstallOpts::new(ssh.clone());
    opts.identity = args.identity;
    opts.payload_dir = args
        .payload_dir
        .or_else(|| std::env::var_os("AWG_MESH_PAYLOAD_DIR").map(PathBuf::from));
    let outcome = match install::install_server(&opts).await {
        Ok(o) => o,
        Err(e) => return install_fail(args.json, &e),
    };

    let mut config_path = None;
    if !args.no_save {
        let path = args.config.clone().unwrap_or_else(config::default_path);
        let mut cfg = match Config::load(&path) {
            Ok(c) => c,
            Err(e) => return install_fail(args.json, &format!("config: {e}")),
        };
        let name = install::server_name(&ssh);
        let mut server = cfg.servers.get(&name).cloned().unwrap_or_default();
        server.ssh_alias = ssh.clone();
        server.cli_path = outcome.remote_path.clone();
        cfg.servers.insert(name.clone(), server);
        if cfg.active_server.is_empty() {
            cfg.active_server = name.clone();
        }
        if let Err(e) = cfg.save(&path) {
            return install_fail(args.json, &format!("config: {e}"));
        }
        config_path = Some(path.display().to_string());
    }

    if args.json {
        let mut report = serde_json::json!({
            "ok": true,
            "ssh": ssh,
            "arch": outcome.arch,
            "remote_path": outcome.remote_path,
            "version": outcome.version,
            "server_name": install::server_name(&ssh),
        });
        if let Some(path) = config_path {
            report["config"] = serde_json::Value::String(path);
        }
        println!("{report}");
        return Ok(());
    }
    println!(
        "copied {ssh}:{}\n{}\nnext: awg-mesh-tui    (i = init, a = adopt)",
        outcome.remote_path, outcome.version
    );
    if let Some(path) = config_path {
        println!("saved {path}");
    }
    Ok(())
}

fn install_fail(json: bool, err: &str) -> Result<()> {
    if json {
        println!("{}", serde_json::json!({"ok": false, "error": err}));
        std::process::exit(1);
    }
    bail!("{err}")
}

async fn probe_once(name: &str, server: &ServerConfig, transport: &Transport) -> Result<()> {
    let result = probe::probe(transport, server).await;
    println!("server: {name} (ssh: {})", server.ssh_alias);
    match &result.docker_path {
        Some(p) => println!("docker: {p} ({:?})", result.docker_version),
        None => println!("docker: NOT FOUND"),
    }
    match &result.cli_version {
        Some(v) => println!("awg-mesh CLI: {v}"),
        None => println!("awg-mesh CLI: NOT FOUND ({})", server.cli_path),
    }
    for note in &result.notes {
        println!("note: {note}");
    }
    Ok(())
}

async fn discover_once(name: &str, server: &ServerConfig, transport: &Transport) -> Result<()> {
    let probe = probe::probe(transport, server).await;
    let docker = match probe.docker_path {
        Some(d) => d,
        None => bail!("docker not found on server '{name}'"),
    };
    let containers = docker::discover(transport, &docker)
        .await
        .map_err(anyhow::Error::msg)?;
    println!("server: {name} — {} container(s)", containers.len());
    for c in &containers {
        println!(
            "{} {:<24} {:<40} {}{}",
            if c.candidate { "◆" } else { "·" },
            c.name,
            c.image,
            c.status,
            if c.candidate {
                "  [migration candidate]"
            } else {
                ""
            },
        );
    }
    Ok(())
}

async fn inspect_once(container: &str, server: &ServerConfig, transport: &Transport) -> Result<()> {
    match inspect::inspect(transport, server, container).await {
        Ok(s) => {
            println!("container: {container}");
            println!("source: {}", s.source);
            println!("image: {}", s.image);
            println!(
                "subnet: {}  mtu: {}  listen: {}  keepalive: {}  obfuscation params: {}",
                s.subnet, s.mtu, s.listen_port, s.server_keepalive, s.obfuscation_params
            );
            println!("server pubkey: {}", s.server_pub);
            println!("peers ({}):", s.peers.len());
            for p in &s.peers {
                println!(
                    "  {}  {:<18}  {}",
                    p.name,
                    p.ip,
                    p.created.as_deref().unwrap_or("—")
                );
            }
            for w in &s.warnings {
                println!("warning: {w}");
            }
            Ok(())
        }
        Err(inspect::InspectError::CliMissing) => bail!(
            "awg-mesh CLI not found on the server (cli_path: {}) — bootstrap it first",
            server.cli_path
        ),
        Err(e) => Err(anyhow::Error::new(e)),
    }
}

async fn peers_once(server: &ServerConfig, transport: &Transport) -> Result<()> {
    let probe = probe::probe(transport, server).await;
    let docker = match probe.docker_path {
        Some(d) => d,
        None => bail!("docker not found on server"),
    };
    let container = &server.container;
    let out = transport
        .exec(&format!("{docker} exec {container} awg show"))
        .await
        .map_err(anyhow::Error::msg)?;
    if !out.ok() {
        bail!("awg show failed in '{container}': {}", out.stderr.trim());
    }
    let info = show::parse_show(&out.stdout).map_err(anyhow::Error::msg)?;
    let names_pub = peers::fetch_names(transport, &server.cli_path).await;
    let mut samples = std::collections::HashMap::new();
    let rows = peers::resolve_peers(
        &info.peers,
        &names_pub,
        &std::collections::HashMap::new(),
        &mut samples,
        std::time::Instant::now(),
    );
    println!("container: {container} ({} peers)", rows.len());
    println!(
        "{:<24} {:<16} {:<8} {:<12} {:<24} {:>12} {:>12}",
        "NAME", "IP", "STATUS", "HANDSHAKE", "ENDPOINT", "RX", "TX"
    );
    for p in &rows {
        println!(
            "{:<24} {:<16} {:<8} {:<12} {:<24} {:>12} {:>12}",
            p.name,
            p.ip,
            p.status.as_str(),
            p.handshake
                .map(peers::format_age)
                .unwrap_or_else(|| "never".into()),
            p.endpoint.clone().unwrap_or_else(|| "—".into()),
            peers::format_rate(p.rx_rate),
            peers::format_rate(p.tx_rate),
        );
    }
    Ok(())
}

async fn run_tui(
    name: &str,
    server: ServerConfig,
    cfg: Config,
    cfg_path: PathBuf,
    transport: Transport,
) -> Result<()> {
    let app = Arc::new(Mutex::new(App::new(
        name.to_string(),
        server,
        cfg,
        cfg_path,
    )));

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, app.clone(), transport).await;

    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    );
    let _ = terminal.show_cursor();

    result
}

enum Event {
    Key(KeyEvent),
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: Arc<Mutex<App>>,
    transport: Transport,
) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || loop {
        match crossterm::event::poll(std::time::Duration::from_millis(100)) {
            Ok(true) => match crossterm::event::read() {
                Ok(crossterm::event::Event::Key(key)) => {
                    if tx.send(Event::Key(key)).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            },
            Ok(false) => {}
            Err(_) => break,
        }
    });

    let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
    loop {
        {
            let state = app.lock().await;
            terminal.draw(|f| ui::draw(f, &state))?;
        }
        tokio::select! {
            _ = tick.tick() => {
                App::pump(&app, &transport).await;
            }
            Some(event) = rx.recv() => match event {
                Event::Key(key) => {
                    if handle_key(&app, key).await {
                        return Ok(());
                    }
                }
            },
        }
    }
}

/// Returns true when the user asked to quit.
async fn handle_key(app: &Arc<Mutex<App>>, key: KeyEvent) -> bool {
    let mut a = app.lock().await;
    // Raw mode swallows the terminal SIGINT, including during a long init/adopt.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return true;
    }
    let typing =
        a.dashboard.input.is_some() || (a.screen == Screen::Wizard && a.wizard.form.editing);
    // A running init/adopt must finish; killing the TUI drops the ssh session.
    let busy = a.screen == Screen::Run && a.run.running;
    if key.code == KeyCode::Char('q') && !typing && !busy {
        return true;
    }
    if busy {
        return false;
    }
    match a.screen {
        Screen::Preflight => match key.code {
            KeyCode::Char('r') => a.request(Job::Probe),
            KeyCode::Char('d') => a.request(Job::Discover),
            KeyCode::Char('b') => {
                if a.bootstrap.confirm_pending {
                    a.bootstrap.confirm_pending = false;
                    a.request(Job::Bootstrap);
                } else {
                    a.bootstrap.confirm_pending = true;
                }
            }
            KeyCode::Char('g') => a.enter_dashboard(),
            KeyCode::Char('i') => a.open_init_wizard(),
            KeyCode::Char('l') => a.enter_log(Screen::Preflight),
            KeyCode::Esc => a.bootstrap.confirm_pending = false,
            _ => {}
        },
        Screen::Discovery => {
            let len = a.discovery.containers.len();
            match key.code {
                KeyCode::Up => {
                    a.discovery.selected = a.discovery.selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    if len > 0 {
                        a.discovery.selected = (a.discovery.selected + 1).min(len - 1);
                    }
                }
                KeyCode::Enter => {
                    let container = a
                        .discovery
                        .containers
                        .get(a.discovery.selected)
                        .map(|c| c.name.clone());
                    if let Some(name) = container {
                        a.request(Job::Inspect(name));
                    }
                }
                KeyCode::Char('d') => a.request(Job::Discover),
                KeyCode::Esc => a.screen = Screen::Preflight,
                _ => {}
            }
        }
        Screen::Inspect => match key.code {
            KeyCode::Esc => a.screen = Screen::Discovery,
            KeyCode::Char('a') => a.open_adopt_wizard(),
            _ => {}
        },
        Screen::Wizard => {
            if a.wizard.form.editing {
                match key.code {
                    KeyCode::Enter | KeyCode::Esc => a.wizard.form.editing = false,
                    KeyCode::Backspace => a.wizard.form.backspace(),
                    KeyCode::Char(c) => a.wizard.form.push_char(c),
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Up => a.wizard.form.move_sel(-1),
                    KeyCode::Down => a.wizard.form.move_sel(1),
                    KeyCode::Enter | KeyCode::Char(' ') => a.wizard.form.activate(),
                    KeyCode::Char('r') => a.arm_provision(),
                    KeyCode::Char('h') => {
                        a.wizard.form.detect_force = true;
                        a.request(Job::DetectHost);
                    }
                    KeyCode::Esc => {
                        if a.wizard.form.confirm_pending {
                            a.wizard.form.confirm_pending = false;
                        } else {
                            a.screen = a.wizard.return_to;
                        }
                    }
                    _ => {}
                }
            }
        }
        Screen::Run => match key.code {
            KeyCode::Esc => a.screen = a.run.return_to,
            KeyCode::Enter if a.run.outcome == Some(RunOutcome::Success) => a.enter_dashboard(),
            KeyCode::Up => a.scroll_run(-1),
            KeyCode::Down => a.scroll_run(1),
            KeyCode::PageUp => a.scroll_run(-10),
            KeyCode::PageDown => a.scroll_run(10),
            KeyCode::End | KeyCode::Char('G') => {
                a.run.follow = true;
                a.run.scroll = a.run.lines.len().saturating_sub(1);
            }
            _ => {}
        },
        Screen::Dashboard => {
            if a.dashboard.input.is_some() {
                match key.code {
                    KeyCode::Enter => a.submit_input(),
                    KeyCode::Esc => a.cancel_input(),
                    KeyCode::Backspace => a.pop_input_char(),
                    KeyCode::Char(c) => a.push_input_char(c),
                    _ => {}
                }
            } else if a.dashboard.confirm.is_some() {
                match key.code {
                    KeyCode::Char('y') => a.confirm_revoke(),
                    _ => a.cancel_revoke(),
                }
            } else {
                match key.code {
                    KeyCode::Esc => a.screen = Screen::Preflight,
                    KeyCode::Up => {
                        a.dashboard.selected = a.dashboard.selected.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        let len = a.dashboard.peers.len();
                        if len > 0 {
                            a.dashboard.selected = (a.dashboard.selected + 1).min(len - 1);
                        }
                    }
                    KeyCode::Char('a') => a.open_add_input(),
                    KeyCode::Char('k') => a.show_saved_key(),
                    KeyCode::Char('r') => a.open_rename_input(),
                    KeyCode::Char('x') | KeyCode::Delete => a.request_revoke(),
                    KeyCode::Char('c') => a.revoke_candidate_key(),
                    KeyCode::Char('l') => a.enter_log(Screen::Dashboard),
                    KeyCode::Char('m') => a.screen = Screen::Map,
                    _ => {}
                }
            }
        }
        Screen::Secret => match key.code {
            KeyCode::Char('s') => a.save_secret_vpn(),
            KeyCode::Char('c') => a.copy_secret_link(),
            KeyCode::Char('p') => a.save_secret_qr(),
            KeyCode::Esc => a.close_secret(),
            _ => {}
        },
        Screen::Map => {
            if key.code == KeyCode::Esc {
                a.screen = Screen::Dashboard;
            }
        }
        Screen::Log => match key.code {
            KeyCode::Esc => a.screen = a.log.return_to.unwrap_or(Screen::Preflight),
            KeyCode::Char('f') => {
                a.log.follow = !a.log.follow;
                if a.log.follow {
                    a.log_bottom();
                }
            }
            KeyCode::Tab => a.cycle_peer_filter(),
            KeyCode::Char('v') => a.cycle_log_view(),
            KeyCode::Up => a.scroll_log(-1),
            KeyCode::Down => a.scroll_log(1),
            KeyCode::PageUp => a.scroll_log(-10),
            KeyCode::PageDown => a.scroll_log(10),
            KeyCode::Home => {
                a.log.follow = false;
                a.log.scroll = 0;
            }
            KeyCode::End | KeyCode::Char('G') => a.log_bottom(),
            _ => {}
        },
    }
    false
}
