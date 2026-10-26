use crate::transport::Transport;

pub const BOOTSTRAP_DIR: &str = "$HOME/amnezia-mesh";
pub const BOOTSTRAP_REPO: &str = "https://github.com/alexpozdnyakof/amnezia-mesh";

#[derive(Debug, Clone)]
pub struct BootstrapResult {
    /// cli_path to record in the server config.
    pub cli_path: String,
    pub version: String,
    pub updated: bool,
}

/// Install (or update) the amnezia-mesh CLI on the server by cloning the
/// public repo. Mutating — the UI asks for confirmation before calling.
pub async fn bootstrap(transport: &Transport) -> Result<BootstrapResult, String> {
    let git = transport
        .exec("command -v git")
        .await
        .map_err(|e| e.to_string())?;
    if !git.ok() || git.stdout_trimmed().is_empty() {
        return Err("git not found on the server; install git first".into());
    }

    let cli_path = format!("{BOOTSTRAP_DIR}/bin/awg-mesh");
    let present = transport
        .exec(&format!("test -x {cli_path} && echo HAVE || echo MISSING"))
        .await
        .map_err(|e| e.to_string())?;
    let updated = present.ok() && present.stdout_trimmed() == "HAVE";

    let git_cmd = if updated {
        format!("git -C {BOOTSTRAP_DIR} pull --ff-only")
    } else {
        format!("git clone --depth 1 {BOOTSTRAP_REPO} {BOOTSTRAP_DIR}")
    };
    let out = transport.exec(&git_cmd).await.map_err(|e| e.to_string())?;
    if !out.ok() {
        let tail = out
            .stdout
            .lines()
            .chain(out.stderr.lines())
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or("git failed");
        return Err(format!("{git_cmd} failed: {tail}"));
    }

    let ver = transport
        .exec(&format!("{cli_path} --version"))
        .await
        .map_err(|e| e.to_string())?;
    if !ver.ok() {
        return Err(format!(
            "cloned, but '{cli_path} --version' failed: {}",
            ver.stderr.trim()
        ));
    }
    Ok(BootstrapResult {
        cli_path,
        version: ver.stdout_trimmed().to_string(),
        updated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    #[tokio::test]
    async fn bootstrap_clones_and_reports_version() {
        let t = Transport::Mock(MockTransport {
            responses: vec![
                ("command -v git".into(), "/usr/bin/git\n".into()),
                ("test -x".into(), "MISSING\n".into()),
                ("git clone".into(), "Cloning into ...\n".into()),
                ("awg-mesh --version".into(), "awg-mesh 0.2.0\n".into()),
            ],
        });
        let r = bootstrap(&t).await.unwrap();
        assert_eq!(r.cli_path, "$HOME/amnezia-mesh/bin/awg-mesh");
        assert_eq!(r.version, "awg-mesh 0.2.0");
        assert!(!r.updated);
    }

    #[tokio::test]
    async fn bootstrap_requires_git() {
        let t = Transport::Mock(MockTransport { responses: vec![] });
        let err = bootstrap(&t).await.unwrap_err();
        assert!(err.contains("git not found"));
    }
}
