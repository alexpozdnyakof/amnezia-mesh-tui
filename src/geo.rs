//! Offline geoip for the connection map (M6). One local mmdb file
//! (GeoLite2-City / IP2Location LITE, user-provided) — the panel never
//! looks peer IPs up online.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use maxminddb::geoip2;

/// Default location suggested to the user (config can override).
pub fn default_mmdb_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("awg-mesh-tui").join("geoip.mmdb")
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeoInfo {
    pub lat: f64,
    pub lon: f64,
    pub country: Option<String>,
    pub city: Option<String>,
}

pub struct GeoDb {
    reader: maxminddb::Reader<Vec<u8>>,
}

impl std::fmt::Debug for GeoDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeoDb").finish_non_exhaustive()
    }
}

impl GeoDb {
    pub fn load(path: &Path) -> Result<Self, String> {
        let reader = maxminddb::Reader::open_readfile(path)
            .map_err(|e| format!("cannot open geoip db at {}: {e}", path.display()))?;
        Ok(GeoDb { reader })
    }

    /// All-`None` fields are possible for reserved/unknown ranges.
    pub fn lookup(&self, ip: IpAddr) -> Option<GeoInfo> {
        let city: geoip2::City = self.reader.lookup(ip).ok()?;
        let loc = city.location?;
        let lat = loc.latitude?;
        let lon = loc.longitude?;
        let name = |n: &Option<std::collections::BTreeMap<&str, &str>>| {
            n.as_ref()
                .and_then(|m| m.get("en"))
                .map(|s| (*s).to_string())
        };
        let country = name(&city.country.and_then(|c| c.names));
        let city_name = name(&city.city.and_then(|c| c.names));
        Some(GeoInfo {
            lat,
            lon,
            country,
            city: city_name,
        })
    }
}

/// "203.0.113.7:51820" → 203.0.113.7 (IPv6-aware: last-colon split).
pub fn endpoint_ip(endpoint: &str) -> Option<IpAddr> {
    // A bare address first — for "2001:db8::1" the last group looks like
    // a port number, so splitting before parsing would corrupt it.
    if let Ok(ip) = endpoint.parse::<IpAddr>() {
        return Some(ip);
    }
    let (host, port) = endpoint.rsplit_once(':')?;
    if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()
}

pub fn geo_label(g: &GeoInfo) -> String {
    match (&g.city, &g.country) {
        (Some(c), Some(co)) => format!("{c}, {co}"),
        (None, Some(co)) => co.clone(),
        (Some(c), None) => c.clone(),
        (None, None) => "unknown".to_string(),
    }
}

/// Lat/lon bounds for the map canvas: 15% margin, degenerate spans
/// expanded to ±5° so a single point does not collapse the projection.
#[cfg(test)]
fn fit_bounds(points: &[(f64, f64)]) -> Option<((f64, f64), (f64, f64))> {
    if points.is_empty() {
        return None;
    }
    let (mut lat_min, mut lat_max) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut lon_min, mut lon_max) = (f64::INFINITY, f64::NEG_INFINITY);
    for &(lat, lon) in points {
        lat_min = lat_min.min(lat);
        lat_max = lat_max.max(lat);
        lon_min = lon_min.min(lon);
        lon_max = lon_max.max(lon);
    }
    let lat_pad = ((lat_max - lat_min) * 0.15).max(5.0);
    let lon_pad = ((lon_max - lon_min) * 0.15).max(5.0);
    Some((
        (lat_min - lat_pad, lat_max + lat_pad),
        (lon_min - lon_pad, lon_max + lon_pad),
    ))
}

/// Hub placement fallback: centroid of peers with known coordinates.
pub fn centroid(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    if points.is_empty() {
        return None;
    }
    let (mut lat, mut lon) = (0.0, 0.0);
    for &(a, o) in points {
        lat += a;
        lon += o;
    }
    let n = points.len() as f64;
    Some((lat / n, lon / n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ipv4_and_ipv6_endpoints() {
        assert_eq!(
            endpoint_ip("203.0.113.7:51820"),
            "203.0.113.7".parse::<IpAddr>().ok()
        );
        assert_eq!(
            endpoint_ip("[2001:db8::1]:51820"),
            "2001:db8::1".parse::<IpAddr>().ok()
        );
        assert_eq!(
            endpoint_ip("2001:db8::1"),
            "2001:db8::1".parse::<IpAddr>().ok()
        );
        assert_eq!(endpoint_ip("(none)"), None);
        assert_eq!(endpoint_ip("bad:port"), None);
    }

    #[test]
    fn geo_label_prefers_city_country() {
        let g = GeoInfo {
            lat: 0.0,
            lon: 0.0,
            country: Some("NL".to_string()),
            city: Some("Amsterdam".to_string()),
        };
        assert_eq!(geo_label(&g), "Amsterdam, NL");
        let g = GeoInfo { city: None, ..g };
        assert_eq!(geo_label(&g), "NL");
    }

    #[test]
    fn default_path_ends_in_cache_dir() {
        assert!(default_mmdb_path().ends_with("awg-mesh-tui/geoip.mmdb"));
    }

    #[test]
    fn fit_bounds_margins_and_degenerate_expansion() {
        let b = fit_bounds(&[(55.0, 37.0), (59.0, 30.0)]).unwrap();
        assert!(b.0 .0 < 55.0 && b.0 .1 > 59.0);
        assert!(b.1 .0 < 30.0 && b.1 .1 > 37.0);
        let b = fit_bounds(&[(55.0, 37.0)]).unwrap();
        assert_eq!(b.0, (50.0, 60.0));
        assert_eq!(b.1, (32.0, 42.0));
        assert!(fit_bounds(&[]).is_none());
    }

    #[test]
    fn centroid_averages() {
        assert_eq!(centroid(&[(10.0, 20.0), (20.0, 40.0)]), Some((15.0, 30.0)));
        assert!(centroid(&[]).is_none());
    }
}
