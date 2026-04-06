//! macOS-specific BLE integration tests using BluestIo.
//!
//! These tests require macOS with Bluetooth hardware and the `ble-macos` feature.
//! They won't run in CI but can be run locally with:
//!     cargo test --features ble-macos --test ble_macos

#[cfg(all(target_os = "macos", feature = "ble-macos"))]
mod tests {
    use crate::transport::ble::io::{BluestIo, BleIo};
    use crate::transport::TransportError;

    #[tokio::test]
    async fn test_bluest_io_new_succeeds() {
        let result = BluestIo::new("default", 2048).await;
        assert!(result.is_ok(), "BluestIo::new should succeed on macOS with Bluetooth");
    }

    #[tokio::test]
    async fn test_bluest_io_start_scanning_succeeds() {
        let io = BluestIo::new("default", 2048).await.expect("BluestIo::new");
        let result = io.start_scanning().await;
        assert!(result.is_ok(), "start_scanning should succeed");
    }

    #[tokio::test]
    async fn test_bluest_io_listen_returns_not_supported() {
        let io = BluestIo::new("default", 2048).await.expect("BluestIo::new");
        let result = io.listen(0x0085).await;
        match result {
            Err(TransportError::NotSupported(msg)) => {
                assert!(msg.contains("central-only"));
            }
            _ => panic!("Expected NotSupported error, got {:?}", result),
        }
    }

    #[tokio::test]
    async fn test_bluest_io_start_advertising_returns_not_supported() {
        let io = BluestIo::new("default", 2048).await.expect("BluestIo::new");
        let result = io.start_advertising().await;
        match result {
            Err(TransportError::NotSupported(msg)) => {
                assert!(msg.contains("central-only"));
            }
            _ => panic!("Expected NotSupported error, got {:?}", result),
        }
    }

    #[tokio::test]
    async fn test_bluest_io_stop_advertising_returns_not_supported() {
        let io = BluestIo::new("default", 2048).await.expect("BluestIo::new");
        let result = io.stop_advertising().await;
        match result {
            Err(TransportError::NotSupported(msg)) => {
                assert!(msg.contains("central-only"));
            }
            _ => panic!("Expected NotSupported error, got {:?}", result),
        }
    }

    #[tokio::test]
    async fn test_bluest_io_local_addr_succeeds() {
        let io = BluestIo::new("default", 2048).await.expect("BluestIo::new");
        let result = io.local_addr();
        assert!(result.is_ok(), "local_addr should succeed");
        let addr = result.unwrap();
        assert_eq!(addr.adapter, "default");
    }

    #[tokio::test]
    async fn test_bluest_io_adapter_name() {
        let io = BluestIo::new("default", 2048).await.expect("BluestIo::new");
        assert_eq!(io.adapter_name(), "default");
    }
}
