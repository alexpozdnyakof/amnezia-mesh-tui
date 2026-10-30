//! Copy the Linux awg-mesh CLI onto a server over the system ssh client.
//!
//! The Homebrew cask stages `payload/linux_{amd64,arm64}/awg-mesh` next to
//! this binary. A dev build points `--payload-dir` at `build/payload` from
//! `amnezia-mesh-go/scripts/stage-mac-bundle.sh`. The command copies the
//! binary to `~/.local/bin/awg-mesh` and stops. `init` stays a separate step.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Terminal;

use crate::transport::shell_quote;

#[derive(Clone, Debug)]
pub struct InstallOpts {
    pub ssh_bin: PathBuf,
    pub scp_bin: PathBuf,
    pub payload_dir: Option<PathBuf>,
    pub target: String,
    pub identity: Option<PathBuf>,
}

impl InstallOpts {
    pub fn new(target: String) -> Self {
        Self {
            ssh_bin: PathBuf::from("ssh"),
            scp_bin: PathBuf::from("scp"),
            payload_dir: None,
            target,
            identity: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallOutcome {
    pub arch: String,
    pub remote_path: String,
    pub version: String,
}

pub fn check_target(ssh: &str) -> Result<&str, String> {
    let ssh = ssh.trim();
    if ssh.is_empty() || ssh.chars().any(|c| c.is_whitespace()) {
        return Err("ssh target must be an alias or user@host, without spaces".into());
    }
    Ok(ssh)
}

pub fn goarch_from_uname(uname: &str) -> Result<&'static str, String> {
    match last_line(uname) {
        "x86_64" | "amd64" => Ok("amd64"),
        "aarch64" | "arm64" => Ok("arm64"),
        other => Err(format!("unsupported server arch: {other}")),
    }
}

/// Toml key for the server entry written after a successful copy.
pub fn server_name(ssh: &str) -> String {
    let mut s: String = ssh
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-');
    let s: String = if s.is_empty() {
        "server".into()
    } else {
        s.chars().take(32).collect()
    };
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("srv-{s}")
    } else {
        s
    }
}

pub fn payload_path(
    exe: &Path,
    goarch: &str,
    payload_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    let path = if let Some(dir) = payload_dir {
        dir.join(format!("linux_{goarch}/awg-mesh"))
    } else {
        let staged = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
        let parent = staged
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", staged.display()))?;
        parent.join(format!("payload/linux_{goarch}/awg-mesh"))
    };
    if !path.is_file() {
        return Err(format!(
            "linux {goarch} payload not found at {}. The Homebrew cask puts it next to awg-mesh-tui; a dev build needs --payload-dir (amnezia-mesh-go/build/payload after scripts/stage-mac-bundle.sh)",
            path.display()
        ));
    }
    Ok(path)
}

pub fn remote_bin(home: &str) -> Result<String, String> {
    let home = last_line(home);
    if home.is_empty() || !home.starts_with('/') || home.contains('\n') {
        return Err(format!("unexpected remote HOME: {home:?}"));
    }
    Ok(format!("{home}/.local/bin/awg-mesh"))
}

pub async fn install_server(opts: &InstallOpts) -> Result<InstallOutcome, String> {
    let target = check_target(&opts.target)?.to_string();
    let opts = InstallOpts {
        target,
        ..opts.clone()
    };

    let uname = ssh(&opts, "uname -m").await?;
    let arch = goarch_from_uname(&uname.stdout)?.to_string();
    let home = ssh(&opts, r#"printf %s "$HOME""#).await?;
    let remote_path = remote_bin(&home.stdout)?;
    let remote_dir = remote_path
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .ok_or_else(|| format!("bad remote path {remote_path}"))?;

    let exe = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
    let payload = payload_path(&exe, &arch, opts.payload_dir.as_deref())?;

    ssh(&opts, &format!("mkdir -p {}", shell_quote(remote_dir))).await?;
    scp(&opts, &payload, &remote_path).await?;
    let version = ssh(
        &opts,
        &format!(
            "chmod 755 {} && {} --version",
            shell_quote(&remote_path),
            shell_quote(&remote_path)
        ),
    )
    .await?;
    let version = last_line(&version.stdout).to_string();
    if version.is_empty() {
        return Err("remote awg-mesh --version printed nothing".into());
    }
    Ok(InstallOutcome {
        arch,
        remote_path,
        version,
    })
}

/// One-field form. Restores the terminal before returning.
pub fn prompt_ssh_target() -> Result<String, String> {
    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = io::stdout();
    if stdout.execute(EnterAlternateScreen).is_err() {
        let _ = disable_raw_mode();
        return Err("cannot open the install form".into());
    }
    let _guard = TermGuard;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;
    let mut input = String::new();
    let mut error = String::new();
    loop {
        terminal
            .draw(|f| draw_form(f, &input, &error))
            .map_err(|e| e.to_string())?;
        if !event::poll(Duration::from_millis(250)).map_err(|e| e.to_string())? {
            continue;
        }
        let Event::Key(key) = event::read().map_err(|e| e.to_string())? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Enter => {
                let t = input.trim().to_string();
                match check_target(&t) {
                    Ok(s) => return Ok(s.to_string()),
                    Err(e) => error = e,
                }
            }
            KeyCode::Esc => return Err("cancelled".into()),
            KeyCode::Backspace => {
                input.pop();
                error.clear();
            }
            KeyCode::Char(c) if !c.is_control() && input.len() < 200 => {
                input.push(c);
                error.clear();
            }
            _ => {}
        }
    }
}

struct TermGuard;

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

fn draw_form(f: &mut ratatui::Frame, input: &str, error: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(f.area());
    let field = Paragraph::new(format!(" {input}"))
        .block(Block::default().title(" SSH target ").borders(Borders::ALL));
    f.render_widget(field, chunks[0]);
    let mut help = "alias from ~/.ssh/config, or user@host\nEnter copies the Linux CLI to ~/.local/bin  ·  Esc cancels".to_string();
    if !error.is_empty() {
        help = format!("{error}\n{help}");
    }
    let style = if error.is_empty() {
        Style::new().fg(Color::DarkGray)
    } else {
        Style::new().fg(Color::Red)
    };
    f.render_widget(
        Paragraph::new(help).style(style).wrap(Wrap { trim: false }),
        chunks[1],
    );
}

struct CmdOut {
    stdout: String,
}

async fn ssh(opts: &InstallOpts, remote: &str) -> Result<CmdOut, String> {
    let mut cmd = tokio::process::Command::new(&opts.ssh_bin);
    cmd.stdin(std::process::Stdio::null());
    push_ssh_opts(&mut cmd, opts);
    cmd.arg(&opts.target);
    cmd.arg(remote);
    run_cmd(&mut cmd, "ssh", &opts.target).await
}

async fn scp(opts: &InstallOpts, src: &Path, remote_path: &str) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new(&opts.scp_bin);
    cmd.stdin(std::process::Stdio::null());
    push_ssh_opts(&mut cmd, opts);
    cmd.arg(src);
    cmd.arg(format!("{}:{remote_path}", opts.target));
    run_cmd(&mut cmd, "scp", &opts.target).await?;
    Ok(())
}

fn push_ssh_opts(cmd: &mut tokio::process::Command, opts: &InstallOpts) {
    cmd.arg("-o").arg("BatchMode=yes");
    cmd.arg("-o").arg("ConnectTimeout=8");
    if let Some(id) = &opts.identity {
        cmd.arg("-i").arg(id);
    }
}

async fn run_cmd(
    cmd: &mut tokio::process::Command,
    bin: &str,
    target: &str,
) -> Result<CmdOut, String> {
    let out = cmd.output().await.map_err(|e| format!("{bin}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if out.status.success() {
        return Ok(CmdOut { stdout });
    }
    let detail = tail(&stderr);
    if detail.contains("Host key verification failed") {
        return Err(format!(
            "ssh to {target} failed: the host key is not in known_hosts. Connect once with ssh so you can confirm the key, then retry. {detail}"
        ));
    }
    Err(format!("{bin} to {target} failed: {detail}"))
}

fn last_line(s: &str) -> &str {
    s.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(str::trim)
        .unwrap_or("")
}

fn tail(s: &str) -> &str {
    let s = s.trim();
    if s.len() <= 500 {
        s
    } else {
        &s[s.len() - 500..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn arch_and_names() {
        assert_eq!(goarch_from_uname("x86_64\n").unwrap(), "amd64");
        assert_eq!(goarch_from_uname("motd\naarch64\n").unwrap(), "arm64");
        assert!(goarch_from_uname("riscv64").is_err());
        assert_eq!(server_name("root@203.0.113.7"), "root-203-0-113-7");
        assert_eq!(server_name("vdsina_awg"), "vdsina_awg");
        assert_eq!(server_name("1host"), "srv-1host");
        assert_eq!(server_name("@@@"), "server");
        assert_eq!(remote_bin("/root\n").unwrap(), "/root/.local/bin/awg-mesh");
        assert!(remote_bin("relative").is_err());
        assert!(check_target("user@host").is_ok());
        assert!(check_target("has space").is_err());
    }

    #[test]
    fn payload_follows_symlink_to_the_staged_dir() {
        let dir = testdir();
        let staged = dir.join("staged");
        fs::create_dir_all(staged.join("payload/linux_amd64")).unwrap();
        fs::write(staged.join("payload/linux_amd64/awg-mesh"), b"bin").unwrap();
        fs::write(staged.join("awg-mesh-tui"), b"tui").unwrap();
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(staged.join("awg-mesh-tui"), bin.join("awg-mesh-tui")).unwrap();
        let found = payload_path(&bin.join("awg-mesh-tui"), "amd64", None).unwrap();
        assert_eq!(fs::read(found).unwrap(), b"bin");
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn install_copies_the_payload_over_ssh() {
        let dir = testdir();
        let home = dir.join("remote-home");
        let payload = dir.join("payload");
        fs::create_dir_all(payload.join("linux_amd64")).unwrap();
        fs::write(payload.join("linux_amd64/awg-mesh"), b"linux-binary").unwrap();
        let ssh_bin = dir.join("ssh");
        let scp_bin = dir.join("scp");
        write_exec(
            &ssh_bin,
            &format!(
                r#"#!/bin/sh
cmd=
for a in "$@"; do cmd=$a; done
case "$cmd" in
  "uname -m") echo x86_64 ;;
  'printf %s "$HOME"') printf '%s' '{home}' ;;
  mkdir\ *) exit 0 ;;
  chmod\ *) echo "awg-mesh test" ;;
  *) echo "unexpected: $cmd" >&2; exit 99 ;;
esac
"#,
                home = home.display()
            ),
        );
        write_exec(
            &scp_bin,
            r#"#!/bin/sh
skip=0
src=
dest=
for a in "$@"; do
  if [ "$skip" = 1 ]; then skip=0; continue; fi
  case "$a" in
    -o|-i) skip=1 ;;
    -*) ;;
    *) src=$dest; dest=$a ;;
  esac
done
path=${dest#*:}
mkdir -p "$(dirname "$path")"
cp "$src" "$path"
"#,
        );
        let outcome = install_server(&InstallOpts {
            ssh_bin,
            scp_bin,
            payload_dir: Some(payload),
            target: "root@example".into(),
            identity: None,
        })
        .await
        .unwrap();
        assert_eq!(outcome.arch, "amd64");
        assert_eq!(outcome.version, "awg-mesh test");
        assert_eq!(
            outcome.remote_path,
            format!("{}/.local/bin/awg-mesh", home.display())
        );
        assert_eq!(fs::read(&outcome.remote_path).unwrap(), b"linux-binary");
        let _ = fs::remove_dir_all(&dir);
    }

    fn testdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "awg-install-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_exec(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }
}
