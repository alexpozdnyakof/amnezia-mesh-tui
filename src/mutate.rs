//! Network mutations through the awg-mesh CLI (M4). Every call is wrapped
//! in `AWG_MESH_NONINTERACTIVE=1` — the TUI does its own confirmation and
//! the CLI must never prompt on stdin.

use serde::Deserialize;

use crate::config::ServerConfig;
use crate::transport::Transport;

#[derive(Debug, Clone, Deserialize)]
pub struct PeerAdded {
    pub name: String,
    pub ip: String,
    pub profiles: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum MutateError {
    #[error("awg-mesh CLI not found on the server — bootstrap first")]
    CliMissing,
    #[error("{0}")]
    Failed(String),
}

/// Mirrors the CLI's peers.valid_name: lowercase letters, digits, dash, ≤32.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

async fn run(
    transport: &Transport,
    server: &ServerConfig,
    args: &str,
) -> Result<serde_json::Value, MutateError> {
    let out = transport
        .exec(&format!(
            "AWG_MESH_NONINTERACTIVE=1 {} {args} --json",
            server.cli_path
        ))
        .await
        .map_err(|e| MutateError::Failed(e.to_string()))?;
    if !out.ok() {
        if out.status == Some(127) {
            return Err(MutateError::CliMissing);
        }
        let msg = out.stderr.trim();
        let msg = if msg.is_empty() {
            out.stdout.trim()
        } else {
            msg
        };
        return Err(MutateError::Failed(msg.to_string()));
    }
    let parsed: serde_json::Value = serde_json::from_str(out.stdout_trimmed())
        .map_err(|e| MutateError::Failed(format!("bad json: {e}")))?;
    if parsed.get("ok") == Some(&serde_json::Value::Bool(false)) {
        return Err(MutateError::Failed(
            parsed["error"]
                .as_str()
                .unwrap_or("mutation failed")
                .to_string(),
        ));
    }
    Ok(parsed)
}

pub async fn peer_add(
    transport: &Transport,
    server: &ServerConfig,
    name: &str,
) -> Result<PeerAdded, MutateError> {
    let v = run(transport, server, &format!("peer add {name}")).await?;
    serde_json::from_value(v).map_err(|e| MutateError::Failed(format!("bad payload: {e}")))
}

pub async fn peer_rename(
    transport: &Transport,
    server: &ServerConfig,
    old: &str,
    new: &str,
) -> Result<(), MutateError> {
    run(transport, server, &format!("peer rename {old} {new}")).await?;
    Ok(())
}

pub async fn peer_revoke(
    transport: &Transport,
    server: &ServerConfig,
    name: &str,
) -> Result<(), MutateError> {
    run(transport, server, &format!("peer revoke {name}")).await?;
    Ok(())
}

/// Fetch a generated profile (.vpn) over ssh. `path` comes from the CLI
/// response (server-generated, under the stack root).
pub async fn fetch_profile(transport: &Transport, path: &str) -> Result<String, MutateError> {
    let out = transport
        .exec(&format!("cat {path}"))
        .await
        .map_err(|e| MutateError::Failed(e.to_string()))?;
    if !out.ok() {
        return Err(MutateError::Failed(format!(
            "cannot read {path}: {}",
            out.stderr.trim()
        )));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    fn server() -> ServerConfig {
        ServerConfig {
            ssh_alias: "mock".into(),
            cli_path: "awg-mesh".into(),
            ..ServerConfig::default()
        }
    }

    #[test]
    fn name_validation_matches_cli() {
        assert!(valid_name("phone-2"));
        assert!(valid_name("a"));
        assert!(!valid_name(""));
        assert!(!valid_name("MacBook Pro"));
        assert!(!valid_name("UPPER"));
        assert!(!valid_name("a".repeat(33).as_str()));
        assert!(!valid_name("bad_name"));
    }

    #[tokio::test]
    async fn add_parses_response() {
        let t = Transport::Mock(MockTransport {
            responses: vec![(
                "peer add".into(),
                r#"{"ok":true,"name":"new-phone","ip":"10.8.1.5",
                    "profiles":["/root/m/config/peers/new-phone-exit.vpn",
                                "/root/m/config/peers/new-phone-mesh.vpn"]}"#
                    .into(),
            )],
        });
        let p = peer_add(&t, &server(), "new-phone").await.unwrap();
        assert_eq!(p.name, "new-phone");
        assert_eq!(p.ip, "10.8.1.5");
        assert_eq!(p.profiles.len(), 2);
    }

    #[tokio::test]
    async fn rename_wraps_error() {
        let t = Transport::Mock(MockTransport {
            responses: vec![(
                "peer rename".into(),
                r#"{"ok":false,"error":"peer already exists: x"}"#.into(),
            )],
        });
        let err = peer_rename(&t, &server(), "a", "x").await.unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn cli_missing_detected() {
        let t = Transport::Mock(MockTransport { responses: vec![] });
        let err = peer_revoke(&t, &server(), "x").await.unwrap_err();
        assert!(matches!(err, MutateError::CliMissing));
    }

    #[tokio::test]
    async fn profile_fetch_reads_file() {
        let t = Transport::Mock(MockTransport {
            responses: vec![("config/peers/new-phone".into(), "vpn://deadbeef\n".into())],
        });
        let text = fetch_profile(&t, "/root/m/config/peers/new-phone-exit.vpn")
            .await
            .unwrap();
        assert_eq!(text, "vpn://deadbeef\n");
    }
}
