use crate::config::ServerConfig;

#[derive(Debug, Clone)]
pub struct RemoteOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl RemoteOutput {
    pub fn ok(&self) -> bool {
        self.status == Some(0)
    }

    pub fn stdout_trimmed(&self) -> &str {
        self.stdout.trim()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to run ssh: {0}")]
    Spawn(#[from] std::io::Error),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Transport {
    Ssh(SshTransport),
    Mock(MockTransport),
    /// Runs commands on the machine the panel itself runs on (the
    /// `ssh_alias = "local"` convention — e.g. the binary lives on the VPS).
    Local,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SshTransport {
    pub alias: String,
    pub login_shell: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MockTransport {
    /// Canned stdout per command substring; first match wins.
    pub responses: Vec<(String, String)>,
}

impl Transport {
    pub fn for_server(server: &ServerConfig) -> Self {
        if server.ssh_alias == "local" {
            return Transport::Local;
        }
        Transport::Ssh(SshTransport {
            alias: server.ssh_alias.clone(),
            login_shell: server.login_shell.clone(),
        })
    }

    /// Run a command line on the server and capture its output. A non-zero
    /// remote exit status is data (returned in `status`), not an error.
    pub async fn exec(&self, cmd: &str) -> Result<RemoteOutput, Error> {
        match self {
            Transport::Ssh(t) => t.exec(cmd).await,
            Transport::Mock(t) => t.exec(cmd).await,
            Transport::Local => {
                let out = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .output()
                    .await?;
                Ok(RemoteOutput {
                    status: out.status.code(),
                    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                })
            }
        }
    }
}

impl SshTransport {
    pub async fn exec(&self, cmd: &str) -> Result<RemoteOutput, Error> {
        // BatchMode: never hang on an interactive password prompt; the panel
        // must fail visibly instead. Login-shell wrapper: docker commonly
        // lives outside the non-interactive PATH (found on the colima stand).
        let remote = format!("{} -lc {}", self.login_shell, shell_quote(cmd));
        let out = tokio::process::Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=8",
                &self.alias,
                &remote,
            ])
            .output()
            .await?;
        Ok(RemoteOutput {
            status: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

impl MockTransport {
    pub async fn exec(&self, cmd: &str) -> Result<RemoteOutput, Error> {
        for (needle, stdout) in &self.responses {
            if cmd.contains(needle) {
                return Ok(RemoteOutput {
                    status: Some(0),
                    stdout: stdout.clone(),
                    stderr: String::new(),
                });
            }
        }
        Ok(RemoteOutput {
            status: Some(127),
            stdout: String::new(),
            stderr: format!("mock: no canned response for: {cmd}"),
        })
    }
}

/// Canned transport for `--mock`: a healthy server with docker, the CLI
/// (already bootstrapped), one AmneziaWG migration candidate and one
/// noise container.
pub fn mock_canned() -> Transport {
    Transport::Mock(MockTransport {
        responses: vec![
            ("command -v docker".into(), "/usr/bin/docker\n".into()),
            ("docker version --format".into(), "29.5.2\n".into()),
            ("awg-mesh --version".into(), "awg-mesh 0.2.0\n".into()),
            ("command -v git".into(), "/usr/bin/git\n".into()),
            ("test -x".into(), "HAVE\n".into()),
            ("pull --ff-only".into(), "Already up to date.\n".into()),
            (
                "compose.project".into(),
                concat!(
                    r#"{"Command":"x","ID":"aa1111111111","Image":"amneziavpn/amneziawg-go:latest","Names":"amnezia-mesh-awg","State":"running","Status":"Up 3 days"}"#,
                    "\n",
                    r#"{"Command":"x","ID":"bb2222222222","Image":"amneziavpn/amnezia-dns:latest","Names":"amnezia-mesh-dns","State":"running","Status":"Up 3 days"}"#,
                    "\n",
                )
                .into(),
            ),
            (
                "awg show".into(),
                concat!(
                    "interface: awg0\n",
                    "  listening port: 51820\n",
                    "\n",
                    "peer: 9R0OeW1gigwJ8VsD1gZzDUWQaL/QV6N7+0cpO+4JXyE=\n",
                    "  endpoint: 203.0.113.7:51820\n",
                    "  allowed ips: 10.8.1.2/32\n",
                    "  latest handshake: 1 minute, 30 seconds ago\n",
                    "  transfer: 12.5 MiB received, 3.1 MiB sent\n",
                    "  persistent keepalive: every 25 seconds\n",
                    "\n",
                    "peer: k5lYeOjn7xDzCPYncLV/i+b0w7iIEFgwj8s8et5mcXI=\n",
                    "  preshared key: (hidden)\n",
                    "  allowed ips: 10.8.1.3/32\n",
                    "  persistent keepalive: 25\n",
                    "\n",
                    "peer: bam48L39KdmWsPazLFfwFXt+PpSNEI2sKgG/9nYXpxs=\n",
                    "  endpoint: 198.51.100.23:51820\n",
                    "  allowed ips: 10.8.1.4/32\n",
                    "  latest handshake: 5 hours ago\n",
                    "  transfer: 128 KiB received, 0 B sent\n",
                    "\n",
                )
                .into(),
            ),
            (
                "peer list".into(),
                r#"{"ok":true,"peers":[
                    {"name":"test-phone","ip":"10.8.1.2","pub":"9R0OeW1gigwJ8VsD1gZzDUWQaL/QV6N7+0cpO+4JXyE=","created":"2026-09-14T08:30:00Z"},
                    {"name":"peer-k5lyeo","ip":"10.8.1.3","pub":"k5lYeOjn7xDzCPYncLV/i+b0w7iIEFgwj8s8et5mcXI=","created":null},
                    {"name":"macbook-pro","ip":"10.8.1.4","pub":"bam48L39KdmWsPazLFfwFXt+PpSNEI2sKgG/9nYXpxs=","created":null}]}"#
                    .into(),
            ),
            (
                "doctor --json".into(),
                r#"{"ok":true,"checks":[
                    {"name":"docker-daemon","ok":true,"detail":"running"},
                    {"name":"initialized","ok":true,"detail":"state found"},
                    {"name":"awg-container","ok":true,"detail":"up"},
                    {"name":"dns-container","ok":true,"detail":"up"},
                    {"name":"listen-port","ok":true,"detail":"51820"},
                    {"name":"ip-forward","ok":true,"detail":"on"},
                    {"name":"magicdns","ok":true,"detail":"resolving"},
                    {"name":"port-published","ok":true,"detail":"udp published"}]}"#
                    .into(),
            ),
            (
                "amnezia-mesh-stats.log".into(),
                concat!(
                    "=== 2026-10-27T08:00:01+07:00 ===\n",
                    "peer: 9R0OeW1gigwJ8VsD1gZzDUWQaL/QV6N7+0cpO+4JXyE=\n",
                    "endpoint: 203.0.113.7:51820\n",
                    "latest handshake: 1 minute, 5 seconds ago\n",
                    "transfer: 12.0 MiB received, 3.0 MiB sent\n",
                    "peer: k5lYeOjn7xDzCPYncLV/i+b0w7iIEFgwj8s8et5mcXI=\n",
                    "latest handshake: off\n",
                    "peer: bam48L39KdmWsPazLFfwFXt+PpSNEI2sKgG/9nYXpxs=\n",
                    "endpoint: 198.51.100.23:51820\n",
                    "latest handshake: 2 hours, 10 minutes ago\n",
                    "transfer: 512.0 KiB received, 64.0 KiB sent\n",
                    "=== 2026-10-27T08:10:01+07:00 ===\n",
                    "=== 2026-10-27T08:10:01+07:00 ===\n",
                    "peer: 9R0OeW1gigwJ8VsD1gZzDUWQaL/QV6N7+0cpO+4JXyE=\n",
                    "endpoint: 203.0.113.7:51820\n",
                    "latest handshake: 11 minutes, 5 seconds ago\n",
                    "transfer: 13.0 MiB received, 3.2 MiB sent\n",
                    "peer: bam48L39KdmWsPazLFfwFXt+PpSNEI2sKgG/9nYXpxs=\n",
                    "latest handshake: 2 hours, 20 minutes ago\n",
                    "=== 2026-10-27T08:20:01+07:00 ===\n",
                    "peer: 9R0OeW1gigwJ8VsD1gZzDUWQaL/QV6N7+0cpO+4JXyE=\n",
                    "endpoint: 203.0.113.7:51820\n",
                    "latest handshake: 30 seconds ago\n",
                    "transfer: 14.5 MiB received, 3.6 MiB sent\n",
                    "peer: bam48L39KdmWsPazLFfwFXt+PpSNEI2sKgG/9nYXpxs=\n",
                    "latest handshake: 2 hours, 30 minutes ago\n",
                    "transfer: 520.0 KiB received, 64.0 KiB sent\n",
                )
                .into(),
            ),
            (
                "amnezia-mesh-watchdog.log".into(),
                concat!(
                    "2026-10-25T04:10:01+07:00 amnezia-mesh-awg down, restarting\n",
                    "amnezia-mesh-awg\n",
                    "2026-10-27T02:33:01+07:00 amnezia-mesh-awg wedged, restarting\n",
                    "amnezia-mesh-awg\n",
                )
                .into(),
            ),
            (
                "peer rename".into(),
                r#"{"ok":true,"renamed":"test-phone","name":"renamed-phone"}"#.into(),
            ),
            (
                "peer revoke".into(),
                r#"{"ok":true,"revoked":"peer-k5lyeo"}"#.into(),
            ),
            (
                "peer add".into(),
                r#"{"ok":true,"name":"new-phone","ip":"10.8.1.5",
                    "profiles":["/tmp/amnezia-mesh/config/peers/new-phone-exit.vpn",
                                "/tmp/amnezia-mesh/config/peers/new-phone-mesh.vpn"]}"#.into(),
            ),
            (
                "config/peers/new-phone".into(),
                "vpn://eyJmb28iOiJiYXIifQ-example-profile-link\n".into(),
            ),
            (
                "docker ps -a".into(),
                concat!(
                    r#"{"Command":"x","ID":"9d041a65c06e","Image":"amneziavpn/amneziawg-go:latest","Names":"amnezia-legacy-test","State":"running","Status":"Up 3 minutes"}"#,
                    "\n",
                    r#"{"Command":"x","ID":"e867f97f7085","Image":"nginx:alpine","Names":"web","State":"running","Status":"Up 2 hours"}"#,
                    "\n",
                )
                .into(),
            ),
            (
                "adopt --inspect".into(),
                r#"{"ok":true,"inspect":{
                    "source":"container 'amnezia-legacy-test'",
                    "image":"amneziavpn/amneziawg-go:latest",
                    "server_pub":"PUB","subnet":"10.8.1.0/24","mtu":"1280",
                    "listen_port":"51820","server_keepalive":"25",
                    "obfuscation_params":20,
                    "peers":[{"name":"test-phone","ip":"10.8.1.2",
                              "created":"2026-09-14T08:30:00Z"}],
                    "warnings":["peer xyz has no usable name comment"]}}"#
                    .into(),
            ),
        ],
    })
}

/// POSIX single-quote escaping for embedding `cmd` into the remote
/// "shell -lc '<cmd>'" invocation.
fn shell_quote(cmd: &str) -> String {
    format!("'{}'", cmd.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_wraps_and_escapes() {
        assert_eq!(shell_quote("docker ps -a"), "'docker ps -a'");
        assert_eq!(shell_quote("echo 'a b'"), "'echo '\\''a b'\\'''");
    }
}

#[cfg(test)]
mod for_server_tests {
    use super::*;

    #[test]
    fn local_alias_maps_to_local_transport() {
        let server = ServerConfig {
            ssh_alias: "local".into(),
            ..ServerConfig::default()
        };
        assert_eq!(Transport::for_server(&server), Transport::Local);
        let server = ServerConfig {
            ssh_alias: "vps".into(),
            login_shell: "bash".into(),
            ..ServerConfig::default()
        };
        assert!(matches!(Transport::for_server(&server), Transport::Ssh(_)));
    }
}
