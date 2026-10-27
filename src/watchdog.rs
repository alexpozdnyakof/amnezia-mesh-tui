/// Parser for `/var/log/amnezia-mesh-watchdog.log`
/// (`scripts/awg-watchdog.sh`): restart events
/// `<ISO8601> <container> down, restarting` / `wedged, restarting`.
///
/// The script also appends raw `docker start/restart` output to the log —
/// those lines are filtered out.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogKind {
    Down,
    Wedged,
}

impl WatchdogKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            WatchdogKind::Down => "down",
            WatchdogKind::Wedged => "wedged",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WatchdogEvent {
    /// Raw ISO8601 timestamp.
    pub at: String,
    pub container: String,
    pub kind: WatchdogKind,
}

pub fn parse_watchdog_log(text: &str) -> Vec<WatchdogEvent> {
    text.lines().filter_map(parse_event_line).collect()
}

fn parse_event_line(line: &str) -> Option<WatchdogEvent> {
    let (ts, rest) = line.split_once(' ')?;
    // crude ISO8601 guard: date -Is looks like 2026-10-26T20:00:01+07:00
    if ts.len() < 15 || !ts.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let (container, tail) = rest.split_once(' ')?;
    if !tail.ends_with(", restarting") {
        return None;
    }
    let kind = if tail.starts_with("down") {
        WatchdogKind::Down
    } else if tail.starts_with("wedged") {
        WatchdogKind::Wedged
    } else {
        return None;
    };
    Some(WatchdogEvent {
        at: ts.to_string(),
        container: container.to_string(),
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_and_skips_command_output() {
        let text = "\
2026-10-20T12:00:01+03:00 amnezia-mesh-awg down, restarting
amnezia-mesh-awg
2026-10-21T13:00:01+03:00 amnezia-mesh-awg wedged, restarting
amnezia-mesh-awg
random noise
2026-10-22T14:00:01+03:00 other-container down, restarting
";
        let evs = parse_watchdog_log(text);
        assert_eq!(evs.len(), 3);
        assert_eq!(evs[0].kind, WatchdogKind::Down);
        assert_eq!(evs[0].container, "amnezia-mesh-awg");
        assert_eq!(evs[0].at, "2026-10-20T12:00:01+03:00");
        assert_eq!(evs[1].kind, WatchdogKind::Wedged);
        assert_eq!(evs[2].container, "other-container");
    }

    #[test]
    fn empty_input_yields_no_events() {
        assert!(parse_watchdog_log("").is_empty());
    }
}
