#!/bin/bash
# ── Bare-metal build of the no_std core ─────────────────────────────────────
# Builds the fips-core library for two targets that have no standard library
# at all: thumbv7em-none-eabihf (Cortex-M4F) and riscv32imac-unknown-none-elf.
# The host fence (clippy and tests on -p fips-core) cannot catch a dependency,
# or a feature of one, that links std, because std exists on the host. Here
# any such crate fails with "can't find crate for `std`".
#
# Only -p fips-core: building the root crate alongside would fail on its own
# std use and, through feature unification, turn std back on for the shared
# dependencies.
#
# secp256k1-sys compiles C, so each target needs a C compiler and archiver.
# clang cross-compiles for both from its target flag, and llvm-ar archives
# their objects. The "'string.h' file not found" build-script warning is
# expected: secp256k1-sys retries with headers it bundles.
#
# Needs the standard library of both targets for the toolchain in
# rust-toolchain.toml, clang, and llvm-ar (any llvm-ar-<version> will do).
# The targets are deliberately not listed in rust-toolchain.toml, so not every
# rustup user fetches them; the cost is that they are per toolchain, so when
# rust-toolchain.toml moves to a new release every machine that runs this
# (developers, the internal builder) must add them again, or this exits 2.
# They are found in the active toolchain's sysroot, which works for rustup and
# for toolchains it does not manage (the Nix dev shell provides neither target).
#
# Run on every push by the core-baremetal job in .github/workflows/ci.yml,
# and by testing/ci-local.sh (Stage 1) when asked for with --baremetal or
# FIPS_CI_BAREMETAL=1, so both runners build the same thing. Local CI leaves
# it off by default because of the prerequisites above; a landing or merge-up
# on a trunk path runs it explicitly.
#
# Exit status: 0 built, 1 the build failed, 2 a prerequisite is missing.

set -uo pipefail

TARGETS=(thumbv7em-none-eabihf riscv32imac-unknown-none-elf)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/.." || { echo "build-core-baremetal: cannot cd to the repository root" >&2; exit 2; }

sysroot=$(rustc --print sysroot) || sysroot=""
if [[ -z "$sysroot" ]]; then
    echo "build-core-baremetal: 'rustc --print sysroot' failed" >&2
    exit 2
fi
for t in "${TARGETS[@]}"; do
    if [[ ! -d "$sysroot/lib/rustlib/$t" ]]; then
        echo "build-core-baremetal: the toolchain at $sysroot has no standard library for $t" >&2
        if command -v rustup >/dev/null 2>&1; then
            echo "build-core-baremetal: install it with 'rustup target add $t'" >&2
        else
            echo "build-core-baremetal: add the $t target to whatever provides this toolchain" >&2
        fi
        exit 2
    fi
done

if ! command -v clang >/dev/null 2>&1; then
    echo "build-core-baremetal: clang not found; install it with 'apt-get install clang'" >&2
    exit 2
fi
ar_tool=$(command -v llvm-ar || compgen -c llvm-ar- | sort -V | tail -n 1)
if [[ -z "$ar_tool" ]]; then
    echo "build-core-baremetal: llvm-ar not found; install it with 'apt-get install llvm'" >&2
    exit 2
fi

target_args=()
for t in "${TARGETS[@]}"; do
    var=${t//-/_}
    export "CC_$var=clang" "AR_$var=$ar_tool"
    target_args+=(--target "$t")
done

echo "build-core-baremetal: CC=clang ($(clang --version | head -n 1)), AR=$ar_tool"
cargo build -p fips-core "${target_args[@]}" || exit 1
