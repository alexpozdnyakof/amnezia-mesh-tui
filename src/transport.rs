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
                let out = local_command(cmd).output().await?;
                Ok(RemoteOutput {
                    status: out.status.code(),
                    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                })
            }
        }
    }

    /// Same as `exec`, but each stdout/stderr line is handed to `on_line`
    /// as it arrives. Used by init/adopt, which run for minutes. Stdin is
    /// closed so a remote prompt cannot eat TUI keypresses.
    pub async fn exec_lines(
        &self,
        cmd: &str,
        on_line: impl FnMut(String),
    ) -> Result<RemoteOutput, Error> {
        match self {
            Transport::Ssh(t) => stream_child(t.command(cmd), on_line).await,
            Transport::Local => stream_child(local_command(cmd), on_line).await,
            Transport::Mock(t) => t.exec_lines(cmd, on_line).await,
        }
    }
}

fn local_command(cmd: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("sh");
    command.arg("-c").arg(cmd);
    command
}

async fn stream_child(
    mut command: tokio::process::Command,
    mut on_line: impl FnMut(String),
) -> Result<RemoteOutput, Error> {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let mut out_lines = BufReader::new(stdout).lines();
    let mut err_lines = BufReader::new(stderr).lines();
    let mut out_open = true;
    let mut err_open = true;
    let mut buf = String::new();
    while out_open || err_open {
        tokio::select! {
            biased;
            line = out_lines.next_line(), if out_open => {
                match line {
                    Ok(Some(l)) => {
                        on_line(l.clone());
                        buf.push_str(&l);
                        buf.push('\n');
                    }
                    Ok(None) => out_open = false,
                    Err(e) => {
                        on_line(format!("(read error: {e})"));
                        out_open = false;
                    }
                }
            }
            line = err_lines.next_line(), if err_open => {
                match line {
                    Ok(Some(l)) => {
                        on_line(l.clone());
                        buf.push_str(&l);
                        buf.push('\n');
                    }
                    Ok(None) => err_open = false,
                    Err(e) => {
                        on_line(format!("(read error: {e})"));
                        err_open = false;
                    }
                }
            }
        }
    }
    let status = child.wait().await?;
    Ok(RemoteOutput {
        status: status.code(),
        stdout: buf,
        stderr: String::new(),
    })
}

impl SshTransport {
    fn command(&self, cmd: &str) -> tokio::process::Command {
        // BatchMode: never hang on an interactive password prompt; the panel
        // must fail visibly instead. Login-shell wrapper: docker commonly
        // lives outside the non-interactive PATH (found on the colima stand).
        let remote = format!("{} -lc {}", self.login_shell, shell_quote(cmd));
        let mut command = tokio::process::Command::new("ssh");
        command.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            &self.alias,
            &remote,
        ]);
        command
    }

    pub async fn exec(&self, cmd: &str) -> Result<RemoteOutput, Error> {
        let out = self.command(cmd).output().await?;
        Ok(RemoteOutput {
            status: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

impl MockTransport {
    fn canned<'a>(&'a self, cmd: &str) -> Option<&'a str> {
        self.responses
            .iter()
            .find(|(needle, _)| cmd.contains(needle))
            .map(|(_, stdout)| stdout.as_str())
    }

    pub async fn exec(&self, cmd: &str) -> Result<RemoteOutput, Error> {
        match self.canned(cmd) {
            Some(stdout) => Ok(RemoteOutput {
                status: Some(0),
                stdout: stdout.to_string(),
                stderr: String::new(),
            }),
            None => Ok(RemoteOutput {
                status: Some(127),
                stdout: String::new(),
                stderr: format!("mock: no canned response for: {cmd}"),
            }),
        }
    }

    /// A canned body may start with `__EXIT__:N\n` to simulate a non-zero
    /// status. `exec` leaves that prefix intact; only the streaming path
    /// (init/adopt) interprets it.
    pub async fn exec_lines(
        &self,
        cmd: &str,
        mut on_line: impl FnMut(String),
    ) -> Result<RemoteOutput, Error> {
        let mut out = self.exec(cmd).await?;
        if out.status != Some(0) {
            return Ok(out);
        }
        let raw = std::mem::take(&mut out.stdout);
        let (status, body) = split_status_prefix(&raw);
        if let Some(code) = status {
            out.status = Some(code);
        }
        for line in body.lines() {
            on_line(line.to_string());
        }
        out.stdout = body.to_string();
        Ok(out)
    }
}

fn split_status_prefix(stdout: &str) -> (Option<i32>, &str) {
    let Some(rest) = stdout.strip_prefix("__EXIT__:") else {
        return (None, stdout);
    };
    let Some((code, body)) = rest.split_once('\n') else {
        return (None, stdout);
    };
    match code.parse::<i32>() {
        Ok(n) => (Some(n), body),
        Err(_) => (None, stdout),
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
            ("ifconfig.me".into(), "203.0.113.10\n".into()),
            (
                " init --host ".into(),
                "pulling image amneziavpn/amneziawg-go:latest ...\namnezia-mesh is up.\n".into(),
            ),
            (
                " adopt --container ".into(),
                "stopping 'amnezia-legacy-test' (kept stopped for rollback — do not remove) ...\nadopt complete: 3 peers carried over, IPs and keys unchanged\n".into(),
            ),
        ],
    })
}

/// POSIX single-quote escaping for embedding `cmd` into the remote
/// "shell -lc '<cmd>'" invocation.
pub(crate) fn shell_quote(cmd: &str) -> String {
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

    #[tokio::test]
    async fn exec_lines_strips_exit_prefix_and_emits_lines() {
        let t = Transport::Mock(MockTransport {
            responses: vec![(
                "init --host".into(),
                "__EXIT__:1\nfirst\nrolled back, x is running again\n".into(),
            )],
        });
        let mut lines = Vec::new();
        let out = t
            .exec_lines("awg-mesh init --host 1", |l| lines.push(l))
            .await
            .unwrap();
        assert_eq!(out.status, Some(1));
        assert_eq!(lines, vec!["first", "rolled back, x is running again"]);
        assert!(!out.stdout.contains("__EXIT__"));
    }

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
