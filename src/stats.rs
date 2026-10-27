/// Parser for `/var/log/amnezia-mesh-stats.log` (written every 10 min by
/// `scripts/awg-stats.sh`: `docker exec awg show | grep -E
/// "peer:|endpoint|latest handshake|transfer"` under `=== <ISO> ===`
/// headers).
///
/// Tolerated quirks (seen live): empty blocks, duplicate `=== ===` headers,
/// docker error text inside blocks (container down at sample time).
use crate::show::parse_transfer;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatsEntry {
    /// Raw `=== ... ===` header timestamp (ISO8601 from `date -Is`).
    pub at: String,
    pub peer: String,
    pub endpoint: Option<String>,
    /// Raw age text ("1 minute, 23 seconds ago"); "off"/absent → None.
    pub handshake: Option<String>,
    pub rx: Option<u64>,
    pub tx: Option<u64>,
}

pub fn parse_stats_log(text: &str) -> Vec<StatsEntry> {
    let mut entries = Vec::new();
    let mut at = String::new();
    let mut cur: Option<StatsEntry> = None;

    for line in text.lines() {
        let t = line.trim();
        if let Some(header) = t.strip_prefix("=== ") {
            flush(&mut cur, &mut entries);
            at = header
                .trim_end_matches('=')
                .trim()
                .trim_end_matches('=')
                .trim()
                .to_string();
            continue;
        }
        if let Some(pubkey) = t.strip_prefix("peer: ") {
            flush(&mut cur, &mut entries);
            cur = Some(StatsEntry {
                at: at.clone(),
                peer: pubkey.trim().to_string(),
                ..StatsEntry::default()
            });
            continue;
        }
        let Some((key, val)) = t.split_once(':') else {
            continue;
        };
        let Some(entry) = cur.as_mut() else { continue };
        match key.trim() {
            "endpoint" => {
                entry.endpoint = match val.trim() {
                    "(none)" | "" => None,
                    ep => Some(ep.to_string()),
                };
            }
            "latest handshake" => {
                let v = val.trim();
                entry.handshake = match v {
                    "" | "off" | "0" => None,
                    _ => Some(v.to_string()),
                };
            }
            "transfer" => {
                if let Some((rx, tx)) = parse_transfer(val.trim()) {
                    entry.rx = Some(rx);
                    entry.tx = Some(tx);
                }
            }
            _ => {}
        }
    }
    flush(&mut cur, &mut entries);
    entries
}

fn flush(cur: &mut Option<StatsEntry>, entries: &mut Vec<StatsEntry>) {
    if let Some(e) = cur.take() {
        entries.push(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks_and_peers() {
        let text = "\
=== 2026-10-26T12:00:01+07:00 ===
peer: PUB1
endpoint: 203.0.113.7:51820
latest handshake: 1 minute, 23 seconds ago
transfer: 1.0 MiB received, 512.0 KiB sent
peer: PUB2
latest handshake: off
=== 2026-10-26T12:10:01+07:00 ===
peer: PUB1
endpoint: 203.0.113.7:51820
latest handshake: 11 minutes, 23 seconds ago
transfer: 2.0 MiB received, 600.0 KiB sent
";
        let es = parse_stats_log(text);
        assert_eq!(es.len(), 3);
        assert_eq!(es[0].at, "2026-10-26T12:00:01+07:00");
        assert_eq!(es[0].peer, "PUB1");
        assert_eq!(es[0].endpoint.as_deref(), Some("203.0.113.7:51820"));
        assert_eq!(es[0].handshake.as_deref(), Some("1 minute, 23 seconds ago"));
        assert_eq!(es[0].rx, Some(1024 * 1024));
        assert_eq!(es[0].tx, Some(512 * 1024));
        assert_eq!(es[1].peer, "PUB2");
        assert_eq!(es[1].handshake, None);
        assert_eq!(es[2].at, "2026-10-26T12:10:01+07:00");
    }

    #[test]
    fn tolerates_empty_blocks_duplicate_headers_and_junk() {
        let text = "\
=== 2026-10-26T12:00:01+07:00 ===
=== 2026-10-26T12:00:01+07:00 ===
Error response from daemon: container not running
=== 2026-10-26T12:10:01+07:00 ===
peer: PUB1
transfer: 0 B received, 0 B sent
some stray line without colon
";
        let es = parse_stats_log(text);
        assert_eq!(es.len(), 1);
        assert_eq!(es[0].rx, Some(0));
    }

    #[test]
    fn empty_input_yields_no_entries() {
        assert!(parse_stats_log("").is_empty());
    }
}
