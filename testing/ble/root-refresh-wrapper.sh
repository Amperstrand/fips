#!/bin/bash
set -euo pipefail

while getopts "c:" opt; do
    case "$opt" in
        c) ;;
    esac
done

/Users/macbook/src/fips/testing/ble/refresh-installed-mac-helper.sh
