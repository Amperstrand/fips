# BLE Bulk Stability — Issues

## 2026-04-17 Session Start

### Known Issues
- xHCI controller death after `systemctl restart bluetooth` (Amperstrand/fips#63) — requires reboot
- CoreBluetooth peripheral mode rejects SMP pairing (Amperstrand/fips#64) — unencrypted L2CAP used
- GATT-first connect sometimes aborts on first attempt (Amperstrand/fips#65)
- ESP32 firmware never sends data back after bloom filter MTU skip fix (Amperstrand/fips#66)
