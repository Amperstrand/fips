#!/bin/bash
# Run a FIPS BLE integration test.
#
# Unlike chaos.sh which uses Docker, this runs FIPS nodes as native processes.
# Requires Bluetooth hardware and (on Linux) BlueZ.
#
# Usage: ./scripts/ble-test.sh <scenario> [options]
#   scenario: path to YAML file, or scenario name (e.g., "ble-smoke")
#
# Options:
#   -v, --verbose          Enable debug logging
#   --seed <N>             Override scenario seed
#   --duration <secs>      Override scenario duration
#   --capture              Enable BLE traffic capture via btmon (Linux only)
#   --fips <path>          Path to FIPS binary (default: auto-detect)
#   --list                 List available scenarios
#
# Examples:
#   ./scripts/ble-test.sh ble-smoke
#   ./scripts/ble-test.sh ble-only --capture
#   ./scripts/ble-test.sh ble-cost --verbose --duration 120
set -e

trap 'echo ""; echo "BLE test interrupted"; exit 130' INT

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CHAOS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SCENARIO_DIR="$CHAOS_DIR/scenarios"

usage() {
    echo "Usage: $0 <scenario> [options]"
    echo ""
    echo "Arguments:"
    echo "  scenario            Path to YAML file, or scenario name (e.g., ble-smoke)"
    echo ""
    echo "Options:"
    echo "  -v, --verbose       Enable debug logging"
    echo "  --seed <N>          Override scenario seed"
    echo "  --duration <secs>   Override scenario duration"
    echo "  --capture           Enable BLE traffic capture via btmon (Linux only)"
    echo "  --fips <path>       Path to FIPS binary (default: auto-detect)"
    echo "  --list              List available scenarios"
    exit 1
}

list_scenarios() {
    echo "=== Available BLE Scenarios ==="
    echo ""
    for f in "$SCENARIO_DIR"/ble-*.yaml; do
        [ -f "$f" ] || continue
        echo "  $(basename "$f" .yaml)"
    done
    exit 0
}

for arg in "$@"; do
    case "$arg" in
        --list) list_scenarios ;;
    esac
done

[ $# -lt 1 ] && usage

SCENARIO_ARG=""
VERBOSE=""
SEED=""
DURATION=""
CAPTURE=""
FIPS_PATH=""

while [ $# -gt 0 ]; do
    case "$1" in
        -v|--verbose) VERBOSE="--verbose"; shift ;;
        --seed)       SEED="$2"; shift 2 ;;
        --duration)   DURATION="$2"; shift 2 ;;
        --capture)    CAPTURE="1"; shift ;;
        --fips)       FIPS_PATH="$2"; shift 2 ;;
        --list)       list_scenarios ;;
        -*)           echo "Error: Unknown option '$1'" >&2; usage ;;
        *)
            if [ -z "$SCENARIO_ARG" ]; then
                SCENARIO_ARG="$1"
            else
                echo "Error: Unexpected argument '$1'" >&2
                usage
            fi
            shift
            ;;
    esac
done

[ -z "$SCENARIO_ARG" ] && usage

# Resolve scenario path
if [ -f "$SCENARIO_ARG" ]; then
    SCENARIO_FILE="$SCENARIO_ARG"
elif [ -f "$SCENARIO_DIR/$SCENARIO_ARG.yaml" ]; then
    SCENARIO_FILE="$SCENARIO_DIR/$SCENARIO_ARG.yaml"
else
    echo "Error: Scenario not found: $SCENARIO_ARG" >&2
    echo "Tried:" >&2
    echo "  $SCENARIO_ARG" >&2
    echo "  $SCENARIO_DIR/$SCENARIO_ARG.yaml" >&2
    echo "" >&2
    echo "Available BLE scenarios:" >&2
    for f in "$SCENARIO_DIR"/ble-*.yaml; do
        [ -f "$f" ] || continue
        echo "  $(basename "$f" .yaml)" >&2
    done
    exit 1
fi

# --- Prerequisites ---

if ! command -v python3 &> /dev/null; then
    echo "Error: python3 not found" >&2
    exit 1
fi

# Detect FIPS binary
if [ -n "$FIPS_PATH" ]; then
    if [ ! -x "$FIPS_PATH" ]; then
        echo "Error: FIPS binary not executable: $FIPS_PATH" >&2
        exit 1
    fi
elif [ -x "$CHAOS_DIR/../../target/release/fips" ]; then
    FIPS_PATH="$CHAOS_DIR/../../target/release/fips"
elif [ -x "$CHAOS_DIR/../../target/debug/fips" ]; then
    FIPS_PATH="$CHAOS_DIR/../../target/debug/fips"
elif command -v fips &> /dev/null; then
    FIPS_PATH="$(command -v fips)"
else
    echo "Error: FIPS binary not found" >&2
    echo "Build with: cargo build --release --features ble" >&2
    echo "Or specify path with --fips <path>" >&2
    exit 1
fi

# Verify BLE feature is enabled
if ! "$FIPS_PATH" --version 2>&1 | grep -qi "ble\|bluetooth" 2>/dev/null; then
    echo "Warning: FIPS binary may not have BLE support." >&2
    echo "Build with: cargo build --release --features ble" >&2
fi

# Check Bluetooth hardware (OS-specific)
OS="$(uname -s)"

check_bluetooth_linux() {
    if ! command -v hciconfig &> /dev/null && ! command -v btmgmt &> /dev/null; then
        echo "Error: Bluetooth tools not found (install bluez)" >&2
        exit 1
    fi

    if command -v hciconfig &> /dev/null; then
        if ! hciconfig hci0 up &> /dev/null 2>&1; then
            echo "Error: No Bluetooth adapter found or hci0 is down" >&2
            echo "Try: sudo hciconfig hci0 up" >&2
            exit 1
        fi
    elif command -v btmgmt &> /dev/null; then
        if ! btmgmt info &> /dev/null 2>&1; then
            echo "Error: No Bluetooth adapter found" >&2
            exit 1
        fi
    fi
}

check_bluetooth_macos() {
    if ! system_profiler SPBluetoothDataType 2>/dev/null | grep -q "Bluetooth"; then
        echo "Error: No Bluetooth hardware detected" >&2
        exit 1
    fi
}

case "$OS" in
    Linux)
        check_bluetooth_linux
        ;;
    Darwin)
        check_bluetooth_macos
        echo "Warning: macOS BLE support is limited. Linux with BlueZ is recommended." >&2
        ;;
    *)
        echo "Error: Unsupported OS: $OS" >&2
        exit 1
        ;;
esac

# BLE capture via btmon (Linux only)
BTMON_PID=""
if [ -n "$CAPTURE" ]; then
    if [ "$OS" != "Linux" ]; then
        echo "Error: --capture requires Linux with btmon (bluez)" >&2
        exit 1
    fi
    if ! command -v btmon &> /dev/null; then
        echo "Error: btmon not found (install bluez)" >&2
        exit 1
    fi
    CAPTURE_FILE="$(date +%Y%m%d-%H%M%S)-ble-capture.log"
    btmon -w "$CAPTURE_FILE" &
    BTMON_PID=$!
    trap 'kill $BTMON_PID 2>/dev/null; rm -f "$PATCHED"' EXIT
    echo "  Capture: $CAPTURE_FILE"
fi

# Build python args
PYTHON_ARGS=("$SCENARIO_FILE" "--native")
[ -n "$VERBOSE" ] && PYTHON_ARGS+=("$VERBOSE")
[ -n "$SEED" ] && PYTHON_ARGS+=("--seed" "$SEED")
[ -n "$DURATION" ] && PYTHON_ARGS+=("--duration" "$DURATION")

echo "=== FIPS BLE Integration Test ==="
echo ""
echo "  Scenario: $(basename "$SCENARIO_FILE" .yaml)"
echo "  File:     $SCENARIO_FILE"
echo "  Binary:   $FIPS_PATH"
echo "  Backend:  native"
echo "  OS:       $OS"
[ -n "$SEED" ] && echo "  Seed:     $SEED (override)"
[ -n "$DURATION" ] && echo "  Duration: ${DURATION}s (override)"
[ -n "$BTMON_PID" ] && echo "  Capture:  $CAPTURE_FILE (PID $BTMON_PID)"
echo ""

# Run from testing/chaos directory (sim expects relative paths)
cd "$CHAOS_DIR"
python3 -m sim "${PYTHON_ARGS[@]}"

# Clean up btmon on success
if [ -n "$BTMON_PID" ]; then
    kill "$BTMON_PID" 2>/dev/null || true
fi
