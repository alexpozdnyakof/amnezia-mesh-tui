use std::sync::Arc;

use tokio::sync::Mutex;

use crate::config::ServerConfig;
use crate::probe::{probe, ProbeResult};
use crate::transport::Transport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeState {
    Idle,
    Running,
    Done,
}

#[derive(Debug)]
pub struct App {
    pub server_name: String,
    pub server: ServerConfig,
    pub probe_state: ProbeState,
    pub probe: Option<ProbeResult>,
    probe_pending: bool,
}

impl App {
    pub fn new(server_name: String, server: ServerConfig) -> Self {
        App {
            server_name,
            server,
            probe_state: ProbeState::Idle,
            probe: None,
            probe_pending: true,
        }
    }

    pub fn request_probe(&mut self) {
        self.probe_pending = true;
    }

    /// Spawn a probe task if one is pending and none is running. Cheap to
    /// call on every tick.
    pub fn maybe_spawn_probe(app: &Arc<Mutex<App>>, transport: Transport) {
        let app_handle = app.clone();
        tokio::spawn(async move {
            let server = {
                let mut app = app_handle.lock().await;
                if app.probe_state == ProbeState::Running || !app.probe_pending {
                    return;
                }
                app.probe_state = ProbeState::Running;
                app.probe_pending = false;
                app.server.clone()
            };
            let result = probe(&transport, &server).await;
            let mut app = app_handle.lock().await;
            app.probe_state = ProbeState::Done;
            app.probe = Some(result);
        });
    }
}
