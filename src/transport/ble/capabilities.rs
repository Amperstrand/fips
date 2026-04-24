#[derive(Debug, Clone, Copy, Default)]
pub struct PeerCapabilities(u8);

impl PeerCapabilities {
    const LEGACY_CENTRAL_ONLY: u8 = 0x01;
    const PREFER_OUTBOUND: u8 = 0x02;
    const PREFER_L2CAP: u8 = 0x04;
    const CAN_CENTRAL: u8 = 0x08;
    const CAN_PERIPHERAL: u8 = 0x10;
    const L2CAP_SUPPORTED: u8 = 0x20;
    const GATT_SUPPORTED: u8 = 0x40;

    pub fn none() -> Self {
        Self(0)
    }

    pub fn linux_default() -> Self {
        Self(
            Self::L2CAP_SUPPORTED
                | Self::CAN_CENTRAL
                | Self::CAN_PERIPHERAL
                | Self::GATT_SUPPORTED
                | Self::PREFER_L2CAP,
        )
    }

    pub fn central_only() -> Self {
        Self(Self::L2CAP_SUPPORTED | Self::CAN_CENTRAL | Self::PREFER_OUTBOUND)
    }

    pub fn peripheral_only() -> Self {
        Self(Self::L2CAP_SUPPORTED | Self::CAN_PERIPHERAL | Self::GATT_SUPPORTED)
    }

    pub fn macos_default() -> Self {
        Self(
            Self::L2CAP_SUPPORTED
                | Self::CAN_CENTRAL
                | Self::CAN_PERIPHERAL
                | Self::GATT_SUPPORTED
                | Self::PREFER_OUTBOUND,
        )
    }

    pub fn is_central_only(&self) -> bool {
        self.can_initiate_outbound() && !self.can_accept_inbound()
    }

    pub fn can_accept_inbound(&self) -> bool {
        self.is_legacy_unrestricted() || (self.0 & Self::CAN_PERIPHERAL != 0)
    }

    pub fn can_initiate_outbound(&self) -> bool {
        self.is_legacy_unrestricted() || (self.0 & Self::CAN_CENTRAL != 0)
    }

    pub fn prefers_outbound(&self) -> bool {
        self.0 & Self::PREFER_OUTBOUND != 0
    }

    pub fn to_byte(self) -> u8 {
        self.0
    }

    pub fn from_byte(byte: u8) -> Self {
        if byte == Self::LEGACY_CENTRAL_ONLY {
            return Self::central_only();
        }
        Self(byte)
    }

    fn is_legacy_unrestricted(&self) -> bool {
        self.0 == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_has_no_capabilities() {
        let caps = PeerCapabilities::none();
        assert!(!caps.is_central_only());
        assert!(caps.can_accept_inbound());
        assert!(caps.can_initiate_outbound());
        assert!(!caps.prefers_outbound());
    }

    #[test]
    fn none_is_legacy_unrestricted() {
        let caps = PeerCapabilities::from_byte(0);
        assert!(caps.can_accept_inbound());
        assert!(caps.can_initiate_outbound());
        assert!(!caps.is_central_only());
    }

    #[test]
    fn central_only_flag_maps_correctly() {
        let caps = PeerCapabilities::from_byte(0x01);
        assert!(caps.is_central_only());
        assert!(!caps.can_accept_inbound());
        assert!(caps.can_initiate_outbound());
        assert!(caps.prefers_outbound());
    }

    #[test]
    fn macos_default_has_full_capabilities() {
        let caps = PeerCapabilities::macos_default();
        assert!(!caps.is_central_only());
        assert!(caps.can_accept_inbound());
        assert!(caps.can_initiate_outbound());
        assert!(caps.prefers_outbound());
    }

    #[test]
    fn linux_default_has_full_capabilities() {
        let caps = PeerCapabilities::linux_default();
        assert!(!caps.is_central_only());
        assert!(caps.can_accept_inbound());
        assert!(caps.can_initiate_outbound());
        assert!(!caps.prefers_outbound());
    }

    #[test]
    fn peripheral_only_has_peripheral_capabilities() {
        let caps = PeerCapabilities::peripheral_only();
        assert!(!caps.is_central_only());
        assert!(caps.can_accept_inbound());
        assert!(!caps.can_initiate_outbound());
        assert!(!caps.prefers_outbound());
    }

    #[test]
    fn roundtrip_byte() {
        let caps = PeerCapabilities::macos_default();
        let byte = caps.to_byte();
        let restored = PeerCapabilities::from_byte(byte);
        assert_eq!(caps.to_byte(), restored.to_byte());
    }
}
