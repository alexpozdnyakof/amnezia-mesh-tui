use crate::config::ServerConfig;
use crate::transport::Transport;

#[derive(Debug, Clone, Default)]
pub struct ProbeResult {
    pub docker_path: Option<String>,
    pub docker_version: Option<String>,
    pub cli_version: Option<String>,
    /// Human-readable failures that are not fatal (shown as warnings).
    pub notes: Vec<String>,
}

/// Preflight probe (M1 step 1/3): can we reach the server, is docker there,
/// is the amnezia-mesh CLI present (bootstrap candidate if not).
pub async fn probe(transport: &Transport, server: &ServerConfig) -> ProbeResult {
    let mut res = ProbeResult::default();

    let which = transport.exec("command -v docker").await;
    match which {
        Ok(out) if out.ok() && !out.stdout_trimmed().is_empty() => {
            let docker = out.stdout_trimmed().to_string();
            res.docker_path = Some(docker.clone());
            let version = transport
                .exec(&format!(
                    "{docker} version --format '{{{{.Server.Version}}}}'"
                ))
                .await;
            if let Ok(v) = version {
                if v.ok() && !v.stdout_trimmed().is_empty() {
                    res.docker_version = Some(v.stdout_trimmed().to_string());
                } else {
                    res.notes
                        .push(format!("docker daemon not reachable: {}", v.stderr.trim()));
                }
            }
        }
        Ok(out) => {
            let detail = out.stderr.trim();
            res.notes.push(if detail.is_empty() {
                "docker not found in PATH".into()
            } else {
                format!("docker not found in PATH ({detail})")
            });
        }
        Err(e) => res.notes.push(format!("ssh probe failed: {e}")),
    }

    let cli = transport
        .exec(&format!("{} --version", server.cli_path))
        .await;
    if let Ok(out) = cli {
        if out.ok() && out.stdout_trimmed().starts_with("awg-mesh") {
            res.cli_version = Some(out.stdout_trimmed().to_string());
        } else {
            res.notes
                .push(format!("awg-mesh CLI not found at '{}'", server.cli_path));
        }
    }

    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    fn test_server() -> ServerConfig {
        ServerConfig {
            ssh_alias: "mock".into(),
            cli_path: "awg-mesh".into(),
            container: "amnezia-mesh-awg".into(),
            login_shell: "sh".into(),
        }
    }

    #[tokio::test]
    async fn probe_reports_docker_and_cli() {
        let t = Transport::Mock(MockTransport {
            responses: vec![
                ("command -v docker".into(), "/usr/bin/docker\n".into()),
                ("docker version --format".into(), "29.5.2\n".into()),
                ("awg-mesh --version".into(), "awg-mesh 0.2.0\n".into()),
            ],
        });
        let r = probe(&t, &test_server()).await;
        assert_eq!(r.docker_path.as_deref(), Some("/usr/bin/docker"));
        assert_eq!(r.docker_version.as_deref(), Some("29.5.2"));
        assert_eq!(r.cli_version.as_deref(), Some("awg-mesh 0.2.0"));
        assert!(r.notes.is_empty());
    }

    #[tokio::test]
    async fn probe_notes_missing_docker_and_cli() {
        let t = Transport::Mock(MockTransport { responses: vec![] });
        let r = probe(&t, &test_server()).await;
        assert!(r.docker_path.is_none());
        assert!(r.cli_version.is_none());
        assert_eq!(r.notes.len(), 2);
    }
}
