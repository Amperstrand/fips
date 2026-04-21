#!/usr/bin/env bash
# FIPS BLE Cross-Platform Test Suite
#
# Comprehensive test of BLE connectivity between Mac and Linux in both
# role configurations. Captures traffic, runs benchmarks, analyzes logs.
#
# Usage:
#   ./testing/ble-cross-test.sh [--phase 1|2|3|4|all] [--skip-swap]
#
# Prerequisites:
#   - Mac FIPS built with: cargo build --release --features "ble-macos benchmark"
#   - Linux FIPS built with: cargo build --release --features "ble benchmark"
#   - SSH access to Linux host (configured as 'ssh 218')
#   - btmon installed on Linux (sudo apt install bluez-tools)
#
# Key npubs:
#   Mac:   npub150gmh6m3gqcgdlasvhdfntqty8v4je4cl6lhg9rj5th2l3zyn8fq3x78an
#   Linux: npub14pwsvvzsdty7xvz8cr9y74harz2xmxqhl87ympt2qz28qh4l0m6suassx4

set -euo pipefail

# --- Configuration ---
MAC_NPUB="npub150gmh6m3gqcgdlasvhdfntqty8v4je4cl6lhg9rj5th2l3zyn8fq3x78an"
LINUX_NPUB="npub14pwsvvzsdty7xvz8cr9y74harz2xmxqhl87ympt2qz28qh4l0m6suassx4"
MAC_SOCKET="/tmp/fips-control.sock"
LINUX_SOCKET="/run/fips/control.sock"
MAC_BIN="/Users/macbook/src/fips/target/release/fipsctl"
MAC_FIPS_BIN="/Users/macbook/src/fips/target/release/fips"
MAC_CONFIG="/tmp/fips-logs/opt-test-mac.yaml"
MAC_LOG="/tmp/fips-logs/fips.log"
LINUX_SSH="218"
RESULTS_DIR="/tmp/fips-logs/ble-test-results"
CAPTURE_FILE="$RESULTS_DIR/btmon-capture.bin"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)

# Echo benchmark sizes to test
ECHO_SIZES=(64 256 512 900)
ECHO_COUNT=20

# Throughput test params
TP_DURATION=10
TP_FRAME_SIZE=256
TP_RATE=40000

# --- Helpers ---
log()  { echo "[$(date +%H:%M:%S)] [INFO]  $*"; }
warn() { echo "[$(date +%H:%M:%S)] [WARN]  $*" >&2; }
pass() { echo "[$(date +%H:%M:%S)] [PASS]  $*"; }
fail() { echo "[$(date +%H:%M:%S)] [FAIL]  $*" >&2; TEST_FAILS=$((TEST_FAILS + 1)); }

TEST_FAILS=0
PHASE="${PHASE:-all}"
SKIP_SWAP="${SKIP_SWAP:-false}"

mkdir -p "$RESULTS_DIR"

mac_fipsctl() {
    sudo "$MAC_BIN" --socket "$MAC_SOCKET" "$@"
}

linux_fipsctl() {
    ssh "$LINUX_SSH" "sudo /usr/local/bin/fipsctl --socket $LINUX_SOCKET $*" 2>/dev/null
}

wait_for_peer() {
    local side="$1"
    local timeout="${2:-120}"
    local elapsed=0
    log "Waiting for BLE peer connection on $side (timeout ${timeout}s)..."
    while [ $elapsed -lt $timeout ]; do
        if [ "$side" = "mac" ]; then
            local count=$($MAC_BIN --socket "$MAC_SOCKET" show peers 2>/dev/null | grep -c '"connectivity": "connected"' || true)
        else
            local count=$(linux_fipsctl show peers | grep -c '"connectivity": "connected"' || true)
        fi
        if [ "$count" -gt 0 ]; then
            log "Peer connected on $side after ${elapsed}s"
            return 0
        fi
        sleep 5
        elapsed=$((elapsed + 5))
    done
    log "ERROR: No peer connected on $side after ${timeout}s"
    return 1
}

run_echo_benchmark() {
    local direction="$1"  # "mac-to-linux" or "linux-to-mac"
    local size="$2"
    local count="$3"
    local label="echo-${direction}-s${size}-n${count}"

    log "Running echo benchmark: $label"
    if [ "$direction" = "mac-to-linux" ]; then
        $MAC_BIN --socket "$MAC_SOCKET" benchmark echo -n "$count" -s "$size" "$LINUX_NPUB" \
            > "$RESULTS_DIR/${TIMESTAMP}-${label}.json" 2>&1
    else
        ssh "$LINUX_SSH" "sudo /usr/local/bin/fipsctl --socket $LINUX_SOCKET benchmark echo -n $count -s $size $MAC_NPUB" \
            > "$RESULTS_DIR/${TIMESTAMP}-${label}.json" 2>&1
    fi
    local rc=$?
    if [ $rc -eq 0 ]; then
        log "  ✅ $label completed"
    else
        log "  ❌ $label failed (exit $rc)"
    fi
    return $rc
}

run_throughput_benchmark() {
    local direction="$1"
    local label="throughput-${direction}-f${TP_FRAME_SIZE}-r${TP_RATE}-t${TP_DURATION}"

    log "Running throughput benchmark: $label"
    if [ "$direction" = "mac-to-linux" ]; then
        $MAC_BIN --socket "$MAC_SOCKET" benchmark throughput \
            -d upload -t "$TP_DURATION" -f "$TP_FRAME_SIZE" -r "$TP_RATE" "$LINUX_NPUB" \
            > "$RESULTS_DIR/${TIMESTAMP}-${label}.json" 2>&1
    else
        # "linux-to-mac upload" means Linux uploads to Mac
        ssh "$LINUX_SSH" "sudo /usr/local/bin/fipsctl --socket $LINUX_SOCKET benchmark throughput -d upload -t $TP_DURATION -f $TP_FRAME_SIZE -r $TP_RATE $MAC_NPUB" \
            > "$RESULTS_DIR/${TIMESTAMP}-${label}.json" 2>&1
    fi
    local rc=$?
    if [ $rc -eq 0 ]; then
        log "  ✅ $label completed"
    else
        log "  ❌ $label failed (exit $rc)"
    fi
    return $rc
}

collect_stats() {
    local phase="$1"
    log "Collecting stats for phase: $phase"
    $MAC_BIN --socket "$MAC_SOCKET" show bloom > "$RESULTS_DIR/${TIMESTAMP}-${phase}-mac-bloom.json" 2>&1 || true
    $MAC_BIN --socket "$MAC_SOCKET" show peers > "$RESULTS_DIR/${TIMESTAMP}-${phase}-mac-peers.json" 2>&1 || true
    $MAC_BIN --socket "$MAC_SOCKET" show status > "$RESULTS_DIR/${TIMESTAMP}-${phase}-mac-status.json" 2>&1 || true
    linux_fipsctl show bloom > "$RESULTS_DIR/${TIMESTAMP}-${phase}-linux-bloom.json" 2>&1 || true
    linux_fipsctl show peers > "$RESULTS_DIR/${TIMESTAMP}-${phase}-linux-peers.json" 2>&1 || true
    linux_fipsctl show status > "$RESULTS_DIR/${TIMESTAMP}-${phase}-linux-status.json" 2>&1 || true
}

analyze_logs() {
    local phase="$1"
    local label="$2"
    log "Analyzing logs for $label"

    # Mac log analysis
    local mac_errors=$(grep -c -i "error\|WARN\|failed\|decrypt.*fail" "$MAC_LOG" 2>/dev/null || echo "0")
    local mac_skip=$(grep -c "Skipping FilterAnnounce" "$MAC_LOG" 2>/dev/null || echo "0")
    log "  Mac: $mac_errors errors/warnings, $mac_skip bloom MTU skips"

    # Save mac log snippet for this phase
    cp "$MAC_LOG" "$RESULTS_DIR/${TIMESTAMP}-${phase}-mac.log" 2>/dev/null || true

    # Linux log analysis
    local linux_log=$(ssh "$LINUX_SSH" "sudo journalctl -u fips --since '5 minutes ago' --no-pager" 2>/dev/null || echo "")
    echo "$linux_log" > "$RESULTS_DIR/${TIMESTAMP}-${phase}-linux.log"
    local linux_errors=$(echo "$linux_log" | grep -c -i "error\|WARN\|failed\|decrypt.*fail" || echo "0")
    local linux_skip=$(echo "$linux_log" | grep -c "Skipping FilterAnnounce" || echo "0")
    log "  Linux: $linux_errors errors/warnings, $linux_skip bloom MTU skips"
}

start_btmon_capture() {
    log "Starting btmon capture on Linux..."
    ssh "$LINUX_SSH" "sudo timeout 600 btmon -w /tmp/btmon-capture.bin &" 2>/dev/null || true
    sleep 2
}

stop_btmon_capture() {
    log "Stopping btmon capture..."
    ssh "$LINUX_SSH" "sudo pkill -f 'btmon -w' 2>/dev/null; sleep 1; sudo cp /tmp/btmon-capture.bin /tmp/btmon-capture-${TIMESTAMP}.bin 2>/dev/null || true" || true
}

restart_mac() {
    local config="$1"
    log "Restarting Mac FIPS with config: $config"
    sudo pkill -f 'fips --config' 2>/dev/null || true
    sleep 2
    sudo "$MAC_FIPS_BIN" --config "$config" > "$MAC_LOG" 2>&1 &
    sleep 5
}

restart_linux() {
    local config_content="$1"
    log "Restarting Linux FIPS with new config"
    ssh "$LINUX_SSH" "sudo chattr -i /etc/fips/fips.yaml" 2>/dev/null || true
    echo "$config_content" | ssh "$LINUX_SSH" "sudo tee /etc/fips/fips.yaml" > /dev/null 2>&1
    ssh "$LINUX_SSH" "sudo chattr +i /etc/fips/fips.yaml" 2>/dev/null || true
    ssh "$LINUX_SSH" "sudo systemctl restart fips" 2>/dev/null
    sleep 5
}

# --- Config templates ---
MAC_CONFIG_PERIPHERAL=$(cat "$MAC_CONFIG")

MAC_CONFIG_CENTRAL="node:
  identity:
    persistent: true
  control:
    enabled: true
    socket_path: /tmp/fips-control.sock
  log_level: debug
tun:
  enabled: true
  mtu: 1280
dns:
  enabled: true
  bind_addr: \"127.0.0.1\"
  port: 5354
transports:
  ble:
    adapter: \"default\"
    mtu: 1024
    accept_connections: false
    auto_connect: true
    scan: true
    advertise: false
    send_rate_bps: 80000
    send_burst_bytes: 2048"

LINUX_CONFIG_PERIPHERAL="node:
  identity:
    persistent: true
  control:
    enabled: true
    socket_path: /run/fips/control.sock
  log_level: debug
tun:
  enabled: true
  mtu: 1280
dns:
  enabled: true
  bind_addr: \"127.0.0.1\"
  port: 5354
transports:
  ble:
    adapter: \"hci0\"
    mtu: 1024
    accept_connections: true
    auto_connect: false
    scan: false
    advertise: true
    send_rate_bps: 80000
    send_burst_bytes: 2048"

# --- Phase implementations ---

phase1() {
    log "============================================"
    log "PHASE 1: Current config (Mac peripheral, Linux central)"
    log "============================================"
    collect_stats "p1-pre"
    start_btmon_capture

    log "--- Echo benchmarks: Mac → Linux ---"
    for size in "${ECHO_SIZES[@]}"; do
        run_echo_benchmark "mac-to-linux" "$size" "$ECHO_COUNT" || true
        sleep 2
    done

    log "--- Echo benchmarks: Linux → Mac ---"
    for size in "${ECHO_SIZES[@]}"; do
        run_echo_benchmark "linux-to-mac" "$size" "$ECHO_COUNT" || true
        sleep 2
    done

    log "--- Throughput: Mac → Linux ---"
    run_throughput_benchmark "mac-to-linux" || true
    sleep 2

    log "--- Throughput: Linux → Mac ---"
    run_throughput_benchmark "linux-to-mac" || true
    sleep 2

    stop_btmon_capture
    collect_stats "p1-post"
    analyze_logs "p1" "Mac peripheral / Linux central"
    log "Phase 1 complete"
}

phase2() {
    log "============================================"
    log "PHASE 2: Swapped roles (Mac central, Linux peripheral)"
    log "============================================"

    # Stop both
    log "Stopping both nodes..."
    sudo pkill -f 'fips --config' 2>/dev/null || true
    ssh "$LINUX_SSH" "sudo systemctl stop fips" 2>/dev/null || true
    sleep 5

    # Write swapped configs
    log "Writing Mac central config..."
    echo "$MAC_CONFIG_CENTRAL" > /tmp/fips-logs/opt-test-mac-central.yaml

    log "Writing Linux peripheral config..."
    restart_linux "$LINUX_CONFIG_PERIPHERAL"
    restart_mac "/tmp/fips-logs/opt-test-mac-central.yaml"

    log "Waiting for connection (Mac scanning for Linux)..."
    if ! wait_for_peer "mac" 180; then
        log "ERROR: Failed to establish connection in swapped roles"
        analyze_logs "p2-fail" "Mac central / Linux peripheral"
        return 1
    fi

    log "Connection established in swapped roles!"

    start_btmon_capture

    log "--- Echo benchmarks: Mac → Linux ---"
    for size in "${ECHO_SIZES[@]}"; do
        run_echo_benchmark "mac-to-linux" "$size" "$ECHO_COUNT" || true
        sleep 2
    done

    log "--- Echo benchmarks: Linux → Mac ---"
    for size in "${ECHO_SIZES[@]}"; do
        run_echo_benchmark "linux-to-mac" "$size" "$ECHO_COUNT" || true
        sleep 2
    done

    log "--- Throughput: Mac → Linux ---"
    run_throughput_benchmark "mac-to-linux" || true
    sleep 2

    log "--- Throughput: Linux → Mac ---"
    run_throughput_benchmark "linux-to-mac" || true
    sleep 2

    stop_btmon_capture
    collect_stats "p2-post"
    analyze_logs "p2" "Mac central / Linux peripheral"
    log "Phase 2 complete"

    # Restore original configs
    log "Restoring original configs..."
    sudo pkill -f 'fips --config' 2>/dev/null || true
    ssh "$LINUX_SSH" "sudo systemctl stop fips" 2>/dev/null || true
    sleep 5

    # Restore Linux to original (central) config
    local LINUX_CONFIG_ORIGINAL="node:
  identity:
    persistent: true
  control:
    enabled: true
    socket_path: /run/fips/control.sock
  log_level: debug
tun:
  enabled: true
  mtu: 1280
dns:
  enabled: true
  bind_addr: \"127.0.0.1\"
  port: 5354
transports:
  ble:
    adapter: \"hci0\"
    mtu: 1024
    accept_connections: true
    auto_connect: true
    scan: true
    advertise: true
    send_rate_bps: 80000
    send_burst_bytes: 2048"
    restart_linux "$LINUX_CONFIG_ORIGINAL"
    restart_mac "$MAC_CONFIG"
    log "Original configs restored, waiting for reconnection..."
    wait_for_peer "mac" 120 || true
}

phase3() {
    log "============================================"
    log "PHASE 3: Near-MTU boundary tests"
    log "============================================"

    # Check we still have a connection
    local count=$($MAC_BIN --socket "$MAC_SOCKET" show peers 2>/dev/null | grep -c '"connectivity": "connected"' || true)
    if [ "$count" -eq 0 ]; then
        log "No peer connected, waiting..."
        wait_for_peer "mac" 120 || { log "ERROR: Cannot reconnect for Phase 3"; return 1; }
    fi

    # Test near-MTU sizes: 512, 900, 950, 1000, 1020
    local mtu_sizes=(512 900 950 1000 1020)
    for size in "${mtu_sizes[@]}"; do
        log "--- MTU boundary test: Mac → Linux, size=${size} ---"
        run_echo_benchmark "mac-to-linux" "$size" "10" || true
        sleep 2
        log "--- MTU boundary test: Linux → Mac, size=${size} ---"
        run_echo_benchmark "linux-to-mac" "$size" "10" || true
        sleep 2
    done

    collect_stats "p3-post"
    analyze_logs "p3" "MTU boundary tests"
    log "Phase 3 complete"
}

phase4() {
    log "============================================"
    log "PHASE 4: Log analysis and summary"
    log "============================================"

    log "=== FINAL STATS ==="
    collect_stats "final"

    log ""
    log "=== TEST RESULTS SUMMARY ==="
    log "Results directory: $RESULTS_DIR"
    log ""
    log "--- Echo benchmark results ---"
    for f in "$RESULTS_DIR"/${TIMESTAMP}-echo-*.json; do
        [ -f "$f" ] || continue
        local name=$(basename "$f" .json | sed "s/${TIMESTAMP}-//")
        local loss=$(grep -o '"loss_rate":[^,}]*' "$f" | head -1 || echo "N/A")
        local rtt=$(grep -o '"mean_rtt_ms":[^,}]*' "$f" | head -1 || echo "N/A")
        log "  $name: $rtt, $loss"
    done

    log ""
    log "--- Throughput benchmark results ---"
    for f in "$RESULTS_DIR"/${TIMESTAMP}-throughput-*.json; do
        [ -f "$f" ] || continue
        local name=$(basename "$f" .json | sed "s/${TIMESTAMP}-//")
        local tput=$(grep -o '"goodput_bps":[^,}]*' "$f" | head -1 || echo "N/A")
        log "  $name: $tput"
    done

    log ""
    log "--- Bloom filter stats ---"
    local mac_mtu_skip=$(grep -o '"mtu_skipped": [0-9]*' "$RESULTS_DIR/${TIMESTAMP}-final-mac-bloom.json" 2>/dev/null || echo "N/A")
    local linux_mtu_skip=$(grep -o '"mtu_skipped": [0-9]*' "$RESULTS_DIR/${TIMESTAMP}-final-linux-bloom.json" 2>/dev/null || echo "N/A")
    log "  Mac mtu_skipped: $mac_mtu_skip"
    log "  Linux mtu_skipped: $linux_mtu_skip"

    log ""
    log "--- MMP delivery ratios ---"
    local mac_fwd=$(grep -o '"delivery_ratio_forward": [0-9.]*' "$RESULTS_DIR/${TIMESTAMP}-final-mac-peers.json" 2>/dev/null || echo "N/A")
    local mac_rev=$(grep -o '"delivery_ratio_reverse": [0-9.]*' "$RESULTS_DIR/${TIMESTAMP}-final-mac-peers.json" 2>/dev/null || echo "N/A")
    local mac_loss=$(grep -o '"loss_rate": [0-9.]*' "$RESULTS_DIR/${TIMESTAMP}-final-mac-peers.json" 2>/dev/null || echo "N/A")
    log "  Mac MMP: $mac_fwd, $mac_rev, $mac_loss"

    log ""
    log "Phase 4 complete. Full results in: $RESULTS_DIR/"
}

# --- Main ---
PHASE="${1:-all}"
SKIP_SWAP="${2:-false}"

mkdir -p "$RESULTS_DIR"
log "FIPS BLE Cross-Platform Test Suite"
log "Timestamp: $TIMESTAMP"
log "Results: $RESULTS_DIR"
log ""

case "$PHASE" in
    1)   phase1 ;;
    2)   phase2 ;;
    3)   phase3 ;;
    4)   phase4 ;;
    all)
        phase1
        phase2
        phase3
        phase4
        ;;
    *)
        echo "Usage: $0 [1|2|3|4|all]"
        exit 1
        ;;
esac

log ""
log "=== TEST SUITE COMPLETE ==="
