use std::time::Duration;

/// One peer block of `awg show` human output (AmneziaWG userspace).
///
/// Quirks confirmed on the live stand: peers without a handshake have no
/// `latest handshake:`, `transfer:` or `endpoint:` lines at all; the
/// interface block carries no public key and extra amnezia lines
/// (`random trailers`, `disable cookies`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShowPeer {
    pub pubkey: String,
    /// `None` when the line is absent or shows `(none)`.
    pub endpoint: Option<String>,
    pub allowed_ips: Vec<String>,
    /// Age of the latest handshake; `None` = never ("off", "0" or no line).
    pub handshake: Option<Duration>,
    pub rx: Option<u64>,
    pub tx: Option<u64>,
    pub keepalive: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShowInfo {
    pub interface: String,
    pub listening_port: Option<u16>,
    pub peers: Vec<ShowPeer>,
}

pub fn parse_show(text: &str) -> Result<ShowInfo, String> {
    let mut info = ShowInfo::default();
    let mut cur: Option<ShowPeer> = None;

    for line in text.lines() {
        let t = line.trim();
        if let Some(pubkey) = t.strip_prefix("peer: ") {
            if let Some(p) = cur.take() {
                info.peers.push(p);
            }
            cur = Some(ShowPeer {
                pubkey: pubkey.trim().to_string(),
                ..ShowPeer::default()
            });
            continue;
        }
        let Some((key, val)) = t.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let val = val.trim();
        if cur.is_none() {
            match key {
                "interface" => info.interface = val.to_string(),
                "listening port" => info.listening_port = val.parse().ok(),
                _ => {}
            }
            continue;
        }
        let Some(peer) = cur.as_mut() else { continue };
        match key {
            "endpoint" => {
                peer.endpoint = match val {
                    "(none)" | "" => None,
                    ep => Some(ep.to_string()),
                };
            }
            "allowed ips" => {
                peer.allowed_ips = val.split(',').map(|s| s.trim().to_string()).collect();
            }
            "latest handshake" => peer.handshake = parse_handshake_age(val),
            "transfer" => {
                if let Some((rx, tx)) = parse_transfer(val) {
                    peer.rx = Some(rx);
                    peer.tx = Some(tx);
                }
            }
            "persistent keepalive" => {
                // "25" or "every 25 seconds"
                let mut toks = val.split_whitespace();
                peer.keepalive = toks
                    .next()
                    .and_then(|s| s.parse().ok())
                    .or_else(|| toks.next().and_then(|s| s.parse().ok()));
            }
            _ => {}
        }
    }
    if let Some(p) = cur.take() {
        info.peers.push(p);
    }
    if info.interface.is_empty() {
        return Err("not an 'awg show' output (no interface line)".into());
    }
    Ok(info)
}

/// "1 minute, 23 seconds ago" / "2 hours ago" → age. "off"/"0"/absent → None.
pub(crate) fn parse_handshake_age(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() || s == "off" || s == "0" {
        return None;
    }
    let s = s.strip_suffix(" ago").unwrap_or(s);
    let mut total = Duration::ZERO;
    for part in s.split(',') {
        let mut it = part.split_whitespace();
        let n: f64 = it.next()?.parse().ok()?;
        let unit = it.next()?;
        let secs = match unit.trim_end_matches('s') {
            "second" => 1.0,
            "minute" => 60.0,
            "hour" => 3600.0,
            "day" => 86400.0,
            "week" => 604800.0,
            _ => return None,
        };
        total += Duration::from_secs_f64(n * secs);
    }
    Some(total)
}

/// "1.2 MiB received, 3.4 MiB sent" → (rx, tx) in bytes.
pub(crate) fn parse_transfer(s: &str) -> Option<(u64, u64)> {
    let (rx_part, tx_part) = s.split_once(" received,")?;
    let tx_part = tx_part.trim().strip_suffix(" sent")?;
    Some((parse_bytes(rx_part)?, parse_bytes(tx_part)?))
}

fn parse_bytes(s: &str) -> Option<u64> {
    let mut it = s.split_whitespace();
    let n: f64 = it.next()?.parse().ok()?;
    let mult = match it.next() {
        None | Some("B") => 1.0,
        Some("KiB") => 1024.0,
        Some("MiB") => 1024.0_f64.powi(2),
        Some("GiB") => 1024.0_f64.powi(3),
        Some("TiB") => 1024.0_f64.powi(4),
        Some(_) => return None,
    };
    Some((n * mult) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured live from the colima stand (`amnezia-legacy-test`).
    const STAND_FIXTURE: &str = "\
interface: awg0
  listening port: 42015
  random trailers: off
  disable cookies: off

peer: 9R0OeW1gigwJ8VsD1gZzDUWQaL/QV6N7+0cpO+4JXyE=
  preshared key: (hidden)
  allowed ips: 10.8.1.2/32
  persistent keepalive: 25

peer: k5lYeOjn7xDzCPYncLV/i+b0w7iIEFgwj8s8et5mcXI=
  preshared key: (hidden)
  allowed ips: 10.8.1.3/32
  persistent keepalive: 25

peer: bam48L39KdmWsPazLFfwFXt+PpSNEI2sKgG/9nYXpxs=
  preshared key: (hidden)
  allowed ips: 10.8.1.4/32
  persistent keepalive: 25
";

    #[test]
    fn parses_stand_fixture_peers_without_handshakes() {
        let info = parse_show(STAND_FIXTURE).unwrap();
        assert_eq!(info.interface, "awg0");
        assert_eq!(info.listening_port, Some(42015));
        assert_eq!(info.peers.len(), 3);
        let p = &info.peers[0];
        assert_eq!(p.pubkey, "9R0OeW1gigwJ8VsD1gZzDUWQaL/QV6N7+0cpO+4JXyE=");
        assert_eq!(p.allowed_ips, vec!["10.8.1.2/32"]);
        assert_eq!(p.keepalive, Some(25));
        // never handshaked: lines absent, not errors
        assert_eq!(p.handshake, None);
        assert_eq!(p.endpoint, None);
        assert_eq!(p.rx, None);
        assert_eq!(p.tx, None);
    }

    #[test]
    fn parses_connected_peer_with_handshake_and_transfer() {
        let text = "\
interface: awg0
  listening port: 51820

peer: AAAA
  endpoint: 203.0.113.7:51820
  allowed ips: 10.8.1.2/32
  latest handshake: 1 minute, 23 seconds ago
  transfer: 12.5 MiB received, 3.1 MiB sent
  persistent keepalive: every 25 seconds
";
        let info = parse_show(text).unwrap();
        let p = &info.peers[0];
        assert_eq!(p.handshake, Some(Duration::from_secs(60 + 23)));
        assert_eq!(p.endpoint.as_deref(), Some("203.0.113.7:51820"));
        assert_eq!(p.rx, Some((12.5 * 1024.0 * 1024.0) as u64));
        assert_eq!(p.tx, Some((3.1 * 1024.0 * 1024.0) as u64));
        assert_eq!(p.keepalive, Some(25));
    }

    #[test]
    fn handshake_off_and_zero_mean_never() {
        assert_eq!(parse_handshake_age("off"), None);
        assert_eq!(parse_handshake_age("0"), None);
        assert_eq!(parse_handshake_age(""), None);
        assert_eq!(
            parse_handshake_age("2 days, 3 hours ago"),
            Some(Duration::from_secs(2 * 86400 + 3 * 3600))
        );
        assert_eq!(
            parse_handshake_age("3 weeks ago"),
            Some(Duration::from_secs(3 * 604800))
        );
        assert_eq!(parse_handshake_age("bogus"), None);
    }

    #[test]
    fn endpoint_none_maps_to_absent() {
        let text = "interface: awg0\n\npeer: X\n  endpoint: (none)\n";
        let info = parse_show(text).unwrap();
        assert_eq!(info.peers[0].endpoint, None);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_show("").is_err());
        assert!(parse_show("random text\n").is_err());
    }
}
