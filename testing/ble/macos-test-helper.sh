#!/bin/bash
set -euo pipefail

HELPER_DIR="/usr/local/libexec/fips-test"
PID_DIR="/var/run/fips-test"
LOG_DIR="/tmp/fips-ble-campaign"

require_root() {
    if [ "$(id -u)" -ne 0 ]; then
        echo "Error: run with sudo" >&2
        exit 1
    fi
}

ensure_dirs() {
    install -d -m 755 "$HELPER_DIR"
    install -d -m 755 "$PID_DIR"
    install -d -m 777 "$LOG_DIR"
}

start_fips() {
    local binary="$1"
    local config="$2"
    local log_file="$3"

    if [ ! -x "$binary" ]; then
        echo "Error: binary not executable: $binary" >&2
        exit 1
    fi
    if [ ! -f "$config" ]; then
        echo "Error: config not found: $config" >&2
        exit 1
    fi

    ensure_dirs
    stop_fips || true

    nohup "$binary" -c "$config" >"$log_file" 2>&1 &
    local pid=$!
    echo "$pid" > "$PID_DIR/fips.pid"
    echo "$pid"
}

stop_fips() {
    local pid_file="$PID_DIR/fips.pid"
    if [ -f "$pid_file" ]; then
        local pid
        pid="$(cat "$pid_file")"
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
            sleep 1
            kill -9 "$pid" 2>/dev/null || true
        fi
        rm -f "$pid_file"
    fi
    pkill -f '^/tmp/fips-target/release/fips -c ' 2>/dev/null || true
    pkill -f '^/usr/local/bin/fips --config ' 2>/dev/null || true
}

status_fips() {
    local pid_file="$PID_DIR/fips.pid"
    if [ ! -f "$pid_file" ]; then
        echo "stopped"
        return 1
    fi

    local pid
    pid="$(cat "$pid_file")"
    if kill -0 "$pid" 2>/dev/null; then
        echo "running $pid"
        return 0
    fi

    echo "stale $pid"
    return 1
}

run_fipsctl() {
    if [ "$#" -lt 1 ]; then
        echo "Error: run-fipsctl requires arguments" >&2
        exit 1
    fi

    local fipsctl_bin="/usr/local/bin/fipsctl"
    if [ ! -x "$fipsctl_bin" ]; then
        fipsctl_bin="/tmp/fips-target/release/fipsctl"
    fi
    if [ ! -x "$fipsctl_bin" ]; then
        echo "Error: fipsctl binary not found in /usr/local/bin or /tmp/fips-target/release" >&2
        exit 1
    fi

    "$fipsctl_bin" --socket /tmp/fips-control.sock "$@"
}

start_iperf_server() {
    ensure_dirs
    pkill -f '^iperf3 -s' 2>/dev/null || true
    nohup iperf3 -s >/tmp/fips-ble-campaign/testD-mac-iperf-server.log 2>&1 &
    local pid=$!
    echo "$pid" > "$PID_DIR/iperf3.pid"
    echo "$pid"
}

stop_iperf_server() {
    local pid_file="$PID_DIR/iperf3.pid"
    if [ -f "$pid_file" ]; then
        local pid
        pid="$(cat "$pid_file")"
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
            sleep 1
            kill -9 "$pid" 2>/dev/null || true
        fi
        rm -f "$pid_file"
    fi
    pkill -f '^iperf3 -s' 2>/dev/null || true
}

case "${1:-}" in
    start-fips)
        require_root
        shift
        start_fips "$@"
        ;;
    stop-fips)
        require_root
        stop_fips
        ;;
    status-fips)
        require_root
        status_fips
        ;;
    run-fipsctl)
        require_root
        shift
        run_fipsctl "$@"
        ;;
    start-iperf-server)
        require_root
        start_iperf_server
        ;;
    stop-iperf-server)
        require_root
        stop_iperf_server
        ;;
    *)
        echo "Usage: $0 {start-fips <binary> <config> <log-file>|stop-fips|status-fips|run-fipsctl <args...>|start-iperf-server|stop-iperf-server}" >&2
        exit 1
        ;;
esac
