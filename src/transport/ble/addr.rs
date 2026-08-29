//! BLE transport address parsing and formatting.
//!
//! Address format: `"hci0/AA:BB:CC:DD:EE:FF"` — adapter name / device address.

use crate::transport::{TransportAddr, TransportError};

/// A parsed BLE device address.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BleAddr {
    /// HCI adapter name (e.g., "hci0").
    pub adapter: String,
    /// 6-byte Bluetooth device address.
    pub device: [u8; 6],
    /// Peer address type learned from discovery (or accept).
    ///
    /// The L2CAP socket dial must use the type the peer actually advertises
    /// with: the kernel never completes a connection whose peer-address type
    /// mismatches (it fails at mgmt level without ever issuing
    /// `LE Create Connection`, and — as observed with bluer 0.17 — the
    /// socket connect future is never told, so the dialer only ever sees
    /// its own timeout). ESP32 peers advertise as Random; a hardcoded
    /// LePublic made every probe against them time out.
    pub kind: BleAddrKind,
}

/// BLE peer address type, mirroring the two LE types the kernel dials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BleAddrKind {
    Public,
    Random,
}

impl BleAddr {
    /// Parse a BLE address from the `"adapter/AA:BB:CC:DD:EE:FF"` format.
    pub fn parse(s: &str) -> Result<Self, TransportError> {
        let (adapter, mac_str) = s.split_once('/').ok_or_else(|| {
            TransportError::InvalidAddress(format!("missing '/' in BLE address: {s}"))
        })?;

        if adapter.is_empty() {
            return Err(TransportError::InvalidAddress("empty adapter name".into()));
        }

        let device = parse_mac(mac_str).ok_or_else(|| {
            TransportError::InvalidAddress(format!("invalid MAC address: {mac_str}"))
        })?;

        Ok(Self {
            adapter: adapter.to_string(),
            device,
            // Config-pinned addresses carry no type information; Public
            // preserves the pre-`kind` dial behavior for static peers.
            kind: BleAddrKind::Public,
        })
    }

    /// Format as `"adapter/AA:BB:CC:DD:EE:FF"`.
    pub fn to_string_repr(&self) -> String {
        format!(
            "{}/{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            self.adapter,
            self.device[0],
            self.device[1],
            self.device[2],
            self.device[3],
            self.device[4],
            self.device[5],
        )
    }

    /// Convert to a `TransportAddr` (string representation).
    pub fn to_transport_addr(&self) -> TransportAddr {
        TransportAddr::from_string(&self.to_string_repr())
    }
}

// ============================================================================
// bluer type conversions (glibc-linux only; see build.rs bluer_available)
// ============================================================================

#[cfg(bluer_available)]
impl From<bluer::AddressType> for BleAddrKind {
    fn from(ty: bluer::AddressType) -> Self {
        match ty {
            bluer::AddressType::LePublic => BleAddrKind::Public,
            bluer::AddressType::LeRandom => BleAddrKind::Random,
            // BR/EDR is not a dialable LE type; Public keeps legacy behavior.
            bluer::AddressType::BrEdr => BleAddrKind::Public,
        }
    }
}

#[cfg(bluer_available)]
impl BleAddr {
    /// Construct from a bluer `Address` and adapter name, learning the peer
    /// address type from discovery/accept.
    pub fn from_bluer_with_kind(
        addr: bluer::Address,
        kind: BleAddrKind,
        adapter: &str,
    ) -> Self {
        Self {
            adapter: adapter.to_string(),
            device: addr.0,
            kind,
        }
    }

    /// Convert to a bluer `Address`.
    pub fn to_bluer_address(&self) -> bluer::Address {
        bluer::Address(self.device)
    }

    /// Convert to a bluer L2CAP `SocketAddr` with the given PSM.
    pub fn to_socket_addr(&self, psm: u16) -> bluer::l2cap::SocketAddr {
        let ty = match self.kind {
            BleAddrKind::Public => bluer::AddressType::LePublic,
            BleAddrKind::Random => bluer::AddressType::LeRandom,
        };
        bluer::l2cap::SocketAddr::new(self.to_bluer_address(), ty, psm)
    }
}

impl std::fmt::Display for BleAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_string_repr())
    }
}

/// Parse a colon-delimited MAC address string into 6 bytes.
fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        mac[i] = u8::from_str_radix(part, 16).ok()?;
    }
    Some(mac)
}

/// Extract the adapter name from a transport address string.
///
/// Returns `None` if the address is not valid UTF-8 or doesn't contain '/'.
pub fn adapter_from_addr(addr: &TransportAddr) -> Option<&str> {
    addr.as_str()?.split_once('/').map(|(adapter, _)| adapter)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid() {
        let addr = BleAddr::parse("hci0/AA:BB:CC:DD:EE:FF").unwrap();
        assert_eq!(addr.adapter, "hci0");
        assert_eq!(addr.device, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn test_parse_lowercase() {
        let addr = BleAddr::parse("hci1/aa:bb:cc:dd:ee:ff").unwrap();
        assert_eq!(addr.adapter, "hci1");
        assert_eq!(addr.device, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn test_roundtrip() {
        let original = "hci0/AA:BB:CC:DD:EE:FF";
        let addr = BleAddr::parse(original).unwrap();
        assert_eq!(addr.to_string_repr(), original);
    }

    #[test]
    fn test_display() {
        let addr = BleAddr::parse("hci0/01:02:03:04:05:06").unwrap();
        assert_eq!(format!("{addr}"), "hci0/01:02:03:04:05:06");
    }

    #[test]
    fn test_to_transport_addr() {
        let addr = BleAddr::parse("hci0/AA:BB:CC:DD:EE:FF").unwrap();
        let ta = addr.to_transport_addr();
        assert_eq!(ta.as_str(), Some("hci0/AA:BB:CC:DD:EE:FF"));
    }

    #[test]
    fn test_parse_missing_slash() {
        assert!(BleAddr::parse("hci0-AA:BB:CC:DD:EE:FF").is_err());
    }

    #[test]
    fn test_parse_empty_adapter() {
        assert!(BleAddr::parse("/AA:BB:CC:DD:EE:FF").is_err());
    }

    #[test]
    fn test_parse_invalid_mac_short() {
        assert!(BleAddr::parse("hci0/AA:BB:CC").is_err());
    }

    #[test]
    fn test_parse_invalid_mac_hex() {
        assert!(BleAddr::parse("hci0/GG:HH:II:JJ:KK:LL").is_err());
    }

    #[test]
    fn test_adapter_from_addr() {
        let ta = TransportAddr::from_string("hci0/AA:BB:CC:DD:EE:FF");
        assert_eq!(adapter_from_addr(&ta), Some("hci0"));
    }

    #[test]
    fn test_adapter_from_addr_no_slash() {
        let ta = TransportAddr::from_string("invalid");
        assert_eq!(adapter_from_addr(&ta), None);
    }

    #[cfg(bluer_available)]
    #[test]
    fn test_parse_defaults_to_public_kind() {
        let addr = BleAddr::parse("hci0/0C:00:00:00:00:FF").unwrap();
        assert_eq!(addr.kind, BleAddrKind::Public);
        assert_eq!(
            addr.to_socket_addr(133).addr_type,
            bluer::AddressType::LePublic
        );
    }

    #[cfg(bluer_available)]
    #[test]
    fn test_learned_random_kind_drives_socket_addr() {
        let addr = bluer::Address([0x0C, 0, 0, 0, 0, 0xFF]);
        let ble = BleAddr::from_bluer_with_kind(addr, BleAddrKind::Random, "hci0");
        let sa = ble.to_socket_addr(133);
        assert_eq!(sa.addr, addr);
        assert_eq!(sa.addr_type, bluer::AddressType::LeRandom);
        assert_eq!(sa.psm, 133);
    }

    #[cfg(bluer_available)]
    #[test]
    fn test_bluer_address_type_conversion() {
        assert_eq!(BleAddrKind::from(bluer::AddressType::LeRandom), BleAddrKind::Random);
        assert_eq!(BleAddrKind::from(bluer::AddressType::LePublic), BleAddrKind::Public);
    }
}
