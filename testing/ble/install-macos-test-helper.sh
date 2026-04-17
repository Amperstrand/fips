#!/bin/bash
set -euo pipefail

if [ "$(id -u)" -ne 0 ]; then
    echo "Error: run with sudo" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SOURCE_HELPER="$SCRIPT_DIR/macos-test-helper.sh"

if [ ! -f "$SOURCE_HELPER" ]; then
    echo "Error: source helper not found: $SOURCE_HELPER" >&2
    exit 1
fi

REAL_USER="${SUDO_USER:-$(stat -f '%Su' /dev/console 2>/dev/null || true)}"
if [ -z "$REAL_USER" ] || [ "$REAL_USER" = "root" ]; then
    echo "Error: could not determine non-root user for sudoers entry" >&2
    exit 1
fi

DEST_DIR="/usr/local/libexec/fips-test"
DEST_HELPER="$DEST_DIR/macos-test-helper.sh"
SUDOERS_FILE="/etc/sudoers.d/fips-ble-test"

install -d -m 755 "$DEST_DIR"
install -d -m 755 "/etc/sudoers.d"
install -m 755 "$SOURCE_HELPER" "$DEST_HELPER"

cat > "$SUDOERS_FILE" <<EOF
$REAL_USER ALL=(root) NOPASSWD: $DEST_HELPER
EOF
chmod 440 "$SUDOERS_FILE"

visudo -cf "$SUDOERS_FILE"

echo "Installed macOS BLE test helper for user: $REAL_USER"
echo "Helper: $DEST_HELPER"
echo "Sudoers: $SUDOERS_FILE"
echo "Example: sudo $DEST_HELPER status-fips"
