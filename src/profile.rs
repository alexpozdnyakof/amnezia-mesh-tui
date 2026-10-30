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
use image::Luma;
use qrcode::render::unicode::Dense1x2;
use qrcode::QrCode;

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

/// Half-block QR, or `None` when it does not fit the pane.
pub fn terminal_qr(data: &str, max_cols: u16, max_rows: u16) -> Option<String> {
    if data.is_empty() || max_cols == 0 || max_rows == 0 {
        return None;
    }
    let code = QrCode::new(data.as_bytes()).ok()?;
    let rendered = code.render::<Dense1x2>().quiet_zone(true).build();
    let width = rendered
        .lines()
        .next()
        .map(|line| line.chars().count())
        .unwrap_or(0);
    let height = rendered.lines().count();
    if width == 0 || height == 0 || width > max_cols as usize || height > max_rows as usize {
        return None;
    }
    Some(rendered)
}

pub fn write_qr_png(data: &str, path: &Path) -> Result<(), ProfileError> {
    if data.is_empty() {
        return Err(ProfileError::Qr);
    }
    let code = QrCode::new(data.as_bytes()).map_err(|_| ProfileError::Qr)?;
    let image = code
        .render::<Luma<u8>>()
        .min_dimensions(512, 512)
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

    #[test]
    fn terminal_qr_fits_a_short_string_and_skips_a_dense_one() {
        let small = terminal_qr("hi", 80, 40).expect("short qr");
        assert!(small.contains('▀') || small.contains('▄') || small.contains('█'));
        let dense = format!("vpn://{}", "A".repeat(2048));
        assert!(terminal_qr(&dense, 40, 20).is_none());
    }

    #[test]
    fn write_qr_png_starts_with_the_png_magic() {
        let path =
            std::env::temp_dir().join(format!("awg-mesh-tui-qr-{}-hi.png", std::process::id()));
        write_qr_png("hi", &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}
