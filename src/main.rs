mod app;
mod config;
mod probe;
mod transport;
mod ui;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::Parser;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::Mutex;

use app::App;
use config::{Config, ServerConfig};
use transport::Transport;

#[derive(Parser)]
#[command(name = "awg-mesh-tui", about = "TUI admin panel for amnezia-mesh")]
struct Args {
    /// Configured server name (see `servers` in the config file).
    #[arg(long)]
    server: Option<String>,

    /// Config file path (default: ~/.config/awg-mesh-tui/config.toml).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Run the preflight probe once, print the result, exit (no TUI).
    #[arg(long)]
    probe_once: bool,

    /// Use a canned in-process transport (TUI development without a server).
    #[arg(long, hide = true)]
    mock: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
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
            "no server '{}' in {}\n\nexample entry:\n\
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

    run_tui(&name, server, transport).await
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

async fn run_tui(name: &str, server: ServerConfig, transport: Transport) -> Result<()> {
    let app = Arc::new(Mutex::new(App::new(name.to_string(), server)));

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
    Key(crossterm::event::KeyEvent),
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
                App::maybe_spawn_probe(&app, transport.clone());
            }
            Some(event) = rx.recv() => match event {
                Event::Key(key) => match key.code {
                    crossterm::event::KeyCode::Char('q') => return Ok(()),
                    crossterm::event::KeyCode::Char('r') => {
                        app.lock().await.request_probe();
                    }
                    _ => {}
                },
            },
        }
    }
}
