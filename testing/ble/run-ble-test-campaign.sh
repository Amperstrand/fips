#!/usr/bin/env bash
# FIPS BLE Test Campaign: Linux ↔ macOS bidirectional proof
#
# Prerequisites:
#   - Linux node at 192.168.13.218 with SSH access
#   - Mac node (this machine) with BLE
#   - Both nodes built from same commit on linux-ble-stability-v2
#   - iperf3 installed on both nodes
#   - btmon available on Linux (bluez-tools package)
#
# Usage:
#   ./run-ble-test-campaign.sh [--test A|B|C|D|E] [--skip-build] [--skip-capture]
#
# Output:
#   /tmp/fips-ble-campaign/   — all logs, captures, and results
#
set -euo pipefail

SSH="ssh -i ~/.ssh/id_ed25519_gitlab ubuntu@192.168.13.218"
LINUX_SCRIPT="/tmp/fips-ble-linux-side.sh"
CAMPAIGN_DIR="/tmp/fips-ble-campaign"
REPO_DIR="$(git rev-parse --show-toplevel)"
LOCAL_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
LOCAL_COMMIT="$(git rev-parse --short=10 HEAD)"
LOCAL_STATUS="$(git status --short --branch)"
REMOTE_REPO_DIR="${REMOTE_REPO_DIR:-/home/ubuntu/src/fips}"
MAC_HELPER="/usr/local/libexec/fips-test/macos-test-helper.sh"

TEST="${1:-all}"
if [[ "$TEST" == "--test" ]]; then
    TEST="${2:-all}"
fi

SKIP_BUILD=false
SKIP_CAPTURE=false
for arg in "$@"; do
    case "$arg" in
        --skip-build) SKIP_BUILD=true ;;
        --skip-capture) SKIP_CAPTURE=true ;;
    esac
done

mkdir -p "$CAMPAIGN_DIR"

echo "=== FIPS BLE Test Campaign ==="
echo "Local branch: $LOCAL_BRANCH"
echo "Local commit: $LOCAL_COMMIT"
echo "Campaign dir: $CAMPAIGN_DIR"
echo "Test: $TEST"
echo ""

mac_helper() {
    sudo "$MAC_HELPER" "$@"
}

mac_fipsctl() {
    mac_helper run-fipsctl "$@"
}

mac_start_iperf_server() {
    mac_helper start-iperf-server
}

mac_stop_iperf_server() {
    mac_helper stop-iperf-server
}

extract_first_peer_ipv6() {
    python3 -c 'import json,sys; data=json.load(sys.stdin); peers=data.get("peers", []); print(peers[0].get("ipv6_addr", "") if peers else "")'
}

wait_for_peer_ipv6() {
    local side="$1"
    local timeout_secs="${2:-30}"
    local raw=""
    local ipv6=""
    echo "  Waiting for peer IPv6 on $side (timeout ${timeout_secs}s)..." >&2
    for i in $(seq 1 "$timeout_secs"); do
        if [[ "$side" == "mac" ]]; then
            raw="$(mac_fipsctl show peers 2>/dev/null || true)"
        else
            raw="$($SSH "sudo fipsctl show peers" 2>/dev/null || true)"
        fi
        if [[ -n "$raw" ]]; then
            ipv6="$(printf '%s' "$raw" | extract_first_peer_ipv6 2>/dev/null || true)"
            if [[ -n "$ipv6" ]]; then
                echo "$ipv6"
                return 0
            fi
        fi
        sleep 1
    done
    return 1
}

count_ble_links_json() {
    python3 -c 'import json,sys; data=json.load(sys.stdin); print(sum(1 for link in data.get("links", []) if link.get("transport_id") is not None))'
}

remote_git() {
    $SSH "git -C $REMOTE_REPO_DIR $*"
}

sync_remote_repo() {
    echo "=== Syncing Linux campaign checkout ==="
    rsync -az --delete \
        --exclude '.git/' \
        --exclude 'target/' \
        --exclude '.sisyphus/' \
        --exclude '.playwright-mcp/' \
        --exclude '.claude/' \
        --exclude '.DS_Store' \
        -e "ssh -i ~/.ssh/id_ed25519_gitlab" \
        "$REPO_DIR/" "ubuntu@192.168.13.218:$REMOTE_REPO_DIR/"
}

capture_remote_provenance() {
    local out_file="$1"
    {
        echo "local_branch=$LOCAL_BRANCH"
        echo "local_commit=$LOCAL_COMMIT"
        echo "local_version=$( /tmp/fips-target/release/fips -V 2>/dev/null || true )"
        echo "local_git_status<<'EOF'"
        printf '%s\n' "$LOCAL_STATUS"
        echo "EOF"
        echo "remote_branch=$(remote_git 'rev-parse --abbrev-ref HEAD' | tr -d '\r')"
        echo "remote_commit=$(remote_git 'rev-parse --short=10 HEAD' | tr -d '\r')"
        echo "remote_version=$($SSH '/usr/local/bin/fips -V 2>/dev/null || true' | tr -d '\r')"
        echo "remote_git_status<<'EOF'"
        $SSH "git -C $REMOTE_REPO_DIR status --short --branch" | tr -d '\r'
        echo "EOF"
        echo "timestamp_utc=$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    } > "$out_file"
}

record_test_metadata() {
    local test_id="$1"
    capture_remote_provenance "$CAMPAIGN_DIR/test${test_id}-provenance.txt"
}

record() {
    local test_id="$1"
    local label="$2"
    local file="$CAMPAIGN_DIR/test${test_id}-${label}"
    printf '%s\n' "$file"
}

stop_linux_fips() {
    $SSH "sudo systemctl stop fips 2>/dev/null || true; sudo pkill -f 'fips -c' 2>/dev/null || true" || true
}

stop_mac_fips() {
    mac_helper stop-fips 2>/dev/null || true
}

# --- Build ---

if [[ "$SKIP_BUILD" == "false" ]]; then
    echo "=== Building Mac binary ==="
    CARGO_TARGET_DIR=/tmp/fips-target cargo build --release --features ble-macos 2>&1 | tee "$CAMPAIGN_DIR/mac-build.log"

    sync_remote_repo

    echo "=== Building Linux binary ==="
    $SSH "source ~/.cargo/env && cd $REMOTE_REPO_DIR && CARGO_TARGET_DIR=/tmp/fips-target cargo build --release --features ble" 2>&1 | tee "$CAMPAIGN_DIR/linux-build.log"

    echo "=== Deploying Linux binary ==="
    stop_linux_fips
    sleep 2
    $SSH "sudo cp /tmp/fips-target/release/fips /usr/local/bin/fips && sudo cp /tmp/fips-target/release/fipsctl /usr/local/bin/fipsctl"
fi

# --- Helper: wait for BLE peer ---

wait_for_peer() {
    local side="$1"
    local timeout_secs="${2:-60}"
    local cmd
    if [[ "$side" == "mac" ]]; then
        cmd="mac_fipsctl show peers"
    else
        cmd="sudo fipsctl show peers"
    fi
    echo "  Waiting for BLE peer ($side, timeout ${timeout_secs}s)..."
    for i in $(seq 1 "$timeout_secs"); do
        if $cmd 2>/dev/null | grep -q "transport_type.*ble\|connected"; then
            echo "  ✓ Peer connected after ${i}s"
            return 0
        fi
        sleep 1
    done
    echo "  ✗ Timed out waiting for peer"
    return 1
}

start_linux_with_config() {
    local config_name="$1"
    local config_path="testing/ble/configs/${config_name}"
    local remote_config="/tmp/fips-test-config.yaml"

    echo "  Deploying config $config_name to Linux..."
    scp -i ~/.ssh/id_ed25519_gitlab "$REPO_DIR/$config_path" "ubuntu@192.168.13.218:$remote_config"

    echo "  Starting Linux FIPS..."
    stop_linux_fips
    sleep 2
    $SSH "sudo chattr -i /etc/fips/fips.yaml 2>/dev/null || true; sudo cp $remote_config /etc/fips/fips.yaml; sudo chattr +i /etc/fips/fips.yaml; sudo systemctl restart fips"
}

start_linux_capture() {
    local test_id="$1"
    if [[ "$SKIP_CAPTURE" == "true" ]]; then return; fi
    echo "  Starting btmon capture on Linux..."
    $SSH "sudo btmon -i hci0 -w /tmp/fips-test${test_id}-ble.pklg &>/dev/null & echo \$!" | head -1
}

stop_linux_capture() {
    if [[ "$SKIP_CAPTURE" == "true" ]]; then return; fi
    $SSH "sudo pkill -f 'btmon.*pklg' 2>/dev/null || true"
    sleep 1
}

collect_linux_artifacts() {
    local test_id="$1"
    echo "  Collecting Linux artifacts..."
    scp -i ~/.ssh/id_ed25519_gitlab "ubuntu@192.168.13.218:/tmp/fips-test${test_id}-ble.pklg" "$CAMPAIGN_DIR/" 2>/dev/null || true
    scp -i ~/.ssh/id_ed25519_gitlab "ubuntu@192.168.13.218:/tmp/fips-ik-ephemeral-test${test_id}.jsonl" "$CAMPAIGN_DIR/" 2>/dev/null || true
    $SSH "sudo journalctl -u fips --no-pager --since '15 min ago'" > "$CAMPAIGN_DIR/test${test_id}-linux-journal.log" 2>/dev/null || true
}

run_directional_iperf_phase() {
    local test_id="$1"
    local phase_label="$2"
    local client_side="$3"
    local linux_config="$4"
    local mac_config="$5"

    echo "  --- Phase: $phase_label ---"
    start_linux_with_config "$linux_config"
    sleep 3

    echo "  Starting Mac FIPS for $phase_label..."
    stop_mac_fips
    sleep 1
    mac_helper start-fips "/tmp/fips-target/release/fips" "$REPO_DIR/testing/ble/configs/$mac_config" "$CAMPAIGN_DIR/test${test_id}-${phase_label}-mac.log" >/dev/null
    sleep 3

    if wait_for_peer "mac" 60; then
        echo "  ✓ Phase $phase_label: peer connected"

        mac_fipsctl show peers > "$(record "$test_id" "${phase_label}-mac-peers")" 2>/dev/null || true
        mac_fipsctl show links > "$(record "$test_id" "${phase_label}-mac-links")" 2>/dev/null || true
        $SSH "sudo fipsctl show peers" > "$(record "$test_id" "${phase_label}-linux-peers")" 2>/dev/null || true
        $SSH "sudo fipsctl show links" > "$(record "$test_id" "${phase_label}-linux-links")" 2>/dev/null || true

        MAC_IPV6_ON_LINUX=$(wait_for_peer_ipv6 linux 30 || true)
        LINUX_IPV6_ON_MAC=$(wait_for_peer_ipv6 mac 30 || true)

        echo "  Linux IPv6 (from Mac peer view): $LINUX_IPV6_ON_MAC"
        echo "  Mac IPv6 (from Linux peer view): $MAC_IPV6_ON_LINUX"

        $SSH "pkill iperf3 2>/dev/null || true"
        mac_stop_iperf_server 2>/dev/null || true

        if [[ "$client_side" == "linux" ]]; then
            if [[ -n "$MAC_IPV6_ON_LINUX" ]]; then
                echo "  Running isolated iperf3 Linux -> Mac (30s)..."
                mac_start_iperf_server >/dev/null
                sleep 1
                $SSH "iperf3 -c $MAC_IPV6_ON_LINUX -6 -t 30 -P 1" > "$(record "$test_id" "${phase_label}-iperf")" 2>&1 || true
            fi
        else
            if [[ -n "$LINUX_IPV6_ON_MAC" ]]; then
                echo "  Running isolated iperf3 Mac -> Linux (30s)..."
                $SSH "iperf3 -s --daemon" 2>/dev/null || true
                sleep 1
                iperf3 -c "$LINUX_IPV6_ON_MAC" -6 -t 30 -P 1 > "$(record "$test_id" "${phase_label}-iperf")" 2>&1 || true
            fi
        fi

        $SSH "pkill iperf3 2>/dev/null || true"
        mac_stop_iperf_server 2>/dev/null || true
    else
        echo "  ✗ Phase $phase_label: failed to establish BLE peer"
    fi

    echo "  Stopping phase $phase_label..."
    stop_mac_fips
    stop_linux_fips
    sleep 3
}

# ========================================================================
# TEST A: Linux outbound → macOS inbound (Mac peripheral)
# ========================================================================

run_test_A() {
    echo ""
    echo "=========================================="
    echo "TEST A: Linux outbound → macOS inbound"
    echo "=========================================="

    start_linux_with_config "linux-testA.yaml"
    sleep 3
    record_test_metadata "A"

    echo "  Starting Mac FIPS (peripheral role, accept_connections=true)..."
    stop_mac_fips
    sleep 1
    mac_helper start-fips "/tmp/fips-target/release/fips" "$REPO_DIR/testing/ble/configs/mac-testA.yaml" "$CAMPAIGN_DIR/testA-mac.log" >/dev/null
    sleep 3

    start_linux_capture "A"

    if wait_for_peer "mac" 60; then
        echo "  ✓ TEST A: Peer connected!"
        echo "  Recording peer state..."
        mac_fipsctl show peers > "$(record A mac-peers)" 2>/dev/null || true
        mac_fipsctl show links > "$(record A mac-links)" 2>/dev/null || true
        $SSH "sudo fipsctl show peers" > "$(record A linux-peers)" 2>/dev/null || true
        $SSH "sudo fipsctl show links" > "$(record A linux-links)" 2>/dev/null || true

        echo "  Running 10s connectivity test..."
        LINUX_FD00=$($SSH "sudo fipsctl show peers" 2>/dev/null | grep -o 'fd00::[0-9a-f]*' | head -1 || true)
        if [[ -n "$LINUX_FD00" ]]; then
            ping6 -c 5 "$LINUX_FD00" > "$(record A ping6)" 2>&1 || true
        fi
    else
        echo "  ✗ TEST A: Failed to establish BLE peer"
    fi

    stop_linux_capture
    collect_linux_artifacts "A"
    cp /tmp/fips-ik-ephemeral-testA.jsonl "$CAMPAIGN_DIR/" 2>/dev/null || true

    echo "  Stopping..."
    stop_mac_fips
    stop_linux_fips
    sleep 3
}

# ========================================================================
# TEST B: macOS outbound → Linux inbound (Mac central)
# ========================================================================

run_test_B() {
    echo ""
    echo "=========================================="
    echo "TEST B: macOS outbound → Linux inbound"
    echo "=========================================="

    start_linux_with_config "linux-testB.yaml"
    sleep 3
    record_test_metadata "B"

    start_linux_capture "B"

    echo "  Starting Mac FIPS (central-only, accept_connections=false)..."
    stop_mac_fips
    sleep 1
    mac_helper start-fips "/tmp/fips-target/release/fips" "$REPO_DIR/testing/ble/configs/mac-testB.yaml" "$CAMPAIGN_DIR/testB-mac.log" >/dev/null
    sleep 3

    if wait_for_peer "mac" 60; then
        echo "  ✓ TEST B: Peer connected!"
        echo "  Recording peer state..."
        mac_fipsctl show peers > "$(record B mac-peers)" 2>/dev/null || true
        mac_fipsctl show links > "$(record B mac-links)" 2>/dev/null || true
        $SSH "sudo fipsctl show peers" > "$(record B linux-peers)" 2>/dev/null || true
        $SSH "sudo fipsctl show links" > "$(record B linux-links)" 2>/dev/null || true

        echo "  Running 10s connectivity test..."
        LINUX_FD00=$($SSH "sudo fipsctl show peers" 2>/dev/null | grep -o 'fd00::[0-9a-f]*' | head -1 || true)
        if [[ -n "$LINUX_FD00" ]]; then
            ping6 -c 5 "$LINUX_FD00" > "$(record B ping6)" 2>&1 || true
        fi
    else
        echo "  ✗ TEST B: Failed to establish BLE peer"
    fi

    stop_linux_capture
    collect_linux_artifacts "B"
    cp /tmp/fips-ik-ephemeral-testB.jsonl "$CAMPAIGN_DIR/" 2>/dev/null || true

    echo "  Stopping..."
    stop_mac_fips
    stop_linux_fips
    sleep 3
}

# ========================================================================
# TEST C: Both directions, both roles (tie-break)
# ========================================================================

run_test_C() {
    echo ""
    echo "=========================================="
    echo "TEST C: Both roles + tie-break"
    echo "=========================================="

    start_linux_with_config "linux-testC.yaml"
    sleep 3
    record_test_metadata "C"

    start_linux_capture "C"

    echo "  Starting Mac FIPS (full dual-role)..."
    stop_mac_fips
    sleep 1
    mac_helper start-fips "/tmp/fips-target/release/fips" "$REPO_DIR/testing/ble/configs/mac-testC.yaml" "$CAMPAIGN_DIR/testC-mac.log" >/dev/null
    sleep 3

    if wait_for_peer "mac" 60; then
        echo "  ✓ TEST C: Peer connected!"
        echo "  Recording peer state..."
        mac_fipsctl show peers > "$(record C mac-peers)" 2>/dev/null || true
        mac_fipsctl show links > "$(record C mac-links)" 2>/dev/null || true
        $SSH "sudo fipsctl show peers" > "$(record C linux-peers)" 2>/dev/null || true
        $SSH "sudo fipsctl show links" > "$(record C linux-links)" 2>/dev/null || true
        $SSH "sudo fipsctl show status" > "$(record C linux-status)" 2>/dev/null || true

        echo "  Verifying single link (no duplicates)..."
        LINK_COUNT=$(mac_fipsctl show links 2>/dev/null | count_ble_links_json || echo "0")
        echo "  BLE links on Mac: $LINK_COUNT"

        echo "  Running reconnect test..."
        echo "  Stopping Mac FIPS..."
        stop_mac_fips
        sleep 5
        echo "  Restarting Mac FIPS..."
        mac_helper start-fips "/tmp/fips-target/release/fips" "$REPO_DIR/testing/ble/configs/mac-testC.yaml" "$CAMPAIGN_DIR/testC-mac-reconnect.log" >/dev/null
        if wait_for_peer "mac" 90; then
            echo "  ✓ Reconnect succeeded!"
        else
            echo "  ✗ Reconnect failed"
        fi
    else
        echo "  ✗ TEST C: Failed to establish BLE peer"
    fi

    stop_linux_capture
    collect_linux_artifacts "C"
    cp /tmp/fips-ik-ephemeral-testC.jsonl "$CAMPAIGN_DIR/" 2>/dev/null || true

    echo "  Stopping..."
    stop_mac_fips
    stop_linux_fips
    sleep 3
}

# ========================================================================
# TEST D: Performance (iperf3)
# ========================================================================

run_test_D() {
    echo ""
    echo "=========================================="
    echo "TEST D: Performance (iperf3)"
    echo "=========================================="

    start_linux_with_config "linux-testD.yaml"
    sleep 3
    record_test_metadata "D"
    start_linux_capture "D"

    echo "  Starting Mac FIPS..."
    stop_mac_fips
    sleep 1
    mac_helper start-fips "/tmp/fips-target/release/fips" "$REPO_DIR/testing/ble/configs/mac-testD.yaml" "$CAMPAIGN_DIR/testD-mac.log" >/dev/null
    sleep 3

    if wait_for_peer "mac" 60; then
        echo "  ✓ Peer connected, starting iperf3..."

        mac_fipsctl show peers > "$(record D mac-peers)" 2>/dev/null || true
        mac_fipsctl show links > "$(record D mac-links)" 2>/dev/null || true
        $SSH "sudo fipsctl show peers" > "$(record D linux-peers)" 2>/dev/null || true
        $SSH "sudo fipsctl show links" > "$(record D linux-links)" 2>/dev/null || true

        MAC_IPV6_ON_LINUX=$(wait_for_peer_ipv6 linux 30 || true)
        LINUX_IPV6_ON_MAC=$(wait_for_peer_ipv6 mac 30 || true)

        echo "  Linux IPv6 (from Mac peer view): $LINUX_IPV6_ON_MAC"
        echo "  Mac IPv6 (from Linux peer view): $MAC_IPV6_ON_LINUX"

        # Start iperf3 server on Linux
        $SSH "iperf3 -s --daemon" 2>/dev/null || true
        mac_stop_iperf_server 2>/dev/null || true
        mac_start_iperf_server >/dev/null
        sleep 1

        # Run iperf3 from Mac → Linux
        if [[ -n "$LINUX_IPV6_ON_MAC" ]]; then
            echo "  Running iperf3 Mac → Linux (30s)..."
            iperf3 -c "$LINUX_IPV6_ON_MAC" -6 -t 30 -P 1 > "$(record D iperf-mac-to-linux)" 2>&1 || true
        fi

        # Run iperf3 from Linux → Mac
        if [[ -n "$MAC_IPV6_ON_LINUX" ]]; then
            echo "  Running iperf3 Linux → Mac (30s)..."
            $SSH "iperf3 -c $MAC_IPV6_ON_LINUX -6 -t 30 -P 1" > "$(record D iperf-linux-to-mac)" 2>&1 || true
        fi

        $SSH "pkill iperf3 2>/dev/null || true"
        mac_stop_iperf_server 2>/dev/null || true
    else
        echo "  ✗ TEST D: Failed to establish BLE peer"
    fi

    stop_linux_capture
    collect_linux_artifacts "D"

    echo "  Stopping..."
    stop_mac_fips
    stop_linux_fips
    sleep 3
}

run_test_E() {
    echo ""
    echo "=========================================="
    echo "TEST E: Isolated directional performance"
    echo "=========================================="

    start_linux_capture "E"

    capture_remote_provenance "$(record E provenance-linux-to-mac)"
    run_directional_iperf_phase "E" "linux-to-mac" "linux" "linux-testD.yaml" "mac-testD.yaml"

    capture_remote_provenance "$(record E provenance-mac-to-linux)"
    run_directional_iperf_phase "E" "mac-to-linux" "mac" "linux-testD.yaml" "mac-testD.yaml"

    stop_linux_capture
    collect_linux_artifacts "E"
}

# ========================================================================
# Summary
# ========================================================================

print_summary() {
    echo ""
    echo "=========================================="
    echo "CAMPAIGN SUMMARY"
    echo "=========================================="
    echo "Local branch: $LOCAL_BRANCH"
    echo "Local commit: $LOCAL_COMMIT"
    echo "Date: $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    echo ""
    echo "Artifacts in $CAMPAIGN_DIR/:"
    ls -la "$CAMPAIGN_DIR/" 2>/dev/null || echo "  (empty)"
    echo ""
    echo "Next steps:"
    echo "  1. Review test*-mac.log and test*-linux-journal.log"
    echo "  2. Analyze BLE captures: wireshark $CAMPAIGN_DIR/test*-ble.pklg"
    echo "  3. Decrypt traffic using ephemeral key logs: $CAMPAIGN_DIR/fips-ik-ephemeral-test*.jsonl"
    echo "  4. Review iperf results: cat $CAMPAIGN_DIR/testD-*"
    echo ""
    echo "To document this run:"
    echo "  See $CAMPAIGN_DIR/test*-provenance.txt for per-test branch/commit/dirty state"
}

# --- Run selected tests ---

case "$TEST" in
    A) run_test_A ;;
    B) run_test_B ;;
    C) run_test_C ;;
    D) run_test_D ;;
    E) run_test_E ;;
    all)
        run_test_A
        run_test_B
        run_test_C
        run_test_D
        run_test_E
        ;;
    *)
        echo "Unknown test: $TEST (use A, B, C, D, E, or all)"
        exit 1
        ;;
esac

print_summary
