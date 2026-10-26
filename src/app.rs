use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::bootstrap::bootstrap;
use crate::config::{Config, ServerConfig};
use crate::docker::{discover, ContainerInfo};
use crate::inspect::{inspect, InspectSummary};
use crate::probe::{probe, ProbeResult};
use crate::transport::Transport;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Job {
    Probe,
    Discover,
    Inspect(String),
    Bootstrap,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Preflight,
    Discovery,
    Inspect,
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

    /// Start the next queued job. Cheap to call on every tick.
    pub async fn pump(app: &Arc<Mutex<App>>, transport: &Transport) {
        let job = {
            let mut a = app.lock().await;
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
        }
    }
}
