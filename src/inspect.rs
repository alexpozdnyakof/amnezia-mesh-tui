use serde::Deserialize;

use crate::config::ServerConfig;
use crate::transport::Transport;

#[derive(Debug, Clone, Deserialize)]
pub struct InspectPeer {
    pub name: String,
    pub ip: String,
    pub created: Option<String>,
}

/// Mirrors `awg-mesh adopt --inspect --json` (added for this panel).
#[derive(Debug, Clone, Deserialize)]
pub struct InspectSummary {
    pub source: String,
    pub image: String,
    pub server_pub: String,
    pub subnet: String,
    pub mtu: String,
    pub listen_port: String,
    pub server_keepalive: String,
    pub obfuscation_params: usize,
    pub peers: Vec<InspectPeer>,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum InspectError {
    #[error("awg-mesh CLI not found on the server — run bootstrap (b) first")]
    CliMissing,
    #[error("inspect failed: {0}")]
    Failed(String),
}

/// Read-only preview of a migration candidate (runs `adopt --inspect`).
pub async fn inspect(
    transport: &Transport,
    server: &ServerConfig,
    container: &str,
) -> Result<InspectSummary, InspectError> {
    let cli = transport
        .exec(&format!("{} --version", server.cli_path))
        .await
        .map_err(|e| InspectError::Failed(e.to_string()))?;
    if !cli.ok() {
        return Err(InspectError::CliMissing);
    }
    // Container names are [a-zA-Z0-9_.-] — safe to embed.
    let out = transport
        .exec(&format!(
            "{} adopt --inspect --container {} --json",
            server.cli_path, container
        ))
        .await
        .map_err(|e| InspectError::Failed(e.to_string()))?;
    if !out.ok() {
        let msg = out.stderr.trim();
        let msg = if msg.is_empty() {
            out.stdout.trim()
        } else {
            msg
        };
        return Err(InspectError::Failed(msg.to_string()));
    }
    let parsed: serde_json::Value = serde_json::from_str(out.stdout_trimmed())
        .map_err(|e| InspectError::Failed(format!("bad inspect json: {e}")))?;
    if parsed.get("ok") == Some(&serde_json::Value::Bool(false)) {
        return Err(InspectError::Failed(
            parsed["error"]
                .as_str()
                .unwrap_or("unknown error")
                .to_string(),
        ));
    }
    serde_json::from_value(parsed["inspect"].clone())
        .map_err(|e| InspectError::Failed(format!("bad inspect payload: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    const INSPECT_JSON: &str = r#"{"ok":true,"inspect":{
        "source":"container 'amnezia-legacy-test'",
        "image":"amneziavpn/amneziawg-go:latest",
        "server_pub":"PUB","subnet":"10.8.1.0/24","mtu":"1280",
        "listen_port":"51820","server_keepalive":"25",
        "obfuscation_params":20,
        "peers":[{"name":"test-phone","ip":"10.8.1.2",
                   "created":"2026-09-14T08:30:00Z"}],
        "warnings":["peer xyz has no usable name comment"]}}"#;

    fn server() -> ServerConfig {
        ServerConfig {
            ssh_alias: "mock".into(),
            cli_path: "awg-mesh".into(),
            container: "amnezia-mesh-awg".into(),
            login_shell: "sh".into(),
        }
    }

    #[tokio::test]
    async fn inspect_parses_summary() {
        let t = Transport::Mock(MockTransport {
            responses: vec![
                ("awg-mesh --version".into(), "awg-mesh 0.2.0\n".into()),
                ("adopt --inspect".into(), format!("{INSPECT_JSON}\n")),
            ],
        });
        let s = inspect(&t, &server(), "amnezia-legacy-test").await.unwrap();
        assert_eq!(s.subnet, "10.8.1.0/24");
        assert_eq!(s.peers[0].name, "test-phone");
        assert_eq!(s.warnings.len(), 1);
    }

    #[tokio::test]
    async fn inspect_reports_cli_missing() {
        let t = Transport::Mock(MockTransport { responses: vec![] });
        let err = inspect(&t, &server(), "x").await.unwrap_err();
        assert!(matches!(err, InspectError::CliMissing));
    }

    #[tokio::test]
    async fn inspect_surfaces_remote_error() {
        let t = Transport::Mock(MockTransport {
            responses: vec![
                ("awg-mesh --version".into(), "awg-mesh 0.2.0\n".into()),
                (
                    "adopt --inspect".into(),
                    r#"{"ok":false,"error":"unsupported [Interface] keys: Foo"}"#.into(),
                ),
            ],
        });
        let err = inspect(&t, &server(), "x").await.unwrap_err();
        assert!(matches!(err, InspectError::Failed(m) if m.contains("Foo")));
    }
}
