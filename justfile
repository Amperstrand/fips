# FIPS build recipes — platform-aware so `just build` always produces
# a binary with all available transports including BLE.
#
# macOS needs --features ble-macos for BLE (CoreBluetooth via bluest).
# Linux auto-detects BlueZ in build.rs; no extra flags required.

ble_features := if os() == "macos" { "--features ble-macos" } else { "" }

# Default: build debug with all platform transports
build:
    cargo build {{ ble_features }}

# Release build
build-release:
    cargo build --release {{ ble_features }}

# Quick type-check
check:
    cargo check {{ ble_features }}

# Run all tests
test:
    cargo test {{ ble_features }}

# Clippy lint
lint:
    cargo clippy {{ ble_features }}

# Clean build artifacts
clean:
    cargo clean

# Build + run with a given config (usage: just run /tmp/fips.yaml)
run config:
    cargo run {{ ble_features }} -- --config {{ config }}
