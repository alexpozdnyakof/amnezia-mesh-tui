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

#[derive(Clone, Debug)]
pub enum Transport {
    Ssh(SshTransport),
    Mock(MockTransport),
}

#[derive(Clone, Debug)]
pub struct SshTransport {
    pub alias: String,
    pub login_shell: String,
}

#[derive(Clone, Debug)]
pub struct MockTransport {
    /// Canned stdout per command substring; first match wins.
    pub responses: Vec<(String, String)>,
}

impl Transport {
    pub fn for_server(server: &ServerConfig) -> Self {
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

/// Canned transport for `--mock`: a healthy server with docker and the CLI.
pub fn mock_canned() -> Transport {
    Transport::Mock(MockTransport {
        responses: vec![
            ("command -v docker".into(), "/usr/bin/docker\n".into()),
            ("docker version --format".into(), "29.5.2\n".into()),
            ("awg-mesh --version".into(), "awg-mesh 0.2.0\n".into()),
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
