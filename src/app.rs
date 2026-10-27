use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::bootstrap::bootstrap;
use crate::config::{Config, ServerConfig};
use crate::docker::{discover, stack_services, ContainerInfo};
use crate::doctor::{doctor, DoctorReport};
use crate::inspect::{inspect, InspectSummary};
use crate::logs::{fetch_stats_log, fetch_watchdog_log};
use crate::peers::{fetch_names, resolve_peers, PeerSample, ResolvedPeer};
use crate::probe::{probe, ProbeResult};
use crate::show::parse_show;
use crate::stats::{parse_stats_log, StatsEntry};
use crate::transport::Transport;
use crate::watchdog::{parse_watchdog_log, WatchdogEvent};

const PEERS_INTERVAL: Duration = Duration::from_secs(3);
const SERVICES_INTERVAL: Duration = Duration::from_secs(10);
const DOCTOR_INTERVAL: Duration = Duration::from_secs(45);
const LOG_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Job {
    Probe,
    Discover,
    Inspect(String),
    Bootstrap,
    PollPeers,
    Services,
    Doctor,
    FetchLog,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Preflight,
    Discovery,
    Inspect,
    Dashboard,
    Log,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProbeState {
    Idle,
    Running,
    Done,
}

#[derive(Debug, Default)]
pub struct DiscoveryState {
    pub running: bool,
    pub containers: Vec<ContainerInfo>,
    pub selected: usize,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct InspectState {
    pub running: bool,
    pub container: String,
    pub summary: Option<InspectSummary>,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct BootstrapState {
    pub confirm_pending: bool,
    pub running: bool,
    pub message: Option<String>,
    pub error: Option<String>,
}

/// Live dashboard state (M2): peers poll, stack services, doctor.
#[derive(Debug, Default)]
pub struct DashboardState {
    pub peers: Vec<ResolvedPeer>,
    pub samples: HashMap<String, PeerSample>,
    pub peers_error: Option<String>,
    pub services: Vec<ContainerInfo>,
    pub services_error: Option<String>,
    pub doctor: Option<DoctorReport>,
    pub doctor_error: Option<String>,
    pub last_peers: Option<Instant>,
    pub last_services: Option<Instant>,
    pub last_doctor: Option<Instant>,
}

/// Stats/watchdog log viewer state (M3).
#[derive(Debug, Default)]
pub struct LogState {
    pub entries: Vec<StatsEntry>,
    pub events: Vec<WatchdogEvent>,
    /// pubkey → name (from `peer list --json`, refreshed with every fetch).
    pub names: HashMap<String, String>,
    /// Active filter: exact pubkey; None = all peers.
    pub filter: Option<String>,
    pub follow: bool,
    /// Selected row index into the *filtered* list.
    pub scroll: usize,
    pub error: Option<String>,
    pub last_fetch: Option<Instant>,
    pub return_to: Option<Screen>,
}

#[derive(Debug)]
pub struct App {
    pub server_name: String,
    pub server: ServerConfig,
    pub config: Config,
    pub cfg_path: PathBuf,
    pub screen: Screen,
    pub probe: Option<ProbeResult>,
    pub discovery: DiscoveryState,
    pub inspect: InspectState,
    pub bootstrap: BootstrapState,
    pub dashboard: DashboardState,
    pub log: LogState,
    queue: Vec<Job>,
    running: Option<Job>,
}

impl App {
    pub fn new(
        server_name: String,
        server: ServerConfig,
        config: Config,
        cfg_path: PathBuf,
    ) -> Self {
        App {
            server_name,
            server,
            config,
            cfg_path,
            screen: Screen::Preflight,
            probe: None,
            discovery: DiscoveryState::default(),
            inspect: InspectState::default(),
            bootstrap: BootstrapState::default(),
            dashboard: DashboardState::default(),
            log: LogState::default(),
            queue: vec![Job::Probe],
            running: None,
        }
    }

    pub fn request(&mut self, job: Job) {
        if self.running.as_ref() == Some(&job) || self.queue.contains(&job) {
            return;
        }
        self.queue.push(job);
    }

    pub fn probe_state(&self) -> ProbeState {
        if self.running.as_ref() == Some(&Job::Probe) {
            ProbeState::Running
        } else if self.probe.is_some() {
            ProbeState::Done
        } else {
            ProbeState::Idle
        }
    }

    /// Switch to the dashboard and fire all polls immediately.
    pub fn enter_dashboard(&mut self) {
        self.screen = Screen::Dashboard;
        self.dashboard.last_peers = None;
        self.dashboard.last_services = None;
        self.dashboard.last_doctor = None;
        self.request(Job::PollPeers);
        self.request(Job::Services);
        self.request(Job::Doctor);
    }

    /// Open the log viewer, returning to `from` on Esc.
    pub fn enter_log(&mut self, from: Screen) {
        self.screen = Screen::Log;
        self.log.return_to = Some(from);
        self.log.follow = true;
        self.log.scroll = 0;
        self.log.filter = None;
        self.log.last_fetch = None;
        self.request(Job::FetchLog);
    }

    /// Tab: cycle the peer filter through [all, first, second, …, all].
    pub fn cycle_peer_filter(&mut self) {
        let mut pubs: Vec<String> = Vec::new();
        for e in &self.log.entries {
            if !pubs.contains(&e.peer) {
                pubs.push(e.peer.clone());
            }
        }
        self.log.filter = match &self.log.filter {
            None => pubs.first().cloned(),
            Some(cur) => match pubs.iter().position(|p| p == cur) {
                Some(i) if i + 1 < pubs.len() => Some(pubs[i + 1].clone()),
                _ => None,
            },
        };
        self.log.scroll = 0;
    }

    pub fn visible_log_len(&self) -> usize {
        self.log
            .entries
            .iter()
            .filter(|e| self.log.filter.as_deref().is_none_or(|fp| e.peer == fp))
            .count()
    }

    pub fn scroll_log(&mut self, delta: isize) {
        self.log.follow = false;
        let len = self.visible_log_len() as isize;
        let s = (self.log.scroll as isize + delta).clamp(0, (len - 1).max(0));
        self.log.scroll = s as usize;
    }

    pub fn log_bottom(&mut self) {
        self.log.scroll = self.visible_log_len().saturating_sub(1);
        self.log.follow = true;
    }

    fn due(at: Option<Instant>, interval: Duration) -> bool {
        at.is_none_or(|t| t.elapsed() >= interval)
    }

    /// Queue periodic polls when their interval has elapsed.
    /// Called from `pump` on every tick; `request` dedups.
    fn schedule_periodics(&mut self) {
        if self.running.is_some() {
            return;
        }
        if self.screen == Screen::Dashboard {
            if Self::due(self.dashboard.last_peers, PEERS_INTERVAL) {
                self.request(Job::PollPeers);
            }
            if Self::due(self.dashboard.last_services, SERVICES_INTERVAL) {
                self.request(Job::Services);
            }
            if Self::due(self.dashboard.last_doctor, DOCTOR_INTERVAL) {
                self.request(Job::Doctor);
            }
        }
        if self.screen == Screen::Log
            && self.log.follow
            && Self::due(self.log.last_fetch, LOG_INTERVAL)
        {
            self.request(Job::FetchLog);
        }
    }

    /// Start the next queued job. Cheap to call on every tick.
    pub async fn pump(app: &Arc<Mutex<App>>, transport: &Transport) {
        let job = {
            let mut a = app.lock().await;
            a.schedule_periodics();
            if a.running.is_some() {
                return;
            }
            match a.queue.first().cloned() {
                Some(j) => {
                    a.queue.remove(0);
                    a.running = Some(j.clone());
                    a.set_job_running(&j, true);
                    j
                }
                None => return,
            }
        };

        let handle = app.clone();
        let transport = transport.clone();
        tokio::spawn(async move {
            let server = {
                let a = handle.lock().await;
                a.server.clone()
            };
            match job.clone() {
                Job::Probe => {
                    let result = probe(&transport, &server).await;
                    let mut a = handle.lock().await;
                    a.probe = Some(result);
                }
                Job::Discover => {
                    let docker = {
                        let a = handle.lock().await;
                        a.probe.as_ref().and_then(|p| p.docker_path.clone())
                    };
                    let outcome = match docker {
                        None => Err("run the probe first (r)".into()),
                        Some(d) => discover(&transport, &d).await,
                    };
                    let mut a = handle.lock().await;
                    a.discovery.running = false;
                    match outcome {
                        Ok(containers) => {
                            a.discovery.containers = containers;
                            a.discovery.selected = 0;
                            a.discovery.error = None;
                            a.screen = Screen::Discovery;
                        }
                        Err(e) => a.discovery.error = Some(e),
                    }
                }
                Job::Inspect(container) => {
                    let outcome = inspect(&transport, &server, &container).await;
                    let mut a = handle.lock().await;
                    a.inspect.running = false;
                    a.inspect.container = container;
                    match outcome {
                        Ok(summary) => {
                            a.inspect.summary = Some(summary);
                            a.inspect.error = None;
                        }
                        Err(e) => {
                            a.inspect.summary = None;
                            a.inspect.error = Some(e.to_string());
                        }
                    }
                    a.screen = Screen::Inspect;
                }
                Job::Bootstrap => {
                    let outcome = bootstrap(&transport).await;
                    let mut a = handle.lock().await;
                    a.bootstrap.running = false;
                    match outcome {
                        Ok(result) => {
                            let action = if result.updated {
                                "updated"
                            } else {
                                "installed"
                            };
                            a.bootstrap.message = Some(format!(
                                "{} {} → {}",
                                action, result.cli_path, result.version
                            ));
                            a.bootstrap.error = None;
                            a.server.cli_path = result.cli_path.clone();
                            let server_name = a.server_name.clone();
                            if let Some(s) = a.config.servers.get_mut(&server_name) {
                                s.cli_path = result.cli_path;
                            }
                            if let Err(e) = a.config.save(&a.cfg_path) {
                                a.bootstrap.error = Some(format!("config not saved: {e}"));
                            }
                            a.queue.push(Job::Probe);
                        }
                        Err(e) => a.bootstrap.error = Some(e),
                    }
                }
                Job::PollPeers => {
                    let (docker, container, cli) = {
                        let a = handle.lock().await;
                        (
                            a.probe.as_ref().and_then(|p| p.docker_path.clone()),
                            a.server.container.clone(),
                            a.server.cli_path.clone(),
                        )
                    };
                    let outcome = async {
                        let docker = docker.ok_or("run the probe first (r)")?;
                        let out = transport
                            .exec(&format!("{docker} exec {container} awg show"))
                            .await
                            .map_err(|e| e.to_string())?;
                        if !out.ok() {
                            let msg = out.stderr.trim();
                            return Err(if msg.is_empty() {
                                format!("awg show failed in '{container}'")
                            } else {
                                format!("awg show failed in '{container}': {msg}")
                            });
                        }
                        let info = parse_show(&out.stdout)?;
                        let names_pub = fetch_names(&transport, &cli).await;
                        Ok((info, names_pub))
                    }
                    .await;
                    let mut a = handle.lock().await;
                    a.dashboard.last_peers = Some(Instant::now());
                    match outcome {
                        Ok((info, names_pub)) => {
                            let names_ip: HashMap<String, String> = a
                                .inspect
                                .summary
                                .as_ref()
                                .map(|s| {
                                    s.peers
                                        .iter()
                                        .map(|p| (p.ip.clone(), p.name.clone()))
                                        .collect()
                                })
                                .unwrap_or_default();
                            a.dashboard.peers = resolve_peers(
                                &info.peers,
                                &names_pub,
                                &names_ip,
                                &mut a.dashboard.samples,
                                Instant::now(),
                            );
                            a.dashboard.peers_error = None;
                        }
                        Err(e) => a.dashboard.peers_error = Some(e),
                    }
                }
                Job::Services => {
                    let docker = {
                        let a = handle.lock().await;
                        a.probe.as_ref().and_then(|p| p.docker_path.clone())
                    };
                    let outcome = match docker {
                        None => Err("run the probe first (r)".into()),
                        Some(d) => stack_services(&transport, &d).await,
                    };
                    let mut a = handle.lock().await;
                    a.dashboard.last_services = Some(Instant::now());
                    match outcome {
                        Ok(list) => {
                            a.dashboard.services = list;
                            a.dashboard.services_error = None;
                        }
                        Err(e) => a.dashboard.services_error = Some(e),
                    }
                }
                Job::Doctor => {
                    let outcome = doctor(&transport, &server).await;
                    let mut a = handle.lock().await;
                    a.dashboard.last_doctor = Some(Instant::now());
                    match outcome {
                        Ok(report) => {
                            a.dashboard.doctor = Some(report);
                            a.dashboard.doctor_error = None;
                        }
                        Err(e) => a.dashboard.doctor_error = Some(e),
                    }
                }
                Job::FetchLog => {
                    let cli = {
                        let a = handle.lock().await;
                        a.server.cli_path.clone()
                    };
                    let outcome = async {
                        let stats = fetch_stats_log(&transport).await?;
                        let watchdog = fetch_watchdog_log(&transport).await?;
                        let names = fetch_names(&transport, &cli).await;
                        Ok((
                            parse_stats_log(&stats),
                            parse_watchdog_log(&watchdog),
                            names,
                        ))
                    }
                    .await;
                    let mut a = handle.lock().await;
                    a.log.last_fetch = Some(Instant::now());
                    match outcome {
                        Ok((entries, events, names)) => {
                            a.log.entries = entries;
                            a.log.events = events;
                            a.log.names = names;
                            a.log.error = None;
                        }
                        Err(e) => a.log.error = Some(e),
                    }
                }
            }
            let mut a = handle.lock().await;
            a.running = None;
        });
    }

    fn set_job_running(&mut self, job: &Job, running: bool) {
        match job {
            Job::Probe => {}
            Job::Discover => self.discovery.running = running,
            Job::Inspect(_) => self.inspect.running = running,
            Job::Bootstrap => self.bootstrap.running = running,
            Job::PollPeers | Job::Services | Job::Doctor | Job::FetchLog => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::mock_canned;

    fn test_app() -> App {
        let server = ServerConfig {
            ssh_alias: "mock".into(),
            cli_path: "awg-mesh".into(),
            container: "amnezia-legacy-test".into(),
            login_shell: "sh".into(),
        };
        let mut config = Config {
            active_server: "mock".into(),
            servers: std::collections::BTreeMap::new(),
        };
        config.servers.insert("mock".into(), server.clone());
        App::new("mock".into(), server, config, PathBuf::from("/tmp/none"))
    }

    #[tokio::test]
    async fn fetch_log_job_fills_log_state() {
        let app = Arc::new(Mutex::new(test_app()));
        let transport = mock_canned();
        app.lock().await.enter_log(Screen::Preflight);
        // App::new seeds the queue with Probe — pump until FetchLog lands.
        for _ in 0..50 {
            App::pump(&app, &transport).await;
            if app.lock().await.log.last_fetch.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let a = app.lock().await;
        assert!(a.log.error.is_none(), "log error: {:?}", a.log.error);
        assert_eq!(a.log.entries.len(), 7);
        assert_eq!(a.log.events.len(), 2);
        assert_eq!(a.log.names.len(), 3);
        assert!(a.log.error.is_none());
    }

    #[tokio::test]
    async fn peer_filter_cycles_all_then_each_peer() {
        let mut app = test_app();
        app.log.entries = vec![
            StatsEntry {
                peer: "P1".into(),
                ..StatsEntry::default()
            },
            StatsEntry {
                peer: "P2".into(),
                ..StatsEntry::default()
            },
        ];
        app.cycle_peer_filter();
        assert_eq!(app.log.filter.as_deref(), Some("P1"));
        app.cycle_peer_filter();
        assert_eq!(app.log.filter.as_deref(), Some("P2"));
        app.cycle_peer_filter();
        assert_eq!(app.log.filter, None);
    }
}
