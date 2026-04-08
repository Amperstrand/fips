//! BLE transport address parsing and formatting.
//!
//! Address format: `"hci0/AA:BB:CC:DD:EE:FF"` — adapter name / device address.
//!
//! On Linux (BlueZ): device is a MAC address (AA:BB:CC:DD:EE:FF).
//! On macOS (CoreBluetooth): device is a UUID (XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX).

use crate::transport::{TransportAddr, TransportError};

/// A parsed BLE device address.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BleDeviceAddr {
    /// 6-byte Bluetooth device address (Linux/BlueZ).
    Mac([u8; 6]),
    /// 16-byte UUID (macOS/CoreBluetooth).
    Uuid([u8; 16]),
}

/// A parsed BLE address.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BleAddr {
    /// HCI adapter name (e.g., "hci0").
    pub adapter: String,
    /// Device address (MAC or UUID depending on platform).
    pub device: BleDeviceAddr,
}

impl BleAddr {
    /// Parse a BLE address from the `"adapter/AA:BB:CC:DD:EE:FF"` or `"adapter/XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX"` format.
    pub fn parse(s: &str) -> Result<Self, TransportError> {
        let (adapter, addr_str) = s.split_once('/').ok_or_else(|| {
            TransportError::InvalidAddress(format!("missing '/' in BLE address: {s}"))
        })?;

        if adapter.is_empty() {
            return Err(TransportError::InvalidAddress("empty adapter name".into()));
        }

        let device = parse_ble_addr(addr_str).ok_or_else(|| {
            TransportError::InvalidAddress(format!("invalid BLE address: {addr_str}"))
        })?;

        Ok(Self {
            adapter: adapter.to_string(),
            device,
        })
    }

    /// Format as `"adapter/AA:BB:CC:DD:EE:FF"` or `"adapter/xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"`.
    pub fn to_string_repr(&self) -> String {
        match &self.device {
            BleDeviceAddr::Mac(mac) => format!(
                "{}/{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                self.adapter, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
            ),
            BleDeviceAddr::Uuid(uuid) => {
                format!(
                    "{}/{}",
                    self.adapter,
                    format!(
                        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                        uuid[0], uuid[1], uuid[2], uuid[3],
                        uuid[4], uuid[5],
                        uuid[6], uuid[7],
                        uuid[8], uuid[9],
                        uuid[10], uuid[11], uuid[12], uuid[13], uuid[14], uuid[15]
                    )
                )
            }
        }
    }

    /// Convert to a `TransportAddr` (string representation).
    pub fn to_transport_addr(&self) -> TransportAddr {
        TransportAddr::from_string(&self.to_string_repr())
    }
}

// ============================================================================
// bluer type conversions (behind ble feature)
// ============================================================================

#[cfg(all(feature = "ble", target_os = "linux"))]
impl BleAddr {
    /// Construct from a bluer `Address` and adapter name.
    pub fn from_bluer(addr: bluer::Address, adapter: &str) -> Self {
        Self {
            adapter: adapter.to_string(),
            device: BleDeviceAddr::Mac(addr.0),
        }
    }

    /// Convert to a bluer `Address`.
    pub fn to_bluer_address(&self) -> bluer::Address {
        match self.device {
            BleDeviceAddr::Mac(mac) => bluer::Address(mac),
            BleDeviceAddr::Uuid(_) => panic!("Cannot convert UUID device address to bluer Address"),
        }
    }

    /// Convert to a bluer L2CAP `SocketAddr` with the given PSM.
    pub fn to_socket_addr(&self, psm: u16) -> bluer::l2cap::SocketAddr {
        bluer::l2cap::SocketAddr::new(self.to_bluer_address(), bluer::AddressType::LeRandom, psm)
    }
}

// ============================================================================
// bluest type conversions (behind ble-macos feature)
// ============================================================================

#[cfg(feature = "ble-macos")]
impl BleAddr {
    /// Construct from a bluest `DeviceId` and adapter name.
    pub fn from_bluest(device_id: bluest::DeviceId, adapter: &str) -> Self {
        let id_str = device_id.to_string();
        let uuid = uuid_from_bluest_device_id(&id_str);
        Self {
            adapter: adapter.to_string(),
            device: BleDeviceAddr::Uuid(uuid),
        }
    }
}

#[cfg(feature = "ble-macos")]
fn uuid_from_bluest_device_id(id_str: &str) -> [u8; 16] {
    let uuid = uuid::Uuid::parse_str(id_str).unwrap_or_else(|_| uuid::Uuid::nil());
    *uuid.as_bytes()
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

/// Parse a hyphenated UUID string into 16 bytes.
fn parse_uuid(s: &str) -> Option<[u8; 16]> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return None;
    }
    if parts[0].len() != 8
        || parts[1].len() != 4
        || parts[2].len() != 4
        || parts[3].len() != 4
        || parts[4].len() != 12
    {
        return None;
    }
    let mut uuid = [0u8; 16];
    let mut offset = 0;

    for part in parts {
        for byte_str in part.as_bytes().chunks_exact(2) {
            if byte_str.len() == 2 {
                uuid[offset] = u8::from_str_radix(std::str::from_utf8(byte_str).ok()?, 16).ok()?;
                offset += 1;
            }
        }
    }
    Some(uuid)
}

/// Parse a BLE address string into BleDeviceAddr (MAC or UUID).
fn parse_ble_addr(s: &str) -> Option<BleDeviceAddr> {
    // Try parsing as UUID first (more specific pattern)
    let uuid = parse_uuid(s);
    if uuid.is_some() {
        return uuid.map(BleDeviceAddr::Uuid);
    }

    // Fall back to MAC address parsing
    parse_mac(s).map(BleDeviceAddr::Mac)
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
        assert_eq!(
            addr.device,
            BleDeviceAddr::Mac([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
        );
    }

    #[test]
    fn test_parse_lowercase() {
        let addr = BleAddr::parse("hci1/aa:bb:cc:dd:ee:ff").unwrap();
        assert_eq!(addr.adapter, "hci1");
        assert_eq!(
            addr.device,
            BleDeviceAddr::Mac([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
        );
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

    // UUID parsing tests
    #[test]
    fn test_parse_uuid_valid() {
        let addr = BleAddr::parse("hci0/12345678-1234-1234-1234-123456789ABC").unwrap();
        assert_eq!(addr.adapter, "hci0");
        assert_eq!(
            addr.device,
            BleDeviceAddr::Uuid([
                0x12, 0x34, 0x56, 0x78, 0x12, 0x34, 0x12, 0x34, 0x12, 0x34, 0x12, 0x34, 0x56, 0x78,
                0x9A, 0xBC
            ])
        );
    }

    #[test]
    fn test_parse_uuid_lowercase() {
        let addr = BleAddr::parse("hci1/abcd1234-ef56-7890-abcd-ef1234567890").unwrap();
        assert_eq!(addr.adapter, "hci1");
        assert_eq!(
            addr.device,
            BleDeviceAddr::Uuid([
                0xAB, 0xCD, 0x12, 0x34, 0xEF, 0x56, 0x78, 0x90, 0xAB, 0xCD, 0xEF, 0x12, 0x34, 0x56,
                0x78, 0x90
            ])
        );
    }

    #[test]
    fn test_parse_uuid_uppercase() {
        let addr = BleAddr::parse("hci2/DEADBEEF-CAFE-BABE-DEAD-BEFEDEADBEEF").unwrap();
        assert_eq!(
            addr.device,
            BleDeviceAddr::Uuid([
                0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE, 0xDE, 0xAD, 0xBE, 0xFE, 0xDE, 0xAD,
                0xBE, 0xEF
            ])
        );
    }

    #[test]
    fn test_parse_uuid_roundtrip() {
        let original = "hci0/12345678-1234-1234-1234-123456789ABC";
        let addr = BleAddr::parse(original).unwrap();
        assert_eq!(addr.to_string_repr(), original);
    }

    #[test]
    fn test_parse_uuid_invalid_format() {
        assert!(BleAddr::parse("hci0/12345678-1234-1234").is_err()); // missing hyphens
        assert!(BleAddr::parse("hci0/12345678-1234-1234-1234-123456789").is_err()); // UUID too short
        assert!(BleAddr::parse("hci0/12345678-1234-1234-1234-123456789ABCD").is_err());
        // UUID too long
    }

    #[test]
    fn test_parse_uuid_empty() {
        assert!(BleAddr::parse("hci0/").is_err());
        assert!(BleAddr::parse("hci0/-").is_err());
        assert!(BleAddr::parse("hci0/-1234-1234-1234-123456789ABC").is_err());
    }

    #[test]
    fn test_parse_uuid_invalid_hex() {
        assert!(BleAddr::parse("hci0/ZZZZZZZZ-ZZZZ-ZZZZ-ZZZZ-ZZZZZZZZZZZZ").is_err());
    }

    // UUID formatting tests
    #[test]
    fn test_to_string_repr_uuid() {
        let addr = BleAddr::parse("hci0/12345678-1234-1234-1234-123456789ABC").unwrap();
        let repr = addr.to_string_repr();
        assert!(repr.contains("12345678-1234-1234-1234-123456789ABC"));
        assert_eq!(
            addr.to_string_repr(),
            "hci0/12345678-1234-1234-1234-123456789ABC"
        );
    }

    // Round-trip tests for UUID
    #[test]
    fn test_roundtrip_uuid() {
        let original = "hci1/DEADBEEF-CAFE-BABE-DEAD-BEFEDEADBEEF";
        let addr = BleAddr::parse(original).unwrap();
        assert_eq!(addr.to_string_repr(), original);
    }
}
