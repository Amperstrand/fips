# Decisions — GATT PSM Exchange Plan

## 2026-04-13 Plan Approved
- Momus verdict: OKAY — no blocking issues
- Scanner-based detection (Option C): detect FIPS_GATT_PSM_SERVICE_UUID in advertisements
- macOS peripheral uses objc2-core-bluetooth directly (bluest lacks GATT server)
- GATT_SUPPORTED set on both macOS and Linux defaults
- macOS also gets CAN_PERIPHERAL since it can now accept inbound

## GitHub Issue Created
- Issue #52: "BLE: GATT PSM exchange for macOS↔macOS and Linux→macOS connectivity"
- URL: https://github.com/Amperstrand/fips/issues/52
- Created 2026-04-13
