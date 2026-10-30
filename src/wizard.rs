//! Init / adopt setup form. The panel confirms here, then runs the CLI in
//! human mode so `say()` progress stays visible (`--json` silences it).
//! `PYTHONUNBUFFERED` keeps the Python CLI from block-buffering stderr
//! when ssh has no tty; the Go binary ignores that variable.

use crate::transport::shell_quote;

/// Public-address hint, executed on the server. Failure is a quiet no-op.
pub const DETECT_HOST_CMD: &str = "curl -4 -fsS --max-time 4 https://ifconfig.me || curl -4 -fsS --max-time 4 https://api.ipify.org";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldId {
    Host,
    Port,
    Subnet,
    Domain,
    Image,
    Dns1,
    Dns2,
    RdpReject,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldKind {
    Text,
    Toggle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub id: FieldId,
    pub label: &'static str,
    pub value: String,
    pub kind: FieldKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Init,
    Adopt { container: String },
}

impl Kind {
    pub fn title(&self) -> String {
        match self {
            Kind::Init => "init — new server".to_string(),
            Kind::Adopt { container } => format!("adopt — {container}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    pub kind: Kind,
    pub fields: Vec<Field>,
    pub selected: usize,
    pub editing: bool,
    pub confirm_pending: bool,
    /// `h` overwrites host; the automatic detect on open does not.
    pub detect_force: bool,
    host_edited: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RunOutcome {
    Success,
    RolledBack,
    Failed,
}

impl Form {
    pub fn init() -> Self {
        Self::new(
            Kind::Init,
            vec![
                text(FieldId::Host, "host", ""),
                text(FieldId::Port, "port", "443"),
                text(FieldId::Subnet, "subnet", "100.64.0.0/24"),
                text(FieldId::Domain, "domain", "awg"),
                text(FieldId::Image, "image", "amneziavpn/amneziawg-go:latest"),
                toggle(FieldId::RdpReject, "rdp-reject", false),
            ],
        )
    }

    pub fn adopt(container: String) -> Self {
        Self::new(
            Kind::Adopt { container },
            vec![
                text(FieldId::Host, "host", ""),
                text(FieldId::Port, "port", "443"),
                text(FieldId::Domain, "domain", "awg"),
                text(FieldId::Dns1, "dns1", "1.1.1.1"),
                text(FieldId::Dns2, "dns2", ""),
                toggle(FieldId::RdpReject, "rdp-reject", false),
            ],
        )
    }

    fn new(kind: Kind, fields: Vec<Field>) -> Self {
        Self {
            kind,
            fields,
            selected: 0,
            editing: false,
            confirm_pending: false,
            detect_force: false,
            host_edited: false,
            error: None,
        }
    }

    pub fn confirm_hint(&self) -> String {
        match &self.kind {
            Kind::Init => {
                "init pulls the image, generates keys, and starts the stack. press r again to run."
                    .to_string()
            }
            Kind::Adopt { container } => format!(
                "adopt stops '{container}' (kept for rollback) and starts amnezia-mesh. press r again to run."
            ),
        }
    }

    pub fn move_sel(&mut self, delta: isize) {
        let n = self.fields.len() as isize;
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).clamp(0, n - 1) as usize;
        self.editing = false;
    }

    pub fn activate(&mut self) {
        let Some(field) = self.fields.get_mut(self.selected) else {
            return;
        };
        self.confirm_pending = false;
        self.error = None;
        if field.kind == FieldKind::Toggle {
            field.value = if field.value == "on" {
                "off".to_string()
            } else {
                "on".to_string()
            };
        } else {
            self.editing = true;
        }
    }

    pub fn push_char(&mut self, c: char) {
        let id = match self.fields.get(self.selected) {
            Some(field) if field.kind == FieldKind::Text && field_accepts(field.id, c) => field.id,
            _ => return,
        };
        let field = &mut self.fields[self.selected];
        if field.value.len() >= field_limit(id) {
            return;
        }
        field.value.push(c);
        if id == FieldId::Host {
            self.host_edited = true;
        }
        self.confirm_pending = false;
        self.error = None;
    }

    pub fn backspace(&mut self) {
        let Some(field) = self.fields.get_mut(self.selected) else {
            return;
        };
        if field.kind != FieldKind::Text {
            return;
        }
        let id = field.id;
        field.value.pop();
        if id == FieldId::Host {
            self.host_edited = true;
        }
        self.confirm_pending = false;
    }

    #[cfg(test)]
    pub fn set_field(&mut self, id: FieldId, value: &str) {
        if let Some(field) = self.fields.iter_mut().find(|f| f.id == id) {
            field.value = value.to_string();
        }
    }

    /// Fill an empty host from a detect command. A value the user typed stays.
    pub fn apply_detected_host(&mut self, raw: &str) -> bool {
        if self.host_edited {
            return false;
        }
        self.write_host(raw, false)
    }

    pub fn force_host(&mut self, raw: &str) -> bool {
        self.write_host(raw, true)
    }

    fn write_host(&mut self, raw: &str, force: bool) -> bool {
        let Some(host) = sanitize_public_host(raw) else {
            return false;
        };
        let Some(field) = self.fields.iter_mut().find(|f| f.id == FieldId::Host) else {
            return false;
        };
        if !force && !field.value.is_empty() {
            return false;
        }
        field.value = host;
        self.host_edited = false;
        self.error = None;
        true
    }

    fn field_value(&self, id: FieldId) -> &str {
        self.fields
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.value.as_str())
            .unwrap_or("")
    }

    fn flag_on(&self, id: FieldId) -> bool {
        self.field_value(id) == "on"
    }
}

pub fn sanitize_public_host(raw: &str) -> Option<String> {
    let host = raw.trim();
    valid_host(host).then(|| host.to_string())
}

/// Shell command for `init` or `adopt`. Human mode, non-interactive, quoted args.
pub fn build_command(cli: &str, form: &Form) -> Result<String, String> {
    if !quotable(cli) {
        return Err("cli path is empty or contains control characters".into());
    }
    let host = form.field_value(FieldId::Host);
    if !valid_host(host) {
        return Err("host is required (public IP or domain)".into());
    }
    let port = form.field_value(FieldId::Port);
    if !valid_port(port) {
        return Err("port must be 1-65535".into());
    }
    let domain = form.field_value(FieldId::Domain);
    if !valid_domain(domain) {
        return Err("domain: lowercase letters, digits, dot, dash".into());
    }

    let mut args = vec![
        "PYTHONUNBUFFERED=1".to_string(),
        "AWG_MESH_NONINTERACTIVE=1".to_string(),
        shell_quote(cli),
    ];
    match &form.kind {
        Kind::Init => {
            let subnet = form.field_value(FieldId::Subnet);
            if !valid_subnet(subnet) {
                return Err("subnet must look like 100.64.0.0/24".into());
            }
            let image = form.field_value(FieldId::Image);
            if !valid_image(image) {
                return Err("image must be a docker reference".into());
            }
            args.push("init".into());
            push_opt(&mut args, "host", host);
            push_opt(&mut args, "port", port);
            push_opt(&mut args, "subnet", subnet);
            push_opt(&mut args, "domain", domain);
            push_opt(&mut args, "image", image);
            if form.flag_on(FieldId::RdpReject) {
                args.push("--rdp-reject".into());
            }
        }
        Kind::Adopt { container } => {
            if !valid_container(container) {
                return Err("container name has unsupported characters".into());
            }
            let dns1 = form.field_value(FieldId::Dns1);
            if !valid_dns(dns1) {
                return Err("dns1 must be an IP address".into());
            }
            let dns2 = form.field_value(FieldId::Dns2);
            if !dns2.is_empty() && !valid_dns(dns2) {
                return Err("dns2 must be an IP address".into());
            }
            args.push("adopt".into());
            push_opt(&mut args, "container", container);
            push_opt(&mut args, "host", host);
            push_opt(&mut args, "port", port);
            push_opt(&mut args, "domain", domain);
            push_opt(&mut args, "dns1", dns1);
            if !dns2.is_empty() {
                push_opt(&mut args, "dns2", dns2);
            }
            if form.flag_on(FieldId::RdpReject) {
                args.push("--rdp-reject".into());
            }
        }
    }
    Ok(args.join(" "))
}

/// Exit status wins. Non-zero output that names the CLI's own rollback is
/// `RolledBack`; the old container is running again and nothing was deleted.
pub fn classify(status: Option<i32>, log: &str) -> RunOutcome {
    if status == Some(0) {
        return RunOutcome::Success;
    }
    let lower = log.to_ascii_lowercase();
    if lower.contains("rolled back") || lower.contains("started back") {
        RunOutcome::RolledBack
    } else {
        RunOutcome::Failed
    }
}

fn text(id: FieldId, label: &'static str, value: &str) -> Field {
    Field {
        id,
        label,
        value: value.to_string(),
        kind: FieldKind::Text,
    }
}

fn toggle(id: FieldId, label: &'static str, on: bool) -> Field {
    Field {
        id,
        label,
        value: if on { "on" } else { "off" }.to_string(),
        kind: FieldKind::Toggle,
    }
}

fn push_opt(args: &mut Vec<String>, name: &str, value: &str) {
    args.push(format!("--{name}"));
    args.push(shell_quote(value));
}

fn quotable(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c.is_control())
}

fn valid_host(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-'))
}

fn valid_port(s: &str) -> bool {
    matches!(s.parse::<u16>(), Ok(p) if p > 0)
}

fn valid_subnet(s: &str) -> bool {
    let Some((addr, pref)) = s.split_once('/') else {
        return false;
    };
    valid_host(addr) && matches!(pref.parse::<u8>(), Ok(n) if n <= 128)
}

fn valid_domain(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-'))
}

fn valid_image(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | ':' | '-' | '_' | '@'))
}

fn valid_dns(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() || matches!(c, '.' | ':'))
}

fn valid_container(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn field_accepts(id: FieldId, c: char) -> bool {
    match id {
        FieldId::Host => c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-'),
        FieldId::Port => c.is_ascii_digit(),
        FieldId::Subnet => c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '/'),
        FieldId::Domain => c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-'),
        FieldId::Image => {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | ':' | '-' | '_' | '@')
        }
        FieldId::Dns1 | FieldId::Dns2 => c.is_ascii_hexdigit() || matches!(c, '.' | ':'),
        FieldId::RdpReject => false,
    }
}

fn field_limit(id: FieldId) -> usize {
    match id {
        FieldId::Host => 253,
        FieldId::Port => 5,
        FieldId::Subnet | FieldId::Domain | FieldId::Dns1 | FieldId::Dns2 => 64,
        FieldId::Image => 200,
        FieldId::RdpReject => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_form() -> Form {
        let mut form = Form::init();
        form.set_field(FieldId::Host, "203.0.113.10");
        form
    }

    #[test]
    fn init_command_is_human_mode_and_quoted() {
        let cmd = build_command("/root/amnezia-mesh/bin/awg-mesh", &init_form()).unwrap();
        assert!(cmd.starts_with(
            "PYTHONUNBUFFERED=1 AWG_MESH_NONINTERACTIVE=1 '/root/amnezia-mesh/bin/awg-mesh' init "
        ));
        assert!(cmd.contains("--host '203.0.113.10'"));
        assert!(cmd.contains("--port '443'"));
        assert!(cmd.contains("--subnet '100.64.0.0/24'"));
        assert!(cmd.contains("--image 'amneziavpn/amneziawg-go:latest'"));
        assert!(!cmd.contains("--json"));
        assert!(!cmd.contains("--rdp-reject"));
    }

    #[test]
    fn init_rdp_reject_is_a_flag() {
        let mut form = init_form();
        form.set_field(FieldId::RdpReject, "on");
        let cmd = build_command("awg-mesh", &form).unwrap();
        assert!(cmd.ends_with(" --rdp-reject"));
    }

    #[test]
    fn adopt_command_carries_container_and_skips_empty_dns2() {
        let mut form = Form::adopt("amnezia-legacy-test".into());
        form.set_field(FieldId::Host, "203.0.113.10");
        let cmd = build_command("awg-mesh", &form).unwrap();
        assert!(cmd.contains(" adopt --container 'amnezia-legacy-test' "));
        assert!(cmd.contains("--dns1 '1.1.1.1'"));
        assert!(!cmd.contains("--dns2"));
        assert!(!cmd.contains("--inspect"));
        assert!(!cmd.contains("--json"));
    }

    #[test]
    fn adopt_rejects_a_container_name_with_spaces() {
        let mut form = Form::adopt("bad name".into());
        form.set_field(FieldId::Host, "203.0.113.10");
        let err = build_command("awg-mesh", &form).unwrap_err();
        assert!(err.contains("container"));
    }

    #[test]
    fn empty_host_is_rejected() {
        let err = build_command("awg-mesh", &Form::init()).unwrap_err();
        assert!(err.contains("host"));
    }

    #[test]
    fn shell_metacharacters_never_reach_the_command() {
        let mut form = init_form();
        form.set_field(FieldId::Host, "203.0.113.10;reboot");
        assert!(build_command("awg-mesh", &form).is_err());
    }

    #[test]
    fn classify_matches_cli_rollback_wording() {
        let rolled = "amnezia-mesh: error: post-adopt checks failed: listen-port: closed — rolled back, 'amnezia-awg' is running again, nothing deleted";
        assert_eq!(classify(Some(1), rolled), RunOutcome::RolledBack);
        let started = "docker compose up failed: boom; 'old' was started back, nothing deleted";
        assert_eq!(classify(Some(1), started), RunOutcome::RolledBack);
        assert_eq!(
            classify(Some(1), "amnezia-mesh: error: --host is required"),
            RunOutcome::Failed
        );
        assert_eq!(
            classify(Some(0), "rolled back but exit 0"),
            RunOutcome::Success
        );
    }

    #[test]
    fn detect_fills_empty_host_and_leaves_a_typed_one() {
        let mut form = Form::init();
        assert!(form.apply_detected_host("  203.0.113.10\n"));
        assert_eq!(form.field_value(FieldId::Host), "203.0.113.10");
        form.push_char('9');
        assert!(!form.apply_detected_host("198.51.100.1"));
        assert!(form.field_value(FieldId::Host).ends_with('9'));
        assert!(form.force_host("198.51.100.1"));
        assert_eq!(form.field_value(FieldId::Host), "198.51.100.1");
        assert!(sanitize_public_host("<html>").is_none());
    }
}
