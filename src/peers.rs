use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::show::ShowPeer;
use crate::transport::Transport;

/// Status thresholds (plan defaults; config overrides come later).
pub const LIVE_MAX_AGE: Duration = Duration::from_secs(3 * 60);
pub const STALE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerStatus {
    Live,
    Stale,
    Dead,
}

impl PeerStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PeerStatus::Live => "live",
            PeerStatus::Stale => "stale",
            PeerStatus::Dead => "dead",
        }
    }
}

/// Previous transfer counters of one peer (for rx/tx rate diffs).
#[derive(Debug, Clone)]
pub struct PeerSample {
    pub rx: u64,
    pub tx: u64,
    pub at: Instant,
}

/// A dashboard row: raw `awg show` data enriched with name, status, rates.
#[derive(Debug, Clone)]
pub struct ResolvedPeer {
    pub name: String,
    pub ip: String,
    /// Stable identity for M3/M4 (log correlation, mutations by key).
    #[allow(dead_code)]
    pub pubkey: String,
    pub status: PeerStatus,
    pub handshake: Option<Duration>,
    pub endpoint: Option<String>,
    /// bytes/sec since the previous poll; 0 on the first poll.
    pub rx_rate: f64,
    pub tx_rate: f64,
}

/// Merge a fresh `awg show` snapshot into dashboard rows.
///
/// Name resolution: pubkey (from `peer list --json`) → IP (from the inspect
/// cache) → short pubkey fallback. `prev_samples` is updated in place and
/// used for rx/tx rates; counter resets (container restart) clamp to 0.
pub fn resolve_peers(
    show: &[ShowPeer],
    names_by_pub: &HashMap<String, String>,
    names_by_ip: &HashMap<String, String>,
    prev_samples: &mut HashMap<String, PeerSample>,
    now: Instant,
) -> Vec<ResolvedPeer> {
    show.iter()
        .map(|p| {
            let ip = p
                .allowed_ips
                .first()
                .map(|a| a.split('/').next().unwrap_or(a).to_string())
                .unwrap_or_default();
            let name = names_by_pub
                .get(&p.pubkey)
                .cloned()
                .or_else(|| names_by_ip.get(&ip).cloned())
                .unwrap_or_else(|| short_pubkey(&p.pubkey));
            let status = match p.handshake {
                None => PeerStatus::Dead,
                Some(age) if age <= LIVE_MAX_AGE => PeerStatus::Live,
                Some(age) if age <= STALE_MAX_AGE => PeerStatus::Stale,
                Some(_) => PeerStatus::Dead,
            };
            let (rx_rate, tx_rate) = match (p.rx, p.tx) {
                (Some(rx), Some(tx)) => {
                    let (mut drx, mut dtx, mut dt) = (0.0_f64, 0.0_f64, 0.0_f64);
                    if let Some(s) = prev_samples.get(&p.pubkey) {
                        drx = rx.saturating_sub(s.rx) as f64;
                        dtx = tx.saturating_sub(s.tx) as f64;
                        dt = now.duration_since(s.at).as_secs_f64();
                    }
                    prev_samples.insert(p.pubkey.clone(), PeerSample { rx, tx, at: now });
                    if dt > 0.0 {
                        (drx / dt, dtx / dt)
                    } else {
                        (0.0, 0.0)
                    }
                }
                _ => (0.0, 0.0),
            };
            ResolvedPeer {
                name,
                ip,
                pubkey: p.pubkey.clone(),
                status,
                handshake: p.handshake,
                endpoint: p.endpoint.clone(),
                rx_rate,
                tx_rate,
            }
        })
        .collect()
}

pub fn short_pubkey(pubkey: &str) -> String {
    format!("peer-{}", pubkey.chars().take(8).collect::<String>())
}

/// pubkey→name map from `awg-mesh peer list --json` (Шаг 0 added `pub`).
/// Returns an empty map when the stack is not initialized — the dashboard
/// then falls back to IP/short-pubkey names.
pub async fn fetch_names(transport: &Transport, cli: &str) -> HashMap<String, String> {
    let Ok(out) = transport.exec(&format!("{cli} peer list --json")).await else {
        return HashMap::new();
    };
    if !out.ok() {
        return HashMap::new();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(out.stdout_trimmed()) else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    if let Some(peers) = v["peers"].as_array() {
        for p in peers {
            if let (Some(pubk), Some(name)) = (p["pub"].as_str(), p["name"].as_str()) {
                map.insert(pubk.to_string(), name.to_string());
            }
        }
    }
    map
}

/// "1m 23s" / "2h 5m" / "3d" / "7d"
pub fn format_age(age: Duration) -> String {
    let s = age.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else if s < 86400 {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d", s / 86400)
    }
}

/// "512 B", "1.5 KiB", "12.5 MiB", "1.3 GiB"
pub fn format_bytes(bytes: f64) -> String {
    if bytes < 1024.0 {
        format!("{:.0} B", bytes)
    } else if bytes < 1024.0_f64.powi(2) {
        format!("{:.1} KiB", bytes / 1024.0)
    } else if bytes < 1024.0_f64.powi(3) {
        format!("{:.1} MiB", bytes / 1024.0_f64.powi(2))
    } else {
        format!("{:.1} GiB", bytes / 1024.0_f64.powi(3))
    }
}

/// "512 B/s", "1.5 KiB/s", "12.5 MiB/s"
pub fn format_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(pubkey: &str, ip: &str, handshake: Option<Duration>) -> ShowPeer {
        ShowPeer {
            pubkey: pubkey.to_string(),
            endpoint: None,
            allowed_ips: vec![format!("{ip}/32")],
            handshake,
            rx: None,
            tx: None,
            keepalive: None,
        }
    }

    #[test]
    fn resolves_status_by_handshake_age() {
        let show = vec![
            peer("A", "10.0.0.1", Some(Duration::from_secs(30))),
            peer("B", "10.0.0.2", Some(Duration::from_secs(3600))),
            peer("C", "10.0.0.3", Some(Duration::from_secs(3 * 86400))),
            peer("D", "10.0.0.4", None),
        ];
        let mut samples = HashMap::new();
        let rows = resolve_peers(
            &show,
            &HashMap::new(),
            &HashMap::new(),
            &mut samples,
            Instant::now(),
        );
        assert_eq!(rows[0].status, PeerStatus::Live);
        assert_eq!(rows[1].status, PeerStatus::Stale);
        assert_eq!(rows[2].status, PeerStatus::Dead);
        assert_eq!(rows[3].status, PeerStatus::Dead);
        assert!(rows[0].name.starts_with("peer-"));
        assert_eq!(rows[1].ip, "10.0.0.2");
    }

    #[test]
    fn prefers_pubkey_name_then_ip_name() {
        let show = vec![peer("PUBKEY1", "10.0.0.1", None)];
        let mut by_pub = HashMap::new();
        by_pub.insert("PUBKEY1".to_string(), "from-pub".to_string());
        let mut by_ip = HashMap::new();
        by_ip.insert("10.0.0.1".to_string(), "from-ip".to_string());
        let mut samples = HashMap::new();
        let rows = resolve_peers(&show, &by_pub, &by_ip, &mut samples, Instant::now());
        assert_eq!(rows[0].name, "from-pub");

        let rows = resolve_peers(&show, &HashMap::new(), &by_ip, &mut samples, Instant::now());
        assert_eq!(rows[0].name, "from-ip");
    }

    #[test]
    fn rates_diff_prev_sample_and_clamp_reset() {
        let mut p = peer("P", "10.0.0.1", None);
        p.rx = Some(2000);
        p.tx = Some(1000);
        let mut samples = HashMap::new();
        let t0 = Instant::now();
        samples.insert(
            "P".to_string(),
            PeerSample {
                rx: 1000,
                tx: 500,
                at: t0 - Duration::from_secs(10),
            },
        );
        let rows = resolve_peers(
            &[p.clone()],
            &HashMap::new(),
            &HashMap::new(),
            &mut samples,
            t0,
        );
        assert_eq!(rows[0].rx_rate, 100.0);
        assert_eq!(rows[0].tx_rate, 50.0);

        // counter reset (container restart) must not go negative
        p.rx = Some(100);
        let rows = resolve_peers(
            &[p],
            &HashMap::new(),
            &HashMap::new(),
            &mut samples,
            t0 + Duration::from_secs(5),
        );
        assert_eq!(rows[0].rx_rate, 0.0);
    }

    #[test]
    fn formats_age_and_rate() {
        assert_eq!(format_age(Duration::from_secs(83)), "1m 23s");
        assert_eq!(format_age(Duration::from_secs(7500)), "2h 5m");
        assert_eq!(format_age(Duration::from_secs(3 * 86400)), "3d");
        assert_eq!(format_rate(512.0), "512 B/s");
        assert_eq!(format_rate(1536.0), "1.5 KiB/s");
    }
}
