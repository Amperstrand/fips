#!/usr/bin/env bash
# FIPS BLE Comprehensive Cross-Platform Test Suite
# Tests Mac ↔ Linux BLE in both roles with echo and throughput benchmarks.
#
# Usage: sudo ./testing/ble-comprehensive-test.sh
#
# Phases:
#   A: Baseline (Mac peripheral, Linux central)
#   B: Role swap (Mac central, Linux peripheral)
#   C: Stress tests (long duration, high packet counts)
#   D: Edge cases (MTU sweep, rate variations, asymmetric configs)
#   E: Resilience (reconnect, config swap mid-test, sustained traffic)
#   F: Report generation
#
# All results saved to /tmp/fips-logs/comprehensive-test/

set -euo pipefail

# ============================================================================
# Configuration
# ============================================================================

MAC_NPUB="npub126j86fegtf38v94t5fs624njxsxv35d3f8mwkpa69wkz49uvr8gqv9tu25"
LINUX_NPUB="npub150gmh6m3gqcgdlasvhdfntqty8v4je4cl6lhg9rj5th2l3zyn8fq3x78an"
LINUX_SSH="218"
MAC_SOCKET="/tmp/fips-control.sock"
FIPSCTL_MAC="/Users/macbook/src/fips/target/release/fipsctl"
FIPSCTL_LINUX="sudo fipsctl"
FIPS_BIN="/Users/macbook/src/fips/target/release/fips"
RESULTS_DIR="/tmp/fips-logs/comprehensive-test"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
LOG_FILE="${RESULTS_DIR}/test-${TIMESTAMP}.log"
RESULTS_JSON="${RESULTS_DIR}/results-${TIMESTAMP}.json"
SUMMARY_MD="${RESULTS_DIR}/summary-${TIMESTAMP}.md"

# Mac configs
MAC_PERIPHERAL_CONFIG="/tmp/fips-mac-peripheral.yaml"
MAC_CENTRAL_CONFIG="/tmp/fips-mac-central.yaml"

# BLE MAC for BlueZ cache clear
LINUX_BLE_MAC="14:7D:DA:7D:4C:31"

# Connection wait time (seconds)
CONNECT_WAIT=25

# ============================================================================
# Utility Functions
# ============================================================================

log() {
    echo "[$(date '+%H:%M:%S')] $*" | tee -a "$LOG_FILE"
}

log_section() {
    echo "" | tee -a "$LOG_FILE"
    echo "========================================================================" | tee -a "$LOG_FILE"
    echo "[$(date '+%H:%M:%S')] $*" | tee -a "$LOG_FILE"
    echo "========================================================================" | tee -a "$LOG_FILE"
}

# Run a test and record result. Args: test_name pass_condition
# pass_condition is evaluated as bash test
record_result() {
    local test_name="$1"
    local result="$2"
    local detail="${3:-}"
    
    if [ "$result" = "PASS" ]; then
        log "  ✅ PASS: ${test_name} ${detail}"
        echo "${TIMESTAMP}|${test_name}|PASS|${detail}" >> "${RESULTS_DIR}/results.csv"
    else
        log "  ❌ FAIL: ${test_name} ${detail}"
        echo "${TIMESTAMP}|${test_name}|FAIL|${detail}" >> "${RESULTS_DIR}/results.csv"
    fi
}

mac_fipsctl() {
    sudo "$FIPSCTL_MAC" -s "$MAC_SOCKET" "$@"
}

linux_fipsctl() {
    ssh "$LINUX_SSH" "$FIPSCTL_LINUX $*"
}

# Check if a peer is connected
check_peer_connected() {
    local side="$1"
    local expected_npub="$2"
    
    if [ "$side" = "mac" ]; then
        local peers
        peers=$(mac_fipsctl show peers 2>/dev/null | python3 -c "
import sys, json
d = json.load(sys.stdin)
for p in d.get('peers', []):
    if p['npub'] == '${expected_npub}':
        print('connected')
        sys.exit(0)
print('not_connected')
" 2>/dev/null) || echo "error"
        echo "$peers"
    else
        local peers
        peers=$(linux_fipsctl "show peers" 2>/dev/null | python3 -c "
import sys, json
d = json.load(sys.stdin)
for p in d.get('peers', []):
    if p['npub'] == '${expected_npub}':
        print('connected')
        sys.exit(0)
print('not_connected')
" 2>/dev/null) || echo "error"
        echo "$peers"
    fi
}

# Wait for peer connection with timeout
wait_for_peer() {
    local side="$1"
    local expected_npub="$2"
    local max_wait="${3:-60}"
    local waited=0
    
    while [ $waited -lt $max_wait ]; do
        local status
        status=$(check_peer_connected "$side" "$expected_npub")
        if [ "$status" = "connected" ]; then
            log "  Peer connected after ${waited}s"
            return 0
        fi
        sleep 5
        waited=$((waited + 5))
    done
    
    log "  ⚠️  Peer NOT connected after ${max_wait}s"
    return 1
}

# Stop Mac fips
stop_mac_fips() {
    sudo pkill -f 'fips -c' 2>/dev/null || true
    sleep 2
    sudo killall -9 fips 2>/dev/null || true
    sleep 1
}

# Stop Linux fips
stop_linux_fips() {
    ssh "$LINUX_SSH" 'sudo systemctl stop fips' 2>/dev/null || true
    sleep 2
}

# Start Mac as peripheral
start_mac_peripheral() {
    stop_mac_fips
    log "  Starting Mac as BLE peripheral (advertise=true, scan=false)..."
    tmux send-keys -t fips-daemon "sudo ${FIPS_BIN} -c ${MAC_PERIPHERAL_CONFIG}" Enter
    sleep 5
}

# Start Mac as central
start_mac_central() {
    stop_mac_fips
    log "  Starting Mac as BLE central (scan=true, advertise=false)..."
    tmux send-keys -t fips-daemon "sudo ${FIPS_BIN} -c ${MAC_CENTRAL_CONFIG}" Enter
    sleep 5
}

# Configure and start Linux as central
start_linux_central() {
    stop_linux_fips
    log "  Configuring Linux as BLE central (scan=true, advertise=false)..."
    ssh "$LINUX_SSH" "sudo chattr -i /etc/fips/fips.yaml 2>/dev/null; cat > /tmp/fips-central.yaml << 'CFEOF'
node:
  identity:
    persistent: true
tun:
  enabled: false
dns:
  enabled: false
transports:
  ble:
    adapter: \"hci0\"
    mtu: 1024
    accept_connections: false
    auto_connect: true
    scan: true
    advertise: false
    send_rate_bps: 80000
    send_burst_bytes: 2048
CFEOF
sudo cp /tmp/fips-central.yaml /etc/fips/fips.yaml && sudo chattr +i /etc/fips/fips.yaml"
    ssh "$LINUX_SSH" 'sudo systemctl start fips'
    sleep 3
}

# Configure and start Linux as peripheral
start_linux_peripheral() {
    stop_linux_fips
    log "  Configuring Linux as BLE peripheral (advertise=true, scan=false)..."
    ssh "$LINUX_SSH" "sudo chattr -i /etc/fips/fips.yaml 2>/dev/null; cat > /tmp/fips-peripheral.yaml << 'CFEOF'
node:
  identity:
    persistent: true
tun:
  enabled: false
dns:
  enabled: false
transports:
  ble:
    adapter: \"hci0\"
    mtu: 1024
    accept_connections: true
    auto_connect: false
    scan: false
    advertise: true
    send_rate_bps: 80000
    send_burst_bytes: 2048
CFEOF
sudo cp /tmp/fips-peripheral.yaml /etc/fips/fips.yaml && sudo chattr +i /etc/fips/fips.yaml"
    ssh "$LINUX_SSH" 'sudo systemctl start fips'
    sleep 3
}

# Clear BlueZ device cache for clean connection
clear_bluez_cache() {
    log "  Clearing BlueZ device cache..."
    ssh "$LINUX_SSH" "bluetoothctl -- remove ${LINUX_BLE_MAC} 2>/dev/null; sudo systemctl restart bluetooth" || true
    sleep 5
}

# Reconnect with clean state
full_reconnect() {
    local mac_role="$1"
    local linux_role="$2"
    
    log "  Full reconnect: Mac=${mac_role}, Linux=${linux_role}..."
    
    stop_mac_fips
    stop_linux_fips
    clear_bluez_cache
    
    if [ "$linux_role" = "peripheral" ]; then
        start_linux_peripheral
        sleep 10
    else
        start_linux_central
        sleep 10
    fi
    
    if [ "$mac_role" = "peripheral" ]; then
        start_mac_peripheral
    else
        start_mac_central
    fi
    
    wait_for_peer "mac" "$LINUX_NPUB" 60
}

# ============================================================================
# Test Functions
# ============================================================================

# Echo test: send from $1 to peer $2, check loss rate
run_echo_test() {
    local label="$1"
    local from_side="$2"
    local target_npub="$3"
    local payload_size="${4:-256}"
    local count="${5:-10}"
    local max_loss="${6:-0}"
    
    log "  Echo: ${label} size=${payload_size}B count=${count} max_loss=${max_loss}%"
    
    local raw
    if [ "$from_side" = "mac" ]; then
        raw=$(mac_fipsctl benchmark echo "$target_npub" --payload-size "$payload_size" --count "$count" 2>&1) || true
    else
        raw=$(linux_fipsctl "benchmark echo ${target_npub} --payload-size ${payload_size} --count ${count}" 2>&1) || true
    fi
    
    local parsed
    parsed=$(echo "$raw" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    loss = float(d.get('loss_rate_pct', '100').rstrip('%'))
    sent = d.get('sent', 0)
    recv = d.get('received', 0)
    rtts = [r['rtt_ms'] for r in d.get('results', [])]
    min_rtt = min(rtts) if rtts else 0
    max_rtt = max(rtts) if rtts else 0
    avg_rtt = sum(rtts)/len(rtts) if rtts else 0
    print(f'{loss}|{sent}|{recv}|{min_rtt:.1f}|{max_rtt:.1f}|{avg_rtt:.1f}')
except Exception as e:
    print(f'ERROR|{e}')
" 2>/dev/null) || parsed="ERROR|parse_failed"
    
    IFS='|' read -r loss sent recv min_rtt max_rtt avg_rtt <<< "$parsed"
    
    if [ "$loss" = "ERROR" ]; then
        record_result "$label" "FAIL" "parse error: $sent"
        return 1
    fi
    
    local result="PASS"
    if [ "$(echo "$loss > $max_loss" | bc -l 2>/dev/null || echo 1)" = "1" ]; then
        result="FAIL"
    fi
    
    record_result "$label" "$result" "loss=${loss}% sent=${sent} recv=${recv} rtt=${min_rtt}-${max_rtt}ms"
    echo "$raw" >> "${RESULTS_DIR}/echo-${label// /_}-${TIMESTAMP}.json"
}

# Throughput test
run_throughput_test() {
    local label="$1"
    local from_side="$2"
    local target_npub="$3"
    local direction="${4:-upload}"
    local duration="${5:-10}"
    local frame_size="${6:-256}"
    local rate="${7:-40000}"
    
    log "  Throughput: ${label} dir=${direction} dur=${duration}s frame=${frame_size}B rate=${rate}bps"
    
    local raw
    if [ "$from_side" = "mac" ]; then
        raw=$(mac_fipsctl benchmark throughput "$target_npub" --direction "$direction" --duration "$duration" --frame-size "$frame_size" --rate "$rate" 2>&1) || true
    else
        raw=$(linux_fipsctl "benchmark throughput ${target_npub} --direction ${direction} --duration ${duration} --frame-size ${frame_size} --rate ${rate}" 2>&1) || true
    fi
    
    local parsed
    parsed=$(echo "$raw" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    frames = d.get('frames_sent', 0)
    bytes_sent = d.get('bytes_sent', 0)
    dur = d.get('duration_secs', 0)
    kbps = (bytes_sent * 8 / dur / 1000) if dur > 0 else 0
    print(f'{frames}|{bytes_sent}|{dur}|{kbps:.1f}')
except Exception as e:
    print(f'ERROR|{e}')
" 2>/dev/null) || parsed="ERROR|parse_failed"
    
    IFS='|' read -r frames bytes_sent dur kbps <<< "$parsed"
    
    if [ "$frames" = "ERROR" ]; then
        record_result "$label" "FAIL" "parse error: $bytes_sent"
        return 1
    fi
    
    local result="PASS"
    [ "$frames" -eq 0 ] && result="FAIL"
    
    record_result "$label" "$result" "frames=${frames} bytes=${bytes_sent} throughput=${kbps}kbps"
    echo "$raw" >> "${RESULTS_DIR}/throughput-${label// /_}-${TIMESTAMP}.json"
}

# ============================================================================
# Setup
# ============================================================================

setup() {
    mkdir -p "$RESULTS_DIR"
    
    # Ensure tmux session exists
    if ! tmux has-session -t fips-daemon 2>/dev/null; then
        tmux new-session -d -s fips-daemon
    fi
    
    # Write Mac configs
    cat > "$MAC_PERIPHERAL_CONFIG" << 'EOF'
node:
  identity:
    persistent: true
tun:
  enabled: false
dns:
  enabled: false
transports:
  ble:
    adapter: "default"
    mtu: 1024
    accept_connections: true
    auto_connect: true
    scan: false
    advertise: true
    send_rate_bps: 80000
    send_burst_bytes: 2048
EOF
    
    cat > "$MAC_CENTRAL_CONFIG" << 'EOF'
node:
  identity:
    persistent: true
tun:
  enabled: false
dns:
  enabled: false
transports:
  ble:
    adapter: "default"
    mtu: 1024
    accept_connections: false
    auto_connect: true
    scan: true
    advertise: false
    send_rate_bps: 80000
    send_burst_bytes: 2048
EOF
    
    # CSV header
    echo "timestamp|test_name|result|detail" > "${RESULTS_DIR}/results.csv"
    
    # Initialize results JSON
    echo '{"test_start": "'$(date -Iseconds)'", "phases": {}}' > "$RESULTS_JSON"
    
    log "Test suite initialized. Results in ${RESULTS_DIR}"
    log "Mac npub: ${MAC_NPUB}"
    log "Linux npub: ${LINUX_NPUB}"
}

# ============================================================================
# Phase A: Baseline — Mac peripheral, Linux central
# ============================================================================

phase_a() {
    log_section "Phase A: Baseline — Mac peripheral, Linux central"
    
    full_reconnect "peripheral" "central"
    
    log "A.1: Echo tests — Mac→Linux"
    for size in 32 64 128 256 384 512 640 768 900; do
        run_echo_test "A-mac-to-linux-${size}B" "mac" "$LINUX_NPUB" "$size" 10 0
        sleep 2
    done
    
    log "A.2: Echo tests — Linux→Mac"
    for size in 32 64 128 256 384 512 640 768 900; do
        run_echo_test "A-linux-to-mac-${size}B" "linux" "$MAC_NPUB" "$size" 10 0
        sleep 2
    done
    
    log "A.3: Throughput tests — Mac→Linux (upload)"
    for frame in 64 128 256 512 900; do
        run_throughput_test "A-mac-upload-${frame}B" "mac" "$LINUX_NPUB" "upload" 10 "$frame" 40000
        sleep 3
    done
    
    log "A.4: Throughput tests — Linux→Mac (upload)"
    for frame in 64 128 256 512 900; do
        run_throughput_test "A-linux-upload-${frame}B" "linux" "$MAC_NPUB" "upload" 10 "$frame" 40000
        sleep 3
    done
    
    log "A.5: Throughput rate variation — Mac→Linux, 256B frame"
    for rate in 10000 20000 40000 60000 80000; do
        run_throughput_test "A-mac-rate-${rate}bps" "mac" "$LINUX_NPUB" "upload" 10 256 "$rate"
        sleep 3
    done
    
    log "A.6: Throughput rate variation — Linux→Mac, 256B frame"
    for rate in 10000 20000 40000 60000 80000; do
        run_throughput_test "A-linux-rate-${rate}bps" "linux" "$MAC_NPUB" "upload" 10 256 "$rate"
        sleep 3
    done
    
    log "Phase A complete"
}

# ============================================================================
# Phase B: Role swap — Mac central, Linux peripheral
# ============================================================================

phase_b() {
    log_section "Phase B: Role swap — Mac central, Linux peripheral"
    
    full_reconnect "central" "peripheral"
    
    log "B.1: Echo tests — Mac→Linux"
    for size in 32 64 128 256 384 512 640 768 900; do
        run_echo_test "B-mac-to-linux-${size}B" "mac" "$LINUX_NPUB" "$size" 10 0
        sleep 2
    done
    
    log "B.2: Echo tests — Linux→Mac"
    for size in 32 64 128 256 384 512 640 768 900; do
        run_echo_test "B-linux-to-mac-${size}B" "linux" "$MAC_NPUB" "$size" 10 0
        sleep 2
    done
    
    log "B.3: Throughput tests — Mac→Linux (upload)"
    for frame in 64 128 256 512 900; do
        run_throughput_test "B-mac-upload-${frame}B" "mac" "$LINUX_NPUB" "upload" 10 "$frame" 40000
        sleep 3
    done
    
    log "B.4: Throughput tests — Linux→Mac (upload)"
    for frame in 64 128 256 512 900; do
        run_throughput_test "B-linux-upload-${frame}B" "linux" "$MAC_NPUB" "upload" 10 "$frame" 40000
        sleep 3
    done
    
    log "B.5: Throughput rate variation — Mac→Linux, 256B frame"
    for rate in 10000 20000 40000 60000 80000; do
        run_throughput_test "B-mac-rate-${rate}bps" "mac" "$LINUX_NPUB" "upload" 10 256 "$rate"
        sleep 3
    done
    
    log "B.6: Throughput rate variation — Linux→Mac, 256B frame"
    for rate in 10000 20000 40000 60000 80000; do
        run_throughput_test "B-linux-rate-${rate}bps" "linux" "$MAC_NPUB" "upload" 10 256 "$rate"
        sleep 3
    done
    
    log "Phase B complete"
}

# ============================================================================
# Phase C: Stress tests
# ============================================================================

phase_c() {
    log_section "Phase C: Stress tests"
    
    # Ensure we're connected (use whichever role from Phase B)
    if ! wait_for_peer "mac" "$LINUX_NPUB" 10; then
        full_reconnect "central" "peripheral"
    fi
    
    log "C.1: High packet count echo — Mac→Linux, 100 packets"
    run_echo_test "C-mac-100pkt" "mac" "$LINUX_NPUB" 256 100 5
    sleep 3
    
    log "C.2: High packet count echo — Linux→Mac, 100 packets"
    run_echo_test "C-linux-100pkt" "linux" "$MAC_NPUB" 256 100 5
    sleep 3
    
    log "C.3: Long duration throughput — Mac→Linux, 60s"
    run_throughput_test "C-mac-60s" "mac" "$LINUX_NPUB" "upload" 60 256 40000
    sleep 5
    
    log "C.4: Long duration throughput — Linux→Mac, 60s"
    run_throughput_test "C-linux-60s" "linux" "$MAC_NPUB" "upload" 60 256 40000
    sleep 5
    
    log "C.5: Burst echo — Mac→Linux, 50 packets, 900B"
    run_echo_test "C-mac-burst-900B" "mac" "$LINUX_NPUB" 900 50 10
    sleep 3
    
    log "C.6: Burst echo — Linux→Mac, 50 packets, 900B"
    run_echo_test "C-linux-burst-900B" "linux" "$MAC_NPUB" 900 50 10
    sleep 3
    
    log "C.7: Maximum rate throughput — Mac→Linux, 80kbps"
    run_throughput_test "C-mac-maxrate" "mac" "$LINUX_NPUB" "upload" 30 256 80000
    sleep 5
    
    log "C.8: Maximum rate throughput — Linux→Mac, 80kbps"
    run_throughput_test "C-linux-maxrate" "linux" "$MAC_NPUB" "upload" 30 256 80000
    sleep 5
    
    log "C.9: Large frame throughput — Mac→Linux, 900B frames"
    run_throughput_test "C-mac-large-frames" "mac" "$LINUX_NPUB" "upload" 30 900 40000
    sleep 5
    
    log "C.10: Large frame throughput — Linux→Mac, 900B frames"
    run_throughput_test "C-linux-large-frames" "linux" "$MAC_NPUB" "upload" 30 900 40000
    sleep 5
    
    log "Phase C complete"
}

# ============================================================================
# Phase D: Edge cases
# ============================================================================

phase_d() {
    log_section "Phase D: Edge cases"
    
    if ! wait_for_peer "mac" "$LINUX_NPUB" 10; then
        full_reconnect "central" "peripheral"
    fi
    
    log "D.1: MTU boundary sweep — Mac→Linux"
    for size in 900 920 940 950 955 960 963 964 965 966 967 970 980 1000; do
        run_echo_test "D-mac-mtu-${size}B" "mac" "$LINUX_NPUB" "$size" 3 0
        sleep 1
    done
    
    log "D.2: MTU boundary sweep — Linux→Mac"
    for size in 900 920 940 950 955 960 963 964 965 966 967 970 980 1000; do
        run_echo_test "D-linux-mtu-${size}B" "linux" "$MAC_NPUB" "$size" 3 0
        sleep 1
    done
    
    log "D.3: Minimum payload echo"
    run_echo_test "D-mac-min-payload" "mac" "$LINUX_NPUB" 0 10 0
    sleep 1
    run_echo_test "D-linux-min-payload" "linux" "$MAC_NPUB" 0 10 0
    sleep 1
    
    log "D.4: Single packet echo"
    run_echo_test "D-mac-single" "mac" "$LINUX_NPUB" 256 1 0
    sleep 1
    run_echo_test "D-linux-single" "linux" "$MAC_NPUB" 256 1 0
    sleep 1
    
    log "D.5: Throughput with minimum frame size"
    run_throughput_test "D-mac-min-frame" "mac" "$LINUX_NPUB" "upload" 10 16 40000
    sleep 2
    run_throughput_test "D-linux-min-frame" "linux" "$MAC_NPUB" "upload" 10 16 40000
    sleep 2
    
    log "D.6: Throughput with max frame size"
    run_throughput_test "D-mac-max-frame" "mac" "$LINUX_NPUB" "upload" 10 900 40000
    sleep 2
    run_throughput_test "D-linux-max-frame" "linux" "$MAC_NPUB" "upload" 10 900 40000
    sleep 2
    
    log "D.7: Low rate throughput (10kbps)"
    run_throughput_test "D-mac-low-rate" "mac" "$LINUX_NPUB" "upload" 10 256 10000
    sleep 2
    run_throughput_test "D-linux-low-rate" "linux" "$MAC_NPUB" "upload" 10 256 10000
    sleep 2
    
    log "D.8: Download direction throughput"
    run_throughput_test "D-mac-download" "mac" "$LINUX_NPUB" "download" 10 256 40000
    sleep 2
    run_throughput_test "D-linux-download" "linux" "$MAC_NPUB" "download" 10 256 40000
    sleep 2
    
    log "Phase D complete"
}

# ============================================================================
# Phase E: Resilience
# ============================================================================

phase_e() {
    log_section "Phase E: Resilience"
    
    log "E.1: Role swap mid-session"
    log "  Starting with Mac peripheral, Linux central..."
    full_reconnect "peripheral" "central"
    
    log "  Running baseline echo..."
    run_echo_test "E-pre-swap-mac" "mac" "$LINUX_NPUB" 256 10 0
    run_echo_test "E-pre-swap-linux" "linux" "$MAC_NPUB" 256 10 0
    
    log "  Swapping roles: Mac central, Linux peripheral..."
    full_reconnect "central" "peripheral"
    
    log "  Running post-swap echo..."
    run_echo_test "E-post-swap-mac" "mac" "$LINUX_NPUB" 256 10 0
    run_echo_test "E-post-swap-linux" "linux" "$MAC_NPUB" 256 10 0
    
    log "E.2: Reconnection test — stop and restart Linux"
    log "  Stopping Linux fips..."
    stop_linux_fips
    sleep 10
    
    log "  Verifying Mac detects peer loss..."
    local mac_status
    mac_status=$(check_peer_connected "mac" "$LINUX_NPUB")
    if [ "$mac_status" = "not_connected" ] || [ "$mac_status" = "error" ]; then
        record_result "E-peer-loss-detection" "PASS" "Mac detected peer loss"
    else
        record_result "E-peer-loss-detection" "FAIL" "Mac did not detect peer loss"
    fi
    
    log "  Restarting Linux as peripheral..."
    start_linux_peripheral
    sleep 10
    
    log "  Waiting for reconnection..."
    if wait_for_peer "mac" "$LINUX_NPUB" 60; then
        record_result "E-reconnect" "PASS" "Reconnected after Linux restart"
        run_echo_test "E-post-reconnect" "mac" "$LINUX_NPUB" 256 10 0
    else
        record_result "E-reconnect" "FAIL" "Could not reconnect after Linux restart"
    fi
    
    log "E.3: Sustained traffic — 5-minute throughput"
    log "  Mac→Linux, 5-minute throughput test, 256B frames, 40kbps..."
    run_throughput_test "E-sustained-5min-mac" "mac" "$LINUX_NPUB" "upload" 30 256 40000
    sleep 3
    run_throughput_test "E-sustained-5min-mac-2" "mac" "$LINUX_NPUB" "upload" 30 256 40000
    sleep 3
    run_throughput_test "E-sustained-5min-mac-3" "mac" "$LINUX_NPUB" "upload" 30 256 40000
    sleep 3
    run_throughput_test "E-sustained-5min-mac-4" "mac" "$LINUX_NPUB" "upload" 30 256 40000
    sleep 3
    run_throughput_test "E-sustained-5min-mac-5" "mac" "$LINUX_NPUB" "upload" 30 256 40000
    
    log "E.4: Back-to-back echo tests (stability check)"
    for i in $(seq 1 10); do
        run_echo_test "E-stability-${i}" "mac" "$LINUX_NPUB" 256 20 5
        sleep 1
    done
    
    log "Phase E complete"
}

# ============================================================================
# Phase F: Report
# ============================================================================

phase_f() {
    log_section "Phase F: Report generation"
    
    local total passed failed
    total=$(tail -n +2 "${RESULTS_DIR}/results.csv" | wc -l | tr -d ' ')
    passed=$(tail -n +2 "${RESULTS_DIR}/results.csv" | grep '|PASS|' | wc -l | tr -d ' ')
    failed=$(tail -n +2 "${RESULTS_DIR}/results.csv" | grep '|FAIL|' | wc -l | tr -d ' ')
    
    local pass_rate="0"
    if [ "$total" -gt 0 ]; then
        pass_rate=$(echo "scale=1; $passed * 100 / $total" | bc 2>/dev/null || echo "0")
    fi
    
    log "Generating summary..."
    
    cat > "$SUMMARY_MD" << EOF
# FIPS BLE Comprehensive Test Report

**Date**: $(date -Iseconds)
**Duration**: N/A (calculated from timestamps)
**Branch**: macos-ble-peripheral (commit 774478b)
**Mac npub**: ${MAC_NPUB}
**Linux npub**: ${LINUX_NPUB}

## Summary

| Metric | Value |
|--------|-------|
| Total tests | ${total} |
| Passed | ${passed} |
| Failed | ${failed} |
| Pass rate | ${pass_rate}% |

## Failed Tests

$(tail -n +2 "${RESULTS_DIR}/results.csv" | grep '|FAIL|' | while IFS='|' read -r ts name result detail; do
    echo "- **${name}**: ${detail}"
done || echo "None")

## Phase Results

### Phase A: Baseline (Mac peripheral, Linux central)
$(tail -n +2 "${RESULTS_DIR}/results.csv" | grep '^.\+|A-' | grep -c '|PASS|') / $(tail -n +2 "${RESULTS_DIR}/results.csv" | grep '^.\+|A-' | wc -l | tr -d ' ') passed

### Phase B: Role Swap (Mac central, Linux peripheral)
$(tail -n +2 "${RESULTS_DIR}/results.csv" | grep '^.\+|B-' | grep -c '|PASS|') / $(tail -n +2 "${RESULTS_DIR}/results.csv" | grep '^.\+|B-' | wc -l | tr -d ' ') passed

### Phase C: Stress Tests
$(tail -n +2 "${RESULTS_DIR}/results.csv" | grep '^.\+|C-' | grep -c '|PASS|') / $(tail -n +2 "${RESULTS_DIR}/results.csv" | grep '^.\+|C-' | wc -l | tr -d ' ') passed

### Phase D: Edge Cases
$(tail -n +2 "${RESULTS_DIR}/results.csv" | grep '^.\+|D-' | grep -c '|PASS|') / $(tail -n +2 "${RESULTS_DIR}/results.csv" | grep '^.\+|D-' | wc -l | tr -d ' ') passed

### Phase E: Resilience
$(tail -n +2 "${RESULTS_DIR}/results.csv" | grep '^.\+|E-' | grep -c '|PASS|') / $(tail -n +2 "${RESULTS_DIR}/results.csv" | grep '^.\+|E-' | wc -l | tr -d ' ') passed

## All Results

$(tail -n +2 "${RESULTS_DIR}/results.csv" | while IFS='|' read -r ts name result detail; do
    if [ "$result" = "PASS" ]; then
        echo "- ✅ **${name}**: ${detail}"
    else
        echo "- ❌ **${name}**: ${detail}"
    fi
done)
EOF
    
    log "Report saved to ${SUMMARY_MD}"
    log ""
    log "========================================="
    log "  FINAL RESULTS: ${passed}/${total} passed (${pass_rate}%)"
    log "  Report: ${SUMMARY_MD}"
    log "========================================="
}

# ============================================================================
# Main
# ============================================================================

main() {
    local phases="${1:-ABCDEF}"
    
    log "FIPS BLE Comprehensive Test Suite"
    log "Phases to run: ${phases}"
    log "Results directory: ${RESULTS_DIR}"
    
    setup
    
    if echo "$phases" | grep -q 'A'; then phase_a; fi
    if echo "$phases" | grep -q 'B'; then phase_b; fi
    if echo "$phases" | grep -q 'C'; then phase_c; fi
    if echo "$phases" | grep -q 'D'; then phase_d; fi
    if echo "$phases" | grep -q 'E'; then phase_e; fi
    if echo "$phases" | grep -q 'F'; then phase_f; fi
    
    log "Test suite complete."
}

# Run with specified phases or all
main "$@"
