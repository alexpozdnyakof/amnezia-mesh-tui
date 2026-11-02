//! Amnezia `vpn://` codec.
//!
//! `"vpn://" + base64url_nopad(uint32_be(len) + zlib(json, level 9))` with
//! compact JSON. Matches `awg_mesh/profile.py` and the Go CLI. Deflate
//! streams are not byte-identical across languages; roundtrip is semantic.
//! The summary view never copies private keys or PSKs.

use std::io::{Read, Write};
use std::path::Path;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use qrcode::types::Color as ModuleColor;
use qrcode::{EcLevel, QrCode};

/// Refuse profiles whose JSON expands past this. Real vpn:// payloads are a few KiB.
const MAX_PROFILE_JSON: usize = 256 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProfileError {
    #[error("bad base64 in vpn://")]
    Base64,
    #[error("truncated vpn:// payload")]
    Truncated,
    #[error("bad zlib payload")]
    Zlib,
    #[error("vpn:// payload is too large")]
    TooLarge,
    #[error("profile is not json")]
    Json,
    #[error("could not encode a QR code")]
    Qr,
    #[error("could not write the QR png")]
    Io,
}

/// Public fields of a vpn:// profile. No private key, no PSK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileView {
    pub description: String,
    pub host: String,
    pub port: String,
    pub client_ip: String,
    pub dns: String,
}

/// First `vpn://` token in `text`, without surrounding whitespace.
pub fn find_vpn_url(text: &str) -> Option<&str> {
    let start = text.find("vpn://")?;
    let rest = &text[start..];
    let end = rest
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    let url = &rest[..end];
    if url.len() <= "vpn://".len() {
        None
    } else {
        Some(url)
    }
}

/// The TUI decodes profiles the server already issued. Encode stays for the
/// semantic roundtrip against the Python/Go fixtures.
#[allow(dead_code)]
pub fn encode_profile(value: &serde_json::Value) -> Result<String, ProfileError> {
    let inner = serde_json::to_vec(value).map_err(|_| ProfileError::Json)?;
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::best());
    enc.write_all(&inner).map_err(|_| ProfileError::Zlib)?;
    let payload = enc.finish().map_err(|_| ProfileError::Zlib)?;
    if payload.len() > u32::MAX as usize {
        return Err(ProfileError::TooLarge);
    }
    let mut framed = Vec::with_capacity(4 + payload.len());
    framed.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    framed.extend_from_slice(&payload);
    Ok(format!("vpn://{}", URL_SAFE_NO_PAD.encode(framed)))
}

pub fn decode_profile(url: &str) -> Result<serde_json::Value, ProfileError> {
    let url = url.trim();
    let b64 = url.strip_prefix("vpn://").unwrap_or(url);
    let b64 = b64.trim_end_matches('=');
    if b64.is_empty() {
        return Err(ProfileError::Truncated);
    }
    let raw = URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ProfileError::Base64)?;
    if raw.len() < 4 {
        return Err(ProfileError::Truncated);
    }
    let length = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
    let end = (4 + length).min(raw.len());
    let json = inflate_capped(&raw[4..end])?;
    serde_json::from_slice(&json).map_err(|_| ProfileError::Json)
}

fn inflate_capped(data: &[u8]) -> Result<Vec<u8>, ProfileError> {
    if data.len() > MAX_PROFILE_JSON {
        return Err(ProfileError::TooLarge);
    }
    let mut dec = ZlibDecoder::new(data);
    let mut out = Vec::new();
    let mut buf = [0u8; 8 * 1024];
    loop {
        let n = dec.read(&mut buf).map_err(|_| ProfileError::Zlib)?;
        if n == 0 {
            break;
        }
        if out.len().saturating_add(n) > MAX_PROFILE_JSON {
            return Err(ProfileError::TooLarge);
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

pub fn summarize(value: &serde_json::Value) -> ProfileView {
    let description = json_str(value, "description");
    let host = json_str(value, "hostName");
    let dns = json_str(value, "dns1");
    let awg = value
        .get("containers")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("awg"));
    let port = awg.map(json_port).unwrap_or_default();
    let client_ip = awg.map(client_ip_of).unwrap_or_default();
    ProfileView {
        description,
        host,
        port,
        client_ip,
        dns,
    }
}

fn json_str(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn json_port(awg: &serde_json::Value) -> String {
    match awg.get("port") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// `last_config` is a JSON string inside the awg block. Read only `client_ip`.
fn client_ip_of(awg: &serde_json::Value) -> String {
    let Some(last) = awg.get("last_config") else {
        return String::new();
    };
    let parsed;
    let obj = match last {
        serde_json::Value::String(s) => {
            parsed = serde_json::from_str::<serde_json::Value>(s).ok();
            parsed.as_ref()
        }
        other => Some(other),
    };
    obj.and_then(|v| v.get("client_ip"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Braille QR (2×4 modules per cell) at the largest quiet zone that fits.
/// `None` when the code cannot be built or is wider/taller than the pane.
/// Error correction is L so a real `vpn://` stays small enough to scan.
pub fn terminal_qr(data: &str, max_cols: u16, max_rows: u16) -> Option<String> {
    if data.is_empty() || max_cols == 0 || max_rows == 0 {
        return None;
    }
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::L).ok()?;
    let modules = code.width();
    for quiet in (2..=4).rev() {
        let side = modules + quiet * 2;
        let cols = side.div_ceil(2);
        let rows = side.div_ceil(4);
        if cols <= max_cols as usize && rows <= max_rows as usize {
            return Some(render_braille(&code, quiet));
        }
    }
    None
}

/// `qrencode`-style half-block QR (1×2 modules per cell). Solid square
/// modules scan more reliably from a phone than braille dots, but the code
/// needs `side` cols × `side/2` rows — pick [`terminal_qr`] when the pane
/// is narrow. Same quiet-zone fallback as `terminal_qr`.
pub fn terminal_qr_blocks(data: &str, max_cols: u16, max_rows: u16) -> Option<String> {
    if data.is_empty() || max_cols == 0 || max_rows == 0 {
        return None;
    }
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::L).ok()?;
    let modules = code.width();
    for quiet in (2..=4).rev() {
        let side = modules + quiet * 2;
        if side <= max_cols as usize && side.div_ceil(2) <= max_rows as usize {
            return Some(render_half_blocks(&code, quiet));
        }
    }
    None
}

fn dark_at(code: &QrCode, modules: usize, quiet: usize, x: usize, y: usize) -> bool {
    if x < quiet || y < quiet {
        return false;
    }
    let (mx, my) = (x - quiet, y - quiet);
    mx < modules && my < modules && code[(mx, my)] == ModuleColor::Dark
}

/// Dots in a cell, Unicode order: 1 4 / 2 5 / 3 6 / 7 8.
const BRAILLE_DOTS: [(usize, usize, u8); 8] = [
    (0, 0, 0x01),
    (1, 0, 0x08),
    (0, 1, 0x02),
    (1, 1, 0x10),
    (0, 2, 0x04),
    (1, 2, 0x20),
    (0, 3, 0x40),
    (1, 3, 0x80),
];

fn render_braille(code: &QrCode, quiet: usize) -> String {
    let modules = code.width();
    let side = modules + quiet * 2;
    let cols = side.div_ceil(2);
    let rows = side.div_ceil(4);
    let mut out = String::with_capacity(rows * (cols + 1));
    for row in 0..rows {
        if row > 0 {
            out.push('\n');
        }
        for col in 0..cols {
            let mut bits = 0u8;
            for (dx, dy, bit) in BRAILLE_DOTS {
                if dark_at(code, modules, quiet, col * 2 + dx, row * 4 + dy) {
                    bits |= bit;
                }
            }
            out.push(char::from_u32(0x2800 + u32::from(bits)).expect("braille"));
        }
    }
    out
}

/// `▀`/`▄`/`█`/space — one char per column, two modules per row, all
/// rendered black on white so the caller can style the whole block at once.
fn render_half_blocks(code: &QrCode, quiet: usize) -> String {
    let modules = code.width();
    let side = modules + quiet * 2;
    let rows = side.div_ceil(2);
    let mut out = String::with_capacity(rows * (side + 1));
    for row in 0..rows {
        if row > 0 {
            out.push('\n');
        }
        for x in 0..side {
            let top = dark_at(code, modules, quiet, x, row * 2);
            let bottom = dark_at(code, modules, quiet, x, row * 2 + 1);
            out.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
    }
    out
}

/// PNG QR for messengers and "show from the phone gallery" sharing: as
/// close to `TARGET_QR_PNG` pixels as possible, never larger. The module
/// count is fixed by the payload, so the size snaps to a whole-module scale.
const TARGET_QR_PNG: u32 = 320;

pub fn write_qr_png(data: &str, path: &Path) -> Result<(), ProfileError> {
    use image::Luma;
    if data.is_empty() {
        return Err(ProfileError::Qr);
    }
    let code = QrCode::new(data.as_bytes()).map_err(|_| ProfileError::Qr)?;
    let side = (code.width() + 8) as u32; // quiet zone 4 on each side
    let module = (TARGET_QR_PNG / side).max(1);
    let image = code
        .render::<Luma<u8>>()
        .module_dimensions(module, module)
        .quiet_zone(true)
        .build();
    image.save(path).map_err(|_| ProfileError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("../tests/fixtures/peer-exit.vpn");

    fn client_priv(value: &serde_json::Value) -> String {
        let last = value["containers"][0]["awg"]["last_config"]
            .as_str()
            .unwrap();
        let inner: serde_json::Value = serde_json::from_str(last).unwrap();
        inner["client_priv_key"].as_str().unwrap().to_string()
    }

    #[test]
    fn golden_laptop_exit_roundtrip_keeps_the_key_out_of_the_summary() {
        let url = find_vpn_url(GOLDEN).unwrap();
        let value = decode_profile(url).unwrap();
        let view = summarize(&value);
        assert_eq!(view.host, "192.0.2.10");
        assert_eq!(view.description, "192.0.2.10 (laptop exit)");
        assert_eq!(view.dns, "100.64.0.1");
        assert_eq!(view.port, "443");
        assert_eq!(view.client_ip, "100.64.0.7");
        let key = client_priv(&value);
        assert!(!key.is_empty());
        assert_ne!(view.description, key);
        assert!(!view.host.contains(&key));
        assert!(!view.client_ip.contains(&key));
        assert!(!view.dns.contains(&key));
        assert!(!view.port.contains(&key));
        let again = decode_profile(&encode_profile(&value).unwrap()).unwrap();
        assert_eq!(again, value);
    }

    #[test]
    fn unicode_json_roundtrips_and_ignores_padding_and_trailing_bytes() {
        let value = serde_json::json!({
            "description": "ноутбук — exit",
            "hostName": "192.0.2.10",
            "dns1": "100.64.0.1"
        });
        let url = encode_profile(&value).unwrap();
        assert_eq!(decode_profile(&url).unwrap(), value);
        assert_eq!(decode_profile(&format!("  {url}\n")).unwrap(), value);

        let body = url.trim_start_matches("vpn://");
        let padded = format!("vpn://{body}===");
        assert_eq!(decode_profile(&padded).unwrap(), value);

        let mut raw = URL_SAFE_NO_PAD.decode(body).unwrap();
        raw.push(0xff);
        let dirty = format!("vpn://{}", URL_SAFE_NO_PAD.encode(raw));
        assert_eq!(decode_profile(&dirty).unwrap(), value);
    }

    #[test]
    fn rejects_bad_base64_truncated_and_bad_zlib() {
        assert_eq!(decode_profile("vpn://!!!!"), Err(ProfileError::Base64));
        assert_eq!(decode_profile("vpn://AAAA"), Err(ProfileError::Truncated));
        assert_eq!(decode_profile("vpn://"), Err(ProfileError::Truncated));
        assert_eq!(decode_profile("   "), Err(ProfileError::Truncated));

        let junk = URL_SAFE_NO_PAD.encode(
            4u32.to_be_bytes()
                .into_iter()
                .chain(*b"junk")
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            decode_profile(&format!("vpn://{junk}")),
            Err(ProfileError::Zlib)
        );
    }

    #[test]
    fn rejects_json_that_expands_past_the_cap() {
        let huge = "x".repeat(MAX_PROFILE_JSON + 1);
        let url = encode_profile(&serde_json::Value::String(huge)).unwrap();
        assert_eq!(decode_profile(&url), Err(ProfileError::TooLarge));
    }

    fn is_braille(text: &str) -> bool {
        text.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c))
    }

    #[test]
    fn terminal_qr_blocks_uses_half_blocks_and_needs_a_wide_pane() {
        let url = find_vpn_url(GOLDEN).unwrap();
        let qr = terminal_qr_blocks(url, 240, 100).expect("golden vpn:// fits a wide pane");
        let cols = qr.lines().next().unwrap().chars().count();
        let rows = qr.lines().count();
        assert!(cols <= 240 && rows <= 100, "{cols}×{rows}");
        // One char per column, two modules per row: full-width lines and
        // exactly ceil(side/2) rows.
        assert!(qr.lines().all(|l| l.chars().count() == cols));
        assert!(rows * 2 >= cols && rows * 2 <= cols + 1, "{cols}×{rows}");
        assert!(
            qr.chars().any(|c| ('\u{2580}'..='\u{259F}').contains(&c)),
            "expected half-block glyphs:\n{qr}"
        );
        // A pane that fits braille is still too narrow for half blocks.
        let braille = terminal_qr(url, 100, 40).expect("braille fits a small pane");
        assert!(cols > braille.lines().next().unwrap().chars().count());
        assert!(terminal_qr_blocks(url, 100, 40).is_none());
    }

    #[test]
    fn terminal_qr_is_braille_and_a_real_profile_fits() {
        let small = terminal_qr("hi", 80, 40).expect("short qr");
        assert!(is_braille(&small), "{small}");
        let url = find_vpn_url(GOLDEN).unwrap();
        let qr = terminal_qr(url, 100, 40).expect("golden vpn:// fits 100×40");
        let cols = qr.lines().next().unwrap().chars().count();
        let rows = qr.lines().count();
        assert!(cols <= 100 && rows <= 40, "{cols}×{rows}");
        assert!(is_braille(&qr));
        assert!(terminal_qr(&format!("vpn://{}", "A".repeat(4096)), 40, 20).is_none());
    }

    #[test]
    fn write_qr_png_targets_320px_never_larger() {
        let path =
            std::env::temp_dir().join(format!("awg-mesh-tui-qr-{}-hi.png", std::process::id()));
        let golden_url = find_vpn_url(GOLDEN).unwrap();
        write_qr_png(golden_url, &path).unwrap();
        use image::GenericImageView as _;
        let img = image::open(&path).unwrap();
        let (w, h) = img.dimensions();
        let _ = std::fs::remove_file(&path);
        assert!(w <= TARGET_QR_PNG && h <= TARGET_QR_PNG, "{w}×{h}");
        assert!(w > TARGET_QR_PNG / 2, "too small: {w}×{h}");
        assert_eq!(w, h, "the QR must stay square");
    }
}
