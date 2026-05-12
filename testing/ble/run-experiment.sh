#!/usr/bin/env bash
# FIPS BLE Systematic Experiment Runner
#
# Runs a single BLE durability experiment with full instrumentation:
# - btmon HCI capture on Linux (via SSH to 218)
# - FIPS event_log JSONL on both platforms
# - Automated startup, timed run, and shutdown
#
# Usage: ./run-experiment.sh <experiment_name> <config_yaml> [duration_mins]
#
# Example: ./run-experiment.sh exp1-baseline /tmp/fips-exp117.yaml 30
#
# Output files in /tmp/fips-experiments/<experiment_name>/:
#   mac-events.jsonl       - macOS FIPS event log
#   linux-events.jsonl     - Linux FIPS event log
#   btmon-raw.log          - Raw btmon HCI capture (Linux)
#   btmon-summary.txt      - Filtered btmon (L2CAP/connection events only)
#   config.yaml            - Copy of config used
#   experiment.log         - Timestamped experiment log
#
# Prerequisites:
#   - Mac: caffeinate, fips binary built with just build
#   - Linux (218): SSH alias configured, cargo env sourced, fips binary built
#   - Linux: sudo btmon available (apt install bluez-tools)

set -euo pipefail

EXPERIMENT="${1:?Usage: run-experiment.sh <name> <config> [duration_mins]}"
CONFIG="${2:?Usage: run-experiment.sh <name> <config> [duration_mins]}"
DURATION_MINS="${3:-30}"
LINUX_HOST="218"
LINUX_REPO="/home/ubuntu/fips"

# Experiment output directory
EXP_DIR="/tmp/fips-experiments/${EXPERIMENT}"
mkdir -p "${EXP_DIR}"

# Save config used
cp "${CONFIG}" "${EXP_DIR}/config.yaml"

LOG="${EXP_DIR}/experiment.log"

log() {
    echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $*" | tee -a "${LOG}"
}

log "=== FIPS BLE Experiment: ${EXPERIMENT} ==="
log "Config: ${CONFIG}"
log "Duration: ${DURATION_MINS} minutes"
log "Output: ${EXP_DIR}"
log ""

# --- Disk space management ---
MIN_DISK_GB=5

disk_free_gb() {
    df -g / 2>/dev/null | awk 'NR==2{print $4}' || df -h / | awk 'NR==2{print $4}'
}

clean_cargo_artifacts() {
    local target_dir="${1:-target}"
    local freed=0
    for dir in "${target_dir}/debug/deps" "${target_dir}/debug/incremental" \
               "${target_dir}/debug/.fingerprint" "${target_dir}/release/deps" \
               "${target_dir}/release/incremental" "${target_dir}/release/.fingerprint"; do
        if [ -d "$dir" ]; then
            local sz=$(du -sm "$dir" 2>/dev/null | awk '{print $1}')
            rm -rf "$dir" 2>/dev/null && freed=$((freed + sz))
        fi
    done
    echo "$freed"
}

free_gb=$(disk_free_gb)
log "Disk free: ${free_gb}GB"

if [ "$free_gb" -lt "$MIN_DISK_GB" ]; then
    log "Low disk (${free_gb}GB < ${MIN_DISK_GB}GB). Cleaning cargo artifacts..."
    freed_mb=$(clean_cargo_artifacts "target")
    log "Freed ~${freed_mb}MB from target/"
    free_gb=$(disk_free_gb)
    log "Disk free after cleanup: ${free_gb}GB"

    if [ "$free_gb" -lt "$MIN_DISK_GB" ]; then
        log "WARNING: Still low on disk (${free_gb}GB). Cleaning old experiments..."
        find /tmp/fips-experiments/ -mindepth 1 -maxdepth 1 -mtime +1 -exec rm -rf {} \; 2>/dev/null || true
        free_gb=$(disk_free_gb)
        log "Disk free after experiment cleanup: ${free_gb}GB"
    fi

    if [ "$free_gb" -lt 1 ]; then
        log "ERROR: Critically low disk (${free_gb}GB). Aborting."
        exit 1
    fi
fi

# --- Cleanup function ---
cleanup() {
    log "Cleaning up..."

    # Stop Mac FIPS
    log "Stopping Mac FIPS..."
    pkill -9 fips 2>/dev/null || true
    sleep 1

    # Stop Linux FIPS
    log "Stopping Linux FIPS..."
    ssh "${LINUX_HOST}" "pkill -9 fips" 2>/dev/null || true
    sleep 1

    # Stop btmon
    log "Stopping btmon..."
    ssh "${LINUX_HOST}" "sudo pkill -9 btmon" 2>/dev/null || true
    sleep 1

    # Verify both stopped
    local mac_running=$(pgrep -x fips 2>/dev/null | wc -l | tr -d ' ' || echo 0)
    local linux_running=$(ssh "${LINUX_HOST}" "pgrep -x fips 2>/dev/null | wc -l" 2>/dev/null | tr -d ' ' || echo 0)

    if [ "${mac_running:-0}" -gt 0 ] || [ "${linux_running:-0}" -gt 0 ]; then
        log "WARNING: Processes still running! Mac=${mac_running}, Linux=${linux_running}"
    else
        log "All processes stopped cleanly."
    fi

    # Post-experiment disk cleanup
    log "Cleaning cargo artifacts..."
    freed_mb=$(clean_cargo_artifacts "target")
    log "Freed ~${freed_mb}MB from target/"
}

trap cleanup EXIT

# --- Build both platforms ---
log "Building Mac..."
just build 2>&1 | tail -1 | tee -a "${LOG}"

log "Building Linux..."
ssh "${LINUX_HOST}" "source ~/.cargo/env && cd ${LINUX_REPO} && cargo build" 2>&1 | tail -1 | tee -a "${LOG}"

# --- Copy config to Linux ---
CONFIG_FILENAME=$(basename "${CONFIG}")
scp "${CONFIG}" "${LINUX_HOST}:/tmp/${CONFIG_FILENAME}" 2>&1 | tee -a "${LOG}"

# --- Kill any existing processes ---
log "Killing existing processes..."
pkill -9 fips 2>/dev/null || true
ssh "${LINUX_HOST}" "pkill -9 fips" 2>/dev/null || true
ssh "${LINUX_HOST}" "sudo pkill -9 btmon" 2>/dev/null || true
sleep 2

# --- Start btmon on Linux ---
log "Starting btmon HCI capture on Linux..."
ssh "${LINUX_HOST}" "sudo btmon -i hci0 -w ${LINUX_REPO}/btmon-${EXPERIMENT}.log" > /dev/null 2>&1 &
BTMON_SSH_PID=$!
sleep 3

# --- Start Linux FIPS ---
log "Starting Linux FIPS..."
ssh -f "${LINUX_HOST}" \
    "source ~/.cargo/env && cd ${LINUX_REPO} && \
     FIPS_BLE_EVENT_LOG=/tmp/fips-${EXPERIMENT}-linux.jsonl \
     RUST_LOG=fips::transport::ble=debug \
     nohup ./target/debug/fips --config /tmp/${CONFIG_FILENAME} \
     > /tmp/fips-${EXPERIMENT}-linux-stdout.log 2>&1"
sleep 3

# Verify Linux started
LINUX_PID=$(ssh "${LINUX_HOST}" "pgrep -x fips" 2>/dev/null || echo "")
if [ -z "$LINUX_PID" ]; then
    log "ERROR: Linux FIPS failed to start!"
    exit 1
fi
log "Linux FIPS started (PID: ${LINUX_PID})"

# --- Start Mac FIPS ---
log "Starting Mac FIPS..."
FIPS_BLE_EVENT_LOG="${EXP_DIR}/mac-events.jsonl" \
RUST_LOG=fips::transport::ble=debug \
caffeinate -is ./target/debug/fips --config "${CONFIG}" \
    > "${EXP_DIR}/mac-stdout.log" 2>&1 &
MAC_PID=$!
sleep 3

# Verify Mac started
if ! kill -0 "$MAC_PID" 2>/dev/null; then
    log "ERROR: Mac FIPS failed to start!"
    cat "${EXP_DIR}/mac-stdout.log" | tail -20 | tee -a "${LOG}"
    exit 1
fi
log "Mac FIPS started (PID: ${MAC_PID})"

# --- Wait for experiment duration ---
log "Experiment running for ${DURATION_MINS} minutes..."
log "Started at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
log "Will end at: $(date -u -v+${DURATION_MINS}M +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d \"+${DURATION_MINS} minutes\" +%Y-%m-%dT%H:%M:%SZ)"

sleep "$((DURATION_MINS * 60))"

log "Experiment duration complete."

# --- Stop everything ---
log "Stopping Mac FIPS..."
kill "$MAC_PID" 2>/dev/null || true
sleep 2
pkill -9 fips 2>/dev/null || true

log "Stopping Linux FIPS..."
ssh "${LINUX_HOST}" "pkill -9 fips" 2>/dev/null || true
sleep 2

log "Stopping btmon..."
ssh "${LINUX_HOST}" "sudo pkill -9 btmon" 2>/dev/null || true
kill $BTMON_SSH_PID 2>/dev/null || true
sleep 2

# --- Collect results ---
log "Collecting results..."

# Copy Linux event log
scp "${LINUX_HOST}:/tmp/fips-${EXPERIMENT}-linux.jsonl" "${EXP_DIR}/linux-events.jsonl" 2>/dev/null || {
    log "WARNING: No Linux event log found"
    touch "${EXP_DIR}/linux-events.jsonl"
}

# Copy Linux stdout
scp "${LINUX_HOST}:/tmp/fips-${EXPERIMENT}-linux-stdout.log" "${EXP_DIR}/linux-stdout.log" 2>/dev/null || true

# Copy btmon capture
scp "${LINUX_HOST}:${LINUX_REPO}/btmon-${EXPERIMENT}.log" "${EXP_DIR}/btmon-raw.log" 2>/dev/null || {
    log "WARNING: No btmon capture found"
    touch "${EXP_DIR}/btmon-raw.log"
}

# Generate btmon summary (filtered for L2CAP and connection events)
if [ -s "${EXP_DIR}/btmon-raw.log" ]; then
    log "Generating btmon summary..."
    # btmon binary format needs re-reading; capture text summary instead
    # We'll note this for future improvement
    echo "btmon raw capture saved. Analyze with: btmon -r ${EXP_DIR}/btmon-raw.log" > "${EXP_DIR}/btmon-summary.txt"
fi

# --- Analyze ---
log ""
log "=== Analysis ==="

if [ -s "${EXP_DIR}/mac-events.jsonl" ] && [ -s "${EXP_DIR}/linux-events.jsonl" ]; then
    "$(dirname "$0")/analyze-experiment.sh" \
        "${EXP_DIR}/mac-events.jsonl" \
        "${EXP_DIR}/linux-events.jsonl" \
        "${DURATION_MINS}" \
        "${EXPERIMENT}" \
        2>&1 | tee -a "${LOG}"
else
    log "WARNING: Missing event logs, skipping analysis"
fi

log ""
log "=== Experiment ${EXPERIMENT} Complete ==="
log "Results in: ${EXP_DIR}/"
log "  mac-events.jsonl:   $(wc -l < "${EXP_DIR}/mac-events.jsonl" 2>/dev/null || echo 0) events"
log "  linux-events.jsonl: $(wc -l < "${EXP_DIR}/linux-events.jsonl" 2>/dev/null || echo 0) events"
log "  btmon-raw.log:      $(du -h "${EXP_DIR}/btmon-raw.log" 2>/dev/null | cut -f1 || echo "N/A")"
