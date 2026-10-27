//! Chart data builders for the log screen: rx/tx series and the presence
//! heatmap. Pure functions — the UI only renders what these produce.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, FixedOffset};

use crate::show::parse_handshake_age;
use crate::stats::StatsEntry;

pub const BUCKET_MINUTES: i64 = 10;
pub const HEATMAP_HOURS: i64 = 24;
/// A peer counts as online for a sample when its handshake age was at most
/// two sample intervals (the logger runs every 10 min).
pub const PRESENCE_GRACE: StdDuration = StdDuration::from_secs(20 * 60);

/// One point of the rx/tx chart: bytes transferred since the previous sample.
#[derive(Debug, Clone, PartialEq)]
pub struct TrafficPoint {
    pub ts: DateTime<FixedOffset>,
    pub rx: f64,
    pub tx: f64,
}

/// Per-interval rx/tx deltas for one peer, chronological. Counter resets
/// (container restart) clamp to 0; samples without counters are skipped.
pub fn traffic_series(entries: &[StatsEntry], peer: &str) -> Vec<TrafficPoint> {
    let mut mine: Vec<&StatsEntry> = entries
        .iter()
        .filter(|e| e.peer == peer && e.ts.is_some())
        .collect();
    mine.sort_by_key(|e| e.ts);
    let mut out = Vec::new();
    let mut prev: Option<&StatsEntry> = None;
    for e in mine {
        let (Some(rx), Some(tx)) = (e.rx, e.tx) else {
            prev = Some(e);
            continue;
        };
        if let Some(p) = prev {
            if let (Some(prx), Some(ptx)) = (p.rx, p.tx) {
                out.push(TrafficPoint {
                    ts: e.ts.expect("filtered above"),
                    rx: rx.saturating_sub(prx) as f64,
                    tx: tx.saturating_sub(ptx) as f64,
                });
            }
        }
        prev = Some(e);
    }
    out
}

/// Bucket-average downsample to at most `max` points (chart width).
pub fn downsample(points: &[TrafficPoint], max: usize) -> Vec<TrafficPoint> {
    if max == 0 || points.len() <= max {
        return points.to_vec();
    }
    let bucket = points.len().div_ceil(max);
    points
        .chunks(bucket)
        .map(|c| TrafficPoint {
            ts: c[0].ts,
            rx: c.iter().map(|p| p.rx).sum::<f64>() / c.len() as f64,
            tx: c.iter().map(|p| p.tx).sum::<f64>() / c.len() as f64,
        })
        .collect()
}

pub fn heatmap_columns() -> usize {
    (HEATMAP_HOURS * 60 / BUCKET_MINUTES) as usize
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeatCell {
    Online,
    Offline,
    NoData,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeatmapRow {
    pub peer: String,
    pub cells: Vec<HeatCell>,
}

/// Presence matrix for the 24 h window ending at `now`: peers × 10-minute
/// buckets. `now` is data-relative (max entry timestamp) so the view is
/// testable and immune to clock skew.
pub fn presence_heatmap(entries: &[StatsEntry], now: DateTime<FixedOffset>) -> Vec<HeatmapRow> {
    let cols = heatmap_columns();
    let start = now - Duration::hours(HEATMAP_HOURS);
    let mut peers: Vec<String> = Vec::new();
    for e in entries {
        if !peers.contains(&e.peer) {
            peers.push(e.peer.clone());
        }
    }
    peers
        .into_iter()
        .map(|peer| {
            let mut cells = vec![HeatCell::NoData; cols];
            for e in entries.iter().filter(|e| e.peer == peer) {
                let Some(ts) = e.ts else { continue };
                if !(start..=now).contains(&ts) {
                    continue;
                }
                let col = ((ts - start).num_minutes() / BUCKET_MINUTES) as usize;
                let online = e
                    .handshake
                    .as_deref()
                    .and_then(parse_handshake_age)
                    .is_some_and(|age| age <= PRESENCE_GRACE);
                if online {
                    cells[col] = HeatCell::Online;
                } else if cells[col] == HeatCell::NoData {
                    cells[col] = HeatCell::Offline;
                }
            }
            HeatmapRow { peer, cells }
        })
        .collect()
}

/// Max timestamp across entries — the heatmap window anchor.
pub fn entries_end(entries: &[StatsEntry]) -> Option<DateTime<FixedOffset>> {
    entries.iter().filter_map(|e| e.ts).max()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    fn entry(at: &str, peer: &str, handshake: Option<&str>, rx: u64, tx: u64) -> StatsEntry {
        StatsEntry {
            at: at.to_string(),
            ts: crate::stats::parse_ts(at),
            peer: peer.to_string(),
            endpoint: None,
            handshake: handshake.map(|s| s.to_string()),
            rx: Some(rx),
            tx: Some(tx),
        }
    }

    #[test]
    fn series_diffs_and_clamps_reset() {
        let es = vec![
            entry("2026-10-27T08:00:00+00:00", "P1", None, 1000, 500),
            entry("2026-10-27T08:10:00+00:00", "P1", None, 1600, 800),
            entry("2026-10-27T08:20:00+00:00", "P1", None, 100, 50),
        ];
        let pts = traffic_series(&es, "P1");
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0].rx, 600.0);
        assert_eq!(pts[0].tx, 300.0);
        assert_eq!(pts[1].rx, 0.0); // counter reset clamped
    }

    #[test]
    fn series_sorts_unsorted_input() {
        let es = vec![
            entry("2026-10-27T08:10:00+00:00", "P1", None, 1600, 0),
            entry("2026-10-27T08:00:00+00:00", "P1", None, 1000, 0),
        ];
        let pts = traffic_series(&es, "P1");
        assert_eq!(pts[0].ts, ts("2026-10-27T08:10:00+00:00"));
        assert_eq!(pts[0].rx, 600.0);
    }

    #[test]
    fn downsample_averages_buckets() {
        let pts: Vec<TrafficPoint> = (0..10)
            .map(|i| TrafficPoint {
                ts: ts("2026-10-27T08:00:00+00:00") + Duration::minutes(i * 10),
                rx: 100.0,
                tx: 10.0,
            })
            .collect();
        let d = downsample(&pts, 4);
        assert_eq!(d.len(), 4); // buckets of 3,3,2,2
        assert_eq!(d[0].rx, 100.0);
        let d = downsample(&pts, 20);
        assert_eq!(d.len(), 10);
    }

    #[test]
    fn heatmap_marks_online_offline_nodata() {
        let now = ts("2026-10-27T12:00:00+00:00");
        let es = vec![
            // online: handshake 5 minutes ago at sample time
            entry(
                "2026-10-27T11:00:00+00:00",
                "P1",
                Some("5 minutes ago"),
                0,
                0,
            ),
            // offline: handshake 2 hours ago
            entry("2026-10-27T11:10:00+00:00", "P1", Some("2 hours ago"), 0, 0),
            // outside the 24h window → ignored
            entry(
                "2026-10-26T10:00:00+00:00",
                "P1",
                Some("1 minute ago"),
                0,
                0,
            ),
            entry("2026-10-27T11:00:00+00:00", "P2", None, 0, 0),
        ];
        let rows = presence_heatmap(&es, now);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].peer, "P1");
        let cols = heatmap_columns();
        assert_eq!(rows[0].cells.len(), cols);
        let c = |t: &str| {
            let t = ts(t);
            ((t - (now - Duration::hours(24))).num_minutes() / BUCKET_MINUTES) as usize
        };
        assert_eq!(
            rows[0].cells[c("2026-10-27T11:00:00+00:00")],
            HeatCell::Online
        );
        assert_eq!(
            rows[0].cells[c("2026-10-27T11:10:00+00:00")],
            HeatCell::Offline
        );
        assert_eq!(rows[0].cells[cols - 1], HeatCell::NoData);
        // no-handshake sample → offline, and online wins over offline
        assert_eq!(
            rows[1].cells[c("2026-10-27T11:00:00+00:00")],
            HeatCell::Offline
        );
    }

    #[test]
    fn entries_end_picks_max() {
        let es = vec![
            entry("2026-10-27T08:00:00+00:00", "P", None, 0, 0),
            entry("2026-10-27T09:30:00+00:00", "P", None, 0, 0),
        ];
        assert_eq!(entries_end(&es), Some(ts("2026-10-27T09:30:00+00:00")));
    }
}
