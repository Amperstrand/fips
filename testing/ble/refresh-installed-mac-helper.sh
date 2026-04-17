#!/bin/bash
set -euo pipefail

install -m 755 \
  /Users/macbook/src/fips/testing/ble/macos-test-helper.sh \
  /usr/local/libexec/fips-test/macos-test-helper.sh
