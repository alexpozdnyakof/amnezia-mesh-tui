use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use zeroize::Zeroize;

use crate::bootstrap::bootstrap;
use crate::config::{Config, ServerConfig};
use crate::docker::{discover, stack_services, ContainerInfo};
use crate::doctor::{doctor, DoctorReport};
use crate::geo::{endpoint_ip, GeoDb, GeoInfo};
use crate::inspect::{inspect, InspectSummary};
use crate::logs::{fetch_stats_log, fetch_watchdog_log};
use crate::mutate::{self, MutateError, PeerAdded};
use crate::peers::{fetch_names, resolve_peers, PeerSample, PeerStatus, ResolvedPeer};
use crate::probe::{probe, ProbeResult};
use crate::profile::{self, ProfileView};
use crate::show::parse_show;
use crate::stats::{parse_stats_log, StatsEntry};
use crate::transport::Transport;
use crate::watchdog::{parse_watchdog_log, WatchdogEvent};
use crate::wizard::{self, Form, RunOutcome};

const READ_ONLY_MSG: &str = "server is read-only (read_only = true in config)";
const RUN_CAP: usize = 400;

fn downloads_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home).join("Downloads");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn file_stem(name: &str) -> String {
    let stem: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if stem.is_empty() {
        "peer".to_string()
    } else {
        stem
    }
}

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
    Mutate(Mutation),
    DetectHost,
    Provision,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Mutation {
    Add(String),
    Rename { old: String, new: String },
    Revoke(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Preflight,
    Discovery,
    Inspect,
    Dashboard,
    Map,
    Log,
    Secret,
    Wizard,
    Run,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LogView {
    #[default]
    List,
    Chart,
    Heat,
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

/// Geo-enriched peer row for the connection map (M6).
#[derive(Debug, Clone)]
pub struct PeerGeo {
    pub name: String,
    pub endpoint_ip: String,
    pub geo: Option<GeoInfo>,
    pub status: PeerStatus,
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
    pub geo: Vec<PeerGeo>,
    pub selected: usize,
    /// Inline text input (peer add / rename) rendered in the keys bar.
    pub input: Option<InputState>,
    /// Peer name pending the revoke confirmation.
    pub confirm: Option<String>,
    /// Last mutation result: (message, is_error).
    pub notice: Option<(String, bool)>,
    /// Poll failures are hidden for a while after a mutation (the stack
    /// restarts, ~5-10s of errors is expected).
    pub poll_silence: Option<Instant>,
    pub last_peers: Option<Instant>,
    pub last_services: Option<Instant>,
    pub last_doctor: Option<Instant>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum InputKind {
    AddPeer,
    RenamePeer(String),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InputState {
    pub prompt: String,
    pub value: String,
    pub kind: InputKind,
}

/// Freshly issued key material (M4). Shown once, zeroized on close.
/// `view` is the public summary only — the private key stays in `profile`.
#[derive(Debug)]
pub struct SecretState {
    pub name: String,
    pub ip: String,
    pub profile: String,
    pub saved_to: Option<String>,
    pub view: Option<ProfileView>,
    pub qr_saved: Option<String>,
    pub qr_error: Option<String>,
}

impl SecretState {
    pub(crate) fn issued(name: String, ip: String, profile: String) -> Self {
        let view = profile::find_vpn_url(&profile)
            .and_then(|url| profile::decode_profile(url).ok())
            .as_ref()
            .map(profile::summarize);
        Self {
            name,
            ip,
            profile,
            saved_to: None,
            view,
            qr_saved: None,
            qr_error: None,
        }
    }
}

impl Drop for SecretState {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.profile.zeroize();
    }
}

/// Init/adopt form (M1). `return_to` is where Esc lands.
#[derive(Debug)]
pub struct WizardView {
    pub form: Form,
    pub return_to: Screen,
}

/// Live init/adopt transcript.
#[derive(Debug)]
pub struct RunState {
    pub title: String,
    pub cmd: String,
    pub lines: Vec<String>,
    pub running: bool,
    pub outcome: Option<RunOutcome>,
    pub scroll: usize,
    pub follow: bool,
    pub return_to: Screen,
}

impl Default for RunState {
    fn default() -> Self {
        Self {
            title: String::new(),
            cmd: String::new(),
            lines: Vec::new(),
            running: false,
            outcome: None,
            scroll: 0,
            follow: true,
            return_to: Screen::Preflight,
        }
    }
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
    pub view: LogView,
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
    pub geo_db: Option<GeoDb>,
    pub geo_error: Option<String>,
    pub secret: Option<SecretState>,
    pub wizard: WizardView,
    pub run: RunState,
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
        let geo_path = server
            .mmdb_path
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(crate::geo::default_mmdb_path);
        let (geo_db, geo_error) = match GeoDb::load(&geo_path) {
            Ok(db) => (Some(db), None),
            Err(e) => (None, Some(e)),
        };
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
            geo_db,
            geo_error,
            secret: None,
            wizard: WizardView {
                form: Form::init(),
                return_to: Screen::Preflight,
            },
            run: RunState::default(),
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

    pub fn cycle_log_view(&mut self) {
        self.log.view = match self.log.view {
            LogView::List => LogView::Chart,
            LogView::Chart => LogView::Heat,
            LogView::Heat => LogView::List,
        };
    }

    // --- M4: mutations (input, confirm, secret) ---

    pub fn read_only(&self) -> bool {
        self.server.read_only == Some(true)
    }

    fn reject_when_read_only(&mut self) -> bool {
        if self.read_only() {
            self.mutate_notice(READ_ONLY_MSG.into(), true);
            true
        } else {
            false
        }
    }

    pub fn open_init_wizard(&mut self) {
        if self.read_only() {
            self.bootstrap.error = Some(READ_ONLY_MSG.into());
            return;
        }
        self.wizard = WizardView {
            form: Form::init(),
            return_to: Screen::Preflight,
        };
        self.screen = Screen::Wizard;
        self.request(Job::DetectHost);
    }

    pub fn open_adopt_wizard(&mut self) {
        if self.inspect.summary.is_none() || self.inspect.container.is_empty() {
            return;
        }
        if self.read_only() {
            self.inspect.error = Some(READ_ONLY_MSG.into());
            return;
        }
        self.wizard = WizardView {
            form: Form::adopt(self.inspect.container.clone()),
            return_to: Screen::Inspect,
        };
        self.screen = Screen::Wizard;
        self.request(Job::DetectHost);
    }

    /// First `r` arms the confirmation; the second builds the command and runs it.
    pub fn arm_provision(&mut self) {
        if self.read_only() {
            self.wizard.form.error = Some(READ_ONLY_MSG.into());
            return;
        }
        self.wizard.form.editing = false;
        let cmd = match wizard::build_command(&self.server.cli_path, &self.wizard.form) {
            Ok(cmd) => cmd,
            Err(e) => {
                self.wizard.form.error = Some(e);
                self.wizard.form.confirm_pending = false;
                return;
            }
        };
        if !self.wizard.form.confirm_pending {
            self.wizard.form.confirm_pending = true;
            self.wizard.form.error = None;
            return;
        }
        self.wizard.form.confirm_pending = false;
        self.run = RunState {
            title: self.wizard.form.kind.title(),
            cmd: cmd.clone(),
            lines: vec![format!("$ {cmd}")],
            running: true,
            outcome: None,
            scroll: 0,
            follow: true,
            return_to: self.wizard.return_to,
        };
        self.screen = Screen::Run;
        self.request(Job::Provision);
    }

    pub fn push_run_line(&mut self, line: String) {
        self.run.lines.push(line);
        if self.run.lines.len() > RUN_CAP {
            let extra = self.run.lines.len() - RUN_CAP;
            self.run.lines.drain(0..extra);
            self.run.scroll = self.run.scroll.saturating_sub(extra);
        }
        if self.run.follow {
            self.run.scroll = self.run.lines.len().saturating_sub(1);
        }
    }

    pub fn scroll_run(&mut self, delta: isize) {
        self.run.follow = false;
        let len = self.run.lines.len() as isize;
        let s = (self.run.scroll as isize + delta).clamp(0, (len - 1).max(0));
        self.run.scroll = s as usize;
    }

    fn note_stack_container(&mut self) {
        self.server.container = crate::config::DEFAULT_CONTAINER.to_string();
        let name = self.server_name.clone();
        if let Some(server) = self.config.servers.get_mut(&name) {
            server.container = self.server.container.clone();
        }
        if let Err(e) = self.config.save(&self.cfg_path) {
            self.push_run_line(format!("stack is up; config not saved: {e}"));
        } else {
            self.push_run_line(format!(
                "dashboard watches container {}",
                self.server.container
            ));
        }
    }

    pub fn open_add_input(&mut self) {
        if self.reject_when_read_only() {
            return;
        }
        self.dashboard.notice = None;
        self.dashboard.input = Some(InputState {
            prompt: "add peer — name".to_string(),
            value: String::new(),
            kind: InputKind::AddPeer,
        });
    }

    pub fn open_rename_input(&mut self) {
        if self.reject_when_read_only() {
            return;
        }
        let Some(peer) = self.selected_peer().cloned() else {
            return;
        };
        self.dashboard.notice = None;
        self.dashboard.input = Some(InputState {
            prompt: format!("rename {0} — new name", peer.name),
            value: String::new(),
            kind: InputKind::RenamePeer(peer.name),
        });
    }

    pub fn selected_peer(&self) -> Option<&ResolvedPeer> {
        self.dashboard.peers.get(self.dashboard.selected)
    }

    pub fn push_input_char(&mut self, c: char) {
        if let Some(input) = &mut self.dashboard.input {
            if input.value.len() < 32 {
                input.value.push(c);
            }
        }
    }

    pub fn pop_input_char(&mut self) {
        if let Some(input) = &mut self.dashboard.input {
            input.value.pop();
        }
    }

    pub fn cancel_input(&mut self) {
        self.dashboard.input = None;
    }

    /// Enter on the input bar: validate and queue the mutation.
    pub fn submit_input(&mut self) {
        let Some(input) = self.dashboard.input.take() else {
            return;
        };
        if !mutate::valid_name(&input.value) {
            self.dashboard.notice = Some((
                "invalid name: lowercase letters, digits, dash, max 32".into(),
                true,
            ));
            return;
        }
        match input.kind {
            InputKind::AddPeer => self.request(Job::Mutate(Mutation::Add(input.value))),
            InputKind::RenamePeer(old) => self.request(Job::Mutate(Mutation::Rename {
                old,
                new: input.value,
            })),
        }
    }

    pub fn request_revoke(&mut self) {
        if self.reject_when_read_only() {
            return;
        }
        let name = self.selected_peer().map(|p| p.name.clone());
        if let Some(name) = name {
            self.dashboard.notice = None;
            self.dashboard.confirm = Some(name);
        }
    }

    /// `c` on the dashboard. The selected silent peer opens the existing
    /// confirm. Otherwise jump to the next candidate. A peer that has never
    /// handshaken is not a candidate.
    pub fn revoke_candidate_key(&mut self) {
        if self.reject_when_read_only() {
            return;
        }
        let n = self.dashboard.peers.len();
        if n == 0 {
            self.mutate_notice("no peer has been silent for 14 days".into(), false);
            return;
        }
        let sel = self.dashboard.selected.min(n - 1);
        if self.dashboard.peers[sel].revoke_candidate {
            self.request_revoke();
            return;
        }
        let next = (1..n)
            .map(|off| (sel + off) % n)
            .find(|&i| self.dashboard.peers[i].revoke_candidate);
        match next {
            Some(i) => {
                let name = self.dashboard.peers[i].name.clone();
                self.dashboard.selected = i;
                self.mutate_notice(
                    format!("{name} has been silent for 14 days — c revokes"),
                    false,
                );
            }
            None => self.mutate_notice("no peer has been silent for 14 days".into(), false),
        }
    }

    pub fn cancel_revoke(&mut self) {
        self.dashboard.confirm = None;
    }

    pub fn confirm_revoke(&mut self) {
        if let Some(name) = self.dashboard.confirm.take() {
            self.request(Job::Mutate(Mutation::Revoke(name)));
        }
    }

    /// Close the secret screen; the profile is zeroized on drop.
    pub fn close_secret(&mut self) {
        self.secret = None;
        if self.screen == Screen::Secret {
            self.screen = Screen::Dashboard;
        }
    }

    pub fn mark_secret_saved(&mut self, path: String) {
        if let Some(s) = &mut self.secret {
            s.saved_to = Some(path);
        }
    }

    pub fn save_secret_vpn(&mut self) {
        let Some(dir) = downloads_dir() else {
            return;
        };
        let written = {
            let Some(secret) = &self.secret else {
                return;
            };
            let path = dir.join(format!("{}.vpn", file_stem(&secret.name)));
            std::fs::write(&path, secret.profile.as_bytes())
                .ok()
                .map(|_| path.display().to_string())
        };
        if let Some(shown) = written {
            self.mark_secret_saved(shown);
        }
    }

    pub fn save_secret_qr(&mut self) {
        let extracted = self.secret.as_ref().map(|secret| {
            (
                profile::find_vpn_url(&secret.profile).map(str::to_string),
                file_stem(&secret.name),
            )
        });
        let Some((url, stem)) = extracted else {
            return;
        };
        let Some(mut url) = url else {
            if let Some(secret) = &mut self.secret {
                secret.qr_error = Some("no vpn:// link in this profile".into());
            }
            return;
        };
        let Some(dir) = downloads_dir() else {
            url.zeroize();
            if let Some(secret) = &mut self.secret {
                secret.qr_error = Some("could not create ~/Downloads".into());
            }
            return;
        };
        let path = dir.join(format!("{stem}-qr.png"));
        let written = profile::write_qr_png(&url, &path);
        url.zeroize();
        match written {
            Ok(()) => {
                if let Some(secret) = &mut self.secret {
                    secret.qr_saved = Some(path.display().to_string());
                    secret.qr_error = None;
                }
            }
            Err(e) => {
                if let Some(secret) = &mut self.secret {
                    secret.qr_error = Some(e.to_string());
                }
            }
        }
    }

    fn mutate_notice(&mut self, message: String, is_error: bool) {
        self.dashboard.notice = Some((message, is_error));
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
        match self.screen {
            Screen::Dashboard => {
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
            // Map rides the peers poll; services/doctor stay on the dashboard.
            Screen::Map => {
                if Self::due(self.dashboard.last_peers, PEERS_INTERVAL) {
                    self.request(Job::PollPeers);
                }
            }
            _ => {}
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
                            a.dashboard.geo = a
                                .geo_db
                                .as_ref()
                                .map(|db| {
                                    a.dashboard
                                        .peers
                                        .iter()
                                        .filter_map(|p| {
                                            let ep = p.endpoint.as_deref()?;
                                            let ip = endpoint_ip(ep)?;
                                            Some(PeerGeo {
                                                name: p.name.clone(),
                                                endpoint_ip: ip.to_string(),
                                                geo: db.lookup(ip),
                                                status: p.status,
                                            })
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                        }
                        Err(e) => {
                            let silenced = a
                                .dashboard
                                .poll_silence
                                .is_some_and(|until| Instant::now() < until);
                            if !silenced {
                                a.dashboard.peers_error = Some(e);
                            }
                        }
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
                Job::Mutate(mutation) => {
                    enum Outcome {
                        Added(PeerAdded, Option<String>),
                        Renamed(String, String),
                        Revoked(String),
                    }
                    let outcome = async {
                        match &mutation {
                            Mutation::Add(name) => {
                                let added = mutate::peer_add(&transport, &server, name).await?;
                                let vpn_path =
                                    added.profiles.iter().find(|p| p.ends_with(".vpn")).cloned();
                                let profile = match vpn_path {
                                    Some(p) => Some(mutate::fetch_profile(&transport, &p).await?),
                                    None => None,
                                };
                                Ok(Outcome::Added(added, profile))
                            }
                            Mutation::Rename { old, new } => {
                                mutate::peer_rename(&transport, &server, old, new).await?;
                                Ok(Outcome::Renamed(old.clone(), new.clone()))
                            }
                            Mutation::Revoke(name) => {
                                mutate::peer_revoke(&transport, &server, name).await?;
                                Ok(Outcome::Revoked(name.clone()))
                            }
                        }
                    }
                    .await
                    .map_err(|e: MutateError| e.to_string());
                    let mut a = handle.lock().await;
                    // The stack restarts after every mutation: hush the
                    // poll for a few seconds and refresh right away.
                    a.dashboard.poll_silence = Some(Instant::now() + Duration::from_secs(10));
                    a.queue.push(Job::PollPeers);
                    match outcome {
                        Ok(Outcome::Added(added, profile)) => {
                            a.mutate_notice(format!("added {} → {}", added.name, added.ip), false);
                            a.secret = Some(SecretState::issued(
                                added.name,
                                added.ip,
                                profile.unwrap_or_else(|| "<profile unavailable>".to_string()),
                            ));
                            a.screen = Screen::Secret;
                        }
                        Ok(Outcome::Renamed(old, new)) => {
                            a.mutate_notice(format!("renamed {old} → {new}"), false);
                        }
                        Ok(Outcome::Revoked(name)) => {
                            a.mutate_notice(format!("revoked {name}"), false);
                            a.dashboard.selected = 0;
                        }
                        Err(e) => a.mutate_notice(format!("mutation failed: {e}"), true),
                    }
                    if matches!(a.screen, Screen::Secret)
                        || a.dashboard.notice.as_ref().is_some_and(|(_, err)| !err)
                    {
                        if let Some(hook) = a.config.post_mutation_hook.clone() {
                            let _ = tokio::process::Command::new("sh")
                                .arg("-c")
                                .arg(hook)
                                .spawn();
                        }
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
                Job::DetectHost => {
                    let force = handle.lock().await.wizard.form.detect_force;
                    let out = transport.exec(wizard::DETECT_HOST_CMD).await;
                    let mut a = handle.lock().await;
                    a.wizard.form.detect_force = false;
                    match out {
                        Ok(output) if output.ok() => {
                            let applied = if force {
                                a.wizard.form.force_host(output.stdout_trimmed())
                            } else {
                                a.wizard.form.apply_detected_host(output.stdout_trimmed())
                            };
                            if force && !applied {
                                a.wizard.form.error =
                                    Some("could not detect a public address".into());
                            }
                        }
                        Ok(_) if force => {
                            a.wizard.form.error = Some("could not detect a public address".into());
                        }
                        Err(e) if force => a.wizard.form.error = Some(e.to_string()),
                        _ => {}
                    }
                }
                Job::Provision => {
                    let cmd = handle.lock().await.run.cmd.clone();
                    if cmd.is_empty() {
                        let mut a = handle.lock().await;
                        a.push_run_line("internal: empty command".into());
                        a.run.outcome = Some(RunOutcome::Failed);
                        a.run.running = false;
                    } else {
                        // Lines arrive on a channel so the UI can repaint while
                        // the CLI is still running. A blocking lock here panics:
                        // this task is already on the tokio runtime.
                        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                        let mut exec = std::pin::pin!(transport.exec_lines(&cmd, move |line| {
                            let _ = tx.send(line);
                        }));
                        let mut result = None;
                        loop {
                            tokio::select! {
                                biased;
                                line = rx.recv() => {
                                    match line {
                                        Some(line) => handle.lock().await.push_run_line(line),
                                        None => {
                                            if result.is_none() {
                                                result = Some((&mut exec).await);
                                            }
                                            break;
                                        }
                                    }
                                }
                                res = &mut exec, if result.is_none() => {
                                    result = Some(res);
                                    while let Ok(line) = rx.try_recv() {
                                        handle.lock().await.push_run_line(line);
                                    }
                                    break;
                                }
                            }
                        }
                        let result = result.expect("provision exec finished");
                        let mut a = handle.lock().await;
                        match result {
                            Ok(output) => {
                                let log = a.run.lines.join("\n");
                                let outcome = wizard::classify(output.status, &log);
                                a.run.outcome = Some(outcome);
                                if outcome == RunOutcome::Success {
                                    a.note_stack_container();
                                }
                            }
                            Err(e) => {
                                a.push_run_line(e.to_string());
                                a.run.outcome = Some(RunOutcome::Failed);
                            }
                        }
                        a.run.running = false;
                        a.run.follow = false;
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
            Job::PollPeers | Job::Services | Job::Doctor | Job::FetchLog | Job::DetectHost => {}
            Job::Mutate(_) => {}
            Job::Provision => self.run.running = running,
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
            ..ServerConfig::default()
        };
        let mut config = Config {
            active_server: "mock".into(),
            servers: std::collections::BTreeMap::new(),
            post_mutation_hook: None,
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

    #[tokio::test]
    async fn provision_success_points_dashboard_at_stack_container() {
        let path = std::env::temp_dir().join(format!(
            "awg-mesh-tui-provision-{}.toml",
            std::process::id()
        ));
        let app = Arc::new(Mutex::new(test_app()));
        {
            let mut a = app.lock().await;
            a.cfg_path = path.clone();
            let mut form = Form::init();
            form.set_field(wizard::FieldId::Host, "203.0.113.10");
            let cmd = wizard::build_command("awg-mesh", &form).unwrap();
            a.run = RunState {
                title: form.kind.title(),
                cmd,
                lines: Vec::new(),
                running: true,
                outcome: None,
                scroll: 0,
                follow: true,
                return_to: Screen::Preflight,
            };
            a.request(Job::Provision);
        }
        let transport = mock_canned();
        for _ in 0..80 {
            App::pump(&app, &transport).await;
            if app.lock().await.run.outcome.is_some() && app.lock().await.running.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let a = app.lock().await;
        assert_eq!(a.run.outcome, Some(RunOutcome::Success));
        assert!(
            a.run.lines.iter().any(|l| l.contains("amnezia-mesh is up")),
            "lines: {:?}",
            a.run.lines
        );
        assert_eq!(a.server.container, "amnezia-mesh-awg");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn provision_rollback_keeps_the_old_container() {
        let app = Arc::new(Mutex::new(test_app()));
        {
            let mut a = app.lock().await;
            a.run = RunState {
                title: "adopt".into(),
                cmd: "AWG_MESH_NONINTERACTIVE=1 awg-mesh adopt --container legacy --host 203.0.113.10".into(),
                lines: Vec::new(),
                running: true,
                outcome: None,
                scroll: 0,
                follow: true,
                return_to: Screen::Inspect,
            };
            a.request(Job::Provision);
        }
        let transport = Transport::Mock(crate::transport::MockTransport {
            responses: vec![(
                "adopt --container".into(),
                "__EXIT__:1\npost-adopt checks failed — rolled back, 'legacy' is running again\n"
                    .into(),
            )],
        });
        for _ in 0..80 {
            App::pump(&app, &transport).await;
            if app.lock().await.run.outcome.is_some() && app.lock().await.running.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let a = app.lock().await;
        assert_eq!(a.run.outcome, Some(RunOutcome::RolledBack));
        assert_eq!(a.server.container, "amnezia-legacy-test");
        assert!(a.run.lines.iter().any(|l| l.contains("rolled back")));
    }

    fn row(name: &str, candidate: bool) -> ResolvedPeer {
        ResolvedPeer {
            name: name.into(),
            ip: "100.64.0.8".into(),
            pubkey: name.into(),
            status: if candidate {
                PeerStatus::Dead
            } else {
                PeerStatus::Live
            },
            handshake: Some(Duration::from_secs(if candidate { 14 * 86400 } else { 30 })),
            endpoint: None,
            rx_rate: 0.0,
            tx_rate: 0.0,
            revoke_candidate: candidate,
        }
    }

    #[test]
    fn c_key_jumps_to_a_silent_peer_then_asks_to_confirm() {
        let mut app = test_app();
        app.dashboard.peers = vec![row("fresh", false), row("gone", true), row("also", true)];
        app.dashboard.selected = 0;
        app.revoke_candidate_key();
        assert_eq!(app.dashboard.selected, 1);
        assert!(app.dashboard.confirm.is_none());
        let (msg, err) = app.dashboard.notice.clone().unwrap();
        assert!(!err);
        assert!(msg.contains("gone"));
        app.revoke_candidate_key();
        assert_eq!(app.dashboard.confirm.as_deref(), Some("gone"));

        app.dashboard.confirm = None;
        app.dashboard.selected = 2;
        app.revoke_candidate_key();
        assert_eq!(app.dashboard.confirm.as_deref(), Some("also"));
    }

    #[test]
    fn c_key_wraps_and_stays_quiet_when_nobody_is_silent() {
        let mut app = test_app();
        app.dashboard.peers = vec![row("gone", true), row("fresh", false)];
        app.dashboard.selected = 1;
        app.revoke_candidate_key();
        assert_eq!(app.dashboard.selected, 0);
        assert!(app.dashboard.confirm.is_none());

        app.dashboard.peers = vec![row("fresh", false)];
        app.dashboard.selected = 0;
        app.revoke_candidate_key();
        assert_eq!(
            app.dashboard.notice.as_ref().map(|(m, _)| m.as_str()),
            Some("no peer has been silent for 14 days")
        );
        assert!(app.dashboard.confirm.is_none());
    }

    #[test]
    fn c_key_on_a_read_only_server_does_not_confirm() {
        let mut app = test_app();
        app.server.read_only = Some(true);
        app.dashboard.peers = vec![row("gone", true)];
        app.revoke_candidate_key();
        assert!(app.dashboard.confirm.is_none());
        let (_, err) = app.dashboard.notice.unwrap();
        assert!(err);
    }
}
