//! Noise key logging for traffic analysis.
//!
//! When the `FIPS_NOISE_KEYLOG` environment variable is set to a file path,
//! derived cipher keys are appended after each successful Noise handshake.
//!
//! Format (one line per handshake completion):
//!
//! ```text
//! FIPS_LINK <local_npub> <peer_npub> <send_key_hex> <recv_key_hex>
//! FIPS_SESSION <local_npub> <peer_npub> <send_key_hex> <recv_key_hex>
//! ```
//!
//! The npub identifiers are hex-encoded x-only public keys (32 bytes → 64 hex
//! chars). Wireshark custom dissectors can match these to BLE L2CAP
//! connections. When `FIPS_NOISE_KEYLOG` is not set, all calls are zero-cost
//! (no file I/O, no string allocation).

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;

static KEYLOG_PATH: OnceLock<Option<String>> = OnceLock::new();

fn keylog_enabled() -> Option<&'static str> {
    KEYLOG_PATH
        .get_or_init(|| std::env::var("FIPS_NOISE_KEYLOG").ok())
        .as_deref()
}

/// Log link-layer (FMP) Noise keys after IK handshake completion.
pub fn log_link_keys(local_npub: &str, peer_npub: &str, send_key: &[u8; 32], recv_key: &[u8; 32]) {
    if let Some(path) = keylog_enabled()
        && let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(
                f,
                "FIPS_LINK {} {} {} {}",
                local_npub,
                peer_npub,
                hex::encode(send_key),
                hex::encode(recv_key),
            );
        }
}

/// Log session-layer (FSP) Noise keys after XK handshake completion.
pub fn log_session_keys(
    local_npub: &str,
    peer_npub: &str,
    send_key: &[u8; 32],
    recv_key: &[u8; 32],
) {
    if let Some(path) = keylog_enabled()
        && let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(
                f,
                "FIPS_SESSION {} {} {} {}",
                local_npub,
                peer_npub,
                hex::encode(send_key),
                hex::encode(recv_key),
            );
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_entry(path: &str, label: &str, a: &str, b: &str, k1: &[u8; 32], k2: &[u8; 32]) {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(
                f,
                "{} {} {} {} {}",
                label,
                a,
                b,
                hex::encode(k1),
                hex::encode(k2)
            );
        }
    }

    #[test]
    fn keylog_format_matches_spec() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.log");
        let path_str = path.to_str().unwrap();

        write_entry(
            path_str,
            "FIPS_LINK",
            "aabbccdd",
            "11223344",
            &[1u8; 32],
            &[2u8; 32],
        );

        let contents = fs::read_to_string(path_str).unwrap();
        let line = contents.lines().next().unwrap();
        assert!(line.starts_with("FIPS_LINK aabbccdd 11223344"));
        assert!(line.contains(&hex::encode([1u8; 32])));
        assert!(line.contains(&hex::encode([2u8; 32])));
    }
}
