use crate::transport::Transport;

pub const STATS_LOG_PATH: &str = "/var/log/amnezia-mesh-stats.log";
pub const WATCHDOG_LOG_PATH: &str = "/var/log/amnezia-mesh-watchdog.log";

/// The full log is capped at 30k lines by the logger itself; the UI only
/// needs the recent tail (charts downsample further).
const STATS_TAIL: u32 = 500;
const WATCHDOG_TAIL: u32 = 100;

/// Read-only tail of the stats log over ssh (`awg-mesh monitoring install`
/// cron). A missing file is an error — the log screen shows it as a banner.
pub async fn fetch_stats_log(transport: &Transport) -> Result<String, String> {
    let out = transport
        .exec(&format!(
            "tail -n {STATS_TAIL} {STATS_LOG_PATH} 2>/dev/null"
        ))
        .await
        .map_err(|e| e.to_string())?;
    if !out.ok() {
        let msg = out.stderr.trim();
        return Err(if msg.is_empty() {
            format!("cannot read {STATS_LOG_PATH}")
        } else {
            format!("cannot read {STATS_LOG_PATH}: {msg}")
        });
    }
    Ok(out.stdout)
}

pub async fn fetch_watchdog_log(transport: &Transport) -> Result<String, String> {
    let out = transport
        .exec(&format!(
            "tail -n {WATCHDOG_TAIL} {WATCHDOG_LOG_PATH} 2>/dev/null"
        ))
        .await
        .map_err(|e| e.to_string())?;
    if !out.ok() {
        let msg = out.stderr.trim();
        return Err(if msg.is_empty() {
            format!("cannot read {WATCHDOG_LOG_PATH}")
        } else {
            format!("cannot read {WATCHDOG_LOG_PATH}: {msg}")
        });
    }
    Ok(out.stdout)
}
