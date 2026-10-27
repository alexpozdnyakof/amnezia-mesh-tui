use serde::Deserialize;

use crate::config::ServerConfig;
use crate::transport::Transport;

#[derive(Debug, Clone, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// Mirrors `awg-mesh doctor --json`.
#[derive(Debug, Clone, Deserialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn failed(&self) -> Vec<&DoctorCheck> {
        self.checks.iter().filter(|c| !c.ok).collect()
    }
}

/// Doctor is slow (listen-port/magicdns retry 15×1s inside the CLI) —
/// poll it on its own long interval, never together with peers.
pub async fn doctor(transport: &Transport, server: &ServerConfig) -> Result<DoctorReport, String> {
    let out = transport
        .exec(&format!("{} doctor --json", server.cli_path))
        .await
        .map_err(|e| e.to_string())?;
    if !out.ok() {
        let msg = out.stderr.trim();
        let msg = if msg.is_empty() {
            out.stdout.trim()
        } else {
            msg
        };
        return Err(msg.to_string());
    }
    let parsed: serde_json::Value =
        serde_json::from_str(out.stdout_trimmed()).map_err(|e| format!("bad doctor json: {e}"))?;
    if parsed.get("ok") == Some(&serde_json::Value::Bool(false)) && parsed.get("error").is_some() {
        // ok:false + error = command failure; ok:false with checks = a report
        // that has failing checks — both are data for the dashboard.
        return Err(parsed["error"]
            .as_str()
            .unwrap_or("doctor failed")
            .to_string());
    }
    serde_json::from_value(parsed).map_err(|e| format!("bad doctor payload: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    const DOCTOR_JSON: &str = r#"{"ok":false,"checks":[
        {"name":"docker-daemon","ok":true,"detail":"running"},
        {"name":"initialized","ok":true,"detail":"state found"},
        {"name":"awg-container","ok":true,"detail":"up"},
        {"name":"dns-container","ok":true,"detail":"up"},
        {"name":"listen-port","ok":true,"detail":"51820"},
        {"name":"ip-forward","ok":true,"detail":"on"},
        {"name":"magicdns","ok":true,"detail":"resolving"},
        {"name":"port-published","ok":false,"detail":"udp port not published"}
    ]}"#;

    fn server() -> ServerConfig {
        ServerConfig {
            ssh_alias: "mock".into(),
            cli_path: "awg-mesh".into(),
            container: "amnezia-mesh-awg".into(),
            login_shell: "sh".into(),
        }
    }

    #[tokio::test]
    async fn doctor_parses_report() {
        let t = Transport::Mock(MockTransport {
            responses: vec![("doctor --json".into(), format!("{DOCTOR_JSON}\n"))],
        });
        let r = doctor(&t, &server()).await.unwrap();
        assert!(!r.ok);
        assert_eq!(r.checks.len(), 8);
        assert_eq!(r.failed().len(), 1);
        assert_eq!(r.failed()[0].name, "port-published");
    }

    #[tokio::test]
    async fn doctor_surfaces_remote_error() {
        let t = Transport::Mock(MockTransport {
            responses: vec![(
                "doctor --json".into(),
                r#"{"ok":false,"error":"not initialized"}"#.into(),
            )],
        });
        let err = doctor(&t, &server()).await.unwrap_err();
        assert!(err.contains("not initialized"));
    }
}
