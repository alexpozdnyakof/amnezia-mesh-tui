use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const DEFAULT_CLI_PATH: &str = "awg-mesh";
pub const DEFAULT_CONTAINER: &str = "amnezia-mesh-awg";
pub const DEFAULT_LOGIN_SHELL: &str = "sh";

fn default_cli_path() -> String {
    DEFAULT_CLI_PATH.to_string()
}

fn default_container() -> String {
    DEFAULT_CONTAINER.to_string()
}

fn default_login_shell() -> String {
    DEFAULT_LOGIN_SHELL.to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerConfig {
    /// ssh alias or hostname to connect to (resolved by the user's ssh config).
    pub ssh_alias: String,
    /// Path of the awg-mesh CLI on the server.
    #[serde(default = "default_cli_path")]
    pub cli_path: String,
    /// Name of the awg container of an initialized stack.
    #[serde(default = "default_container")]
    pub container: String,
    /// Login shell used to wrap remote commands (docker often is not in the
    /// PATH of non-interactive ssh sessions).
    #[serde(default = "default_login_shell")]
    pub login_shell: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub active_server: String,
    #[serde(default)]
    pub servers: std::collections::BTreeMap<String, ServerConfig>,
}

impl Config {
    pub fn load(path: &PathBuf) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config {
                active_server: String::new(),
                servers: std::collections::BTreeMap::new(),
            }),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &PathBuf) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(std::fs::write(path, toml::to_string_pretty(self)?)?)
    }
}

pub fn default_path() -> PathBuf {
    // Plan pins ~/.config even on macOS (Linux-style XDG location).
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config")
        .join("awg-mesh-tui")
        .join("config.toml")
}
