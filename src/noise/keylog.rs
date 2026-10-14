//! LAB-ONLY transport-key logging for bench capture decryption.
//!
//! Active only when `FIPS_LAB_KEYLOG` names a file to append to; with the
//! variable unset — the only supported configuration outside the bench —
//! [`transport_keys`] returns without touching any state. On the bench the
//! point is the reverse of production hygiene: capture confidentiality is
//! worthless, and a packet capture nobody can decrypt is a capture nobody
//! can debug.
//!
//! One JSON line is appended each time a handshake completes. Because a
//! rekey in this codebase is a fresh handshake, the same hook covers every
//! rekey. `split()` is symmetric, so `k1` is the initiator→responder key
//! and `k2` the responder→initiator key on BOTH lines of a pair — a
//! decryptor reads either line and gets both directions. `h` (the
//! handshake hash) is identical on both sides, which joins the two lines
//! of one handshake into a pair.

use std::io::Write;
use std::path::Path;

/// Append one completed handshake's transport keys to the lab keylog.
///
/// `pattern` is "ik" (link layer) or "xk" (session layer), `role` is
/// "initiator" or "responder", and `peer` is the remote static key in
/// compressed secp256k1 form. Write failures are ignored: a misconfigured
/// diagnostic log must never take the daemon down with it.
pub(super) fn transport_keys(
    pattern: &str,
    role: &str,
    peer: &[u8; 33],
    handshake_hash: &[u8; 32],
    k1: &[u8; 32],
    k2: &[u8; 32],
) {
    let Ok(path) = std::env::var("FIPS_LAB_KEYLOG") else {
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let line = format!(
        "{{\"ts\":{ts},\"pattern\":\"{pattern}\",\"role\":\"{role}\",\"peer\":\"{peer}\",\"h\":\"{h}\",\"k1\":\"{k1}\",\"k2\":\"{k2}\"}}\n",
        peer = hex(peer),
        h = hex(handshake_hash),
        k1 = hex(k1),
        k2 = hex(k2),
    );
    let _ = append(&path, &line);
}

fn append(path: &str, line: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(Path::new(path))?;
    file.write_all(line.as_bytes())
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(TABLE[usize::from(b >> 4)] as char);
        out.push(TABLE[usize::from(b & 0x0f)] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encodes_each_byte_as_two_lowercase_digits() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(hex(&[]), "");
    }
}
