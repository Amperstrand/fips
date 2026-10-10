#!/bin/bash
# ── Cross-target compile check ──────────────────────────────────────────────
# Compiles the crate, its tests, examples and benches for the non-Linux
# targets FIPS ships on, so a change that breaks the macOS, Windows or FreeBSD
# build fails here rather than first on GitHub. The usual shape of such a break
# is a Unix-only item (a std::os::unix import, a libc call, a field that exists
# only under cfg(unix)) used in code that is compiled on every target without
# a cfg gate.
#
# Usage: check.sh [--list | <target>...]
#   With no argument, checks every target below in turn. With targets named,
#   checks only those, each of which must be on the list. --list prints the
#   targets, one per line, and exits; ci-local.sh reads it so that it records
#   one result per target without keeping a second copy of the list.
#
# Each target runs
#   cargo check --workspace --all-targets --target <target>
# which type-checks and runs build scripts but does not link, so no target
# linker or SDK is needed. Build scripts that compile C through the cc crate
# get zig as their C compiler and archiver, through the zig-cc and zig-ar
# wrappers beside this script. The Linux target is not checked here: the build
# and clippy stages of ci-local.sh compile it already.
#
# All targets build in one target directory,
#   ${CARGO_TARGET_DIR:-<repo>/target}/cross-check
# where cargo keeps each target's output under its own <target>/ and builds the
# host-side build scripts and proc macros once for all three. It is kept
# between runs so incremental reuse works, and kept apart from the normal build
# so neither invalidates the other. Like any cargo target directory it grows
# as the source changes; `cargo clean` removes it.
#
# Requirements, each reported by name when missing:
#   zig       ${ZIG:-zig}, found on PATH unless ZIG names it (0.16.0 is the
#             version this was written against).
#   the Rust standard library for each target, for the toolchain
#             rust-toolchain.toml pins. A missing one is installed with
#             `rustup target add` when rustup is present. The targets are
#             deliberately not listed in rust-toolchain.toml: rustup would
#             then fetch them for every cargo call in the repository, on every
#             host and in every container, and fail every one of those calls
#             when the download server cannot be reached.
#
# Exit 0 = every target compiled. Exit 1 = at least one target failed to
# compile. Exit 2 = the check could not run (zig, cargo or a target's standard
# library missing and not installable, or an unknown target named); never
# treated as a pass.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Rust target triple, and the zig target its C code is compiled for.
TARGETS=(
    "x86_64-apple-darwin x86_64-macos"
    "x86_64-pc-windows-gnu x86_64-windows-gnu"
    "x86_64-unknown-freebsd x86_64-freebsd"
)

cannot() { echo "cross-check: $*" >&2; exit 2; }

# The zig target for a Rust triple, or nothing if the triple is not listed.
zig_target() {
    local entry
    for entry in "${TARGETS[@]}"; do
        if [[ "${entry%% *}" == "$1" ]]; then
            echo "${entry#* }"
            return 0
        fi
    done
    return 1
}

list_targets() {
    local entry
    for entry in "${TARGETS[@]}"; do echo "${entry%% *}"; done
    return 0
}

if [[ "${1:-}" == "--list" ]]; then
    list_targets
    exit 0
fi

selected=("$@")
if [[ ${#selected[@]} -eq 0 ]]; then
    mapfile -t selected < <(list_targets)
fi
for t in "${selected[@]}"; do
    zig_target "$t" >/dev/null || cannot "unknown target '$t'; known targets: $(list_targets | tr '\n' ' ')"
done

cd "$PROJECT_ROOT" || cannot "cannot cd to $PROJECT_ROOT"

command -v cargo >/dev/null 2>&1 || cannot "cargo not found on PATH"
command -v rustc >/dev/null 2>&1 || cannot "rustc not found on PATH"

zig_name="${ZIG:-zig}"
zig_path="$(command -v "$zig_name" 2>/dev/null)" \
    || cannot "zig not found (looked for '$zig_name' on PATH). Install zig 0.16.0 or set ZIG to its path, or run ci-local.sh without --cross-check."
zig_version="$("$zig_path" version 2>/dev/null)" \
    || cannot "'$zig_path version' failed; the zig install there is broken"
export ZIG="$zig_path"

# Resolved inside the repository, so rustup applies rust-toolchain.toml and
# `rustup target add` installs into the toolchain cargo will use.
sysroot="$(rustc --print sysroot 2>/dev/null)" || cannot "'rustc --print sysroot' failed"
for t in "${selected[@]}"; do
    [[ -d "$sysroot/lib/rustlib/$t/lib" ]] && continue
    command -v rustup >/dev/null 2>&1 \
        || cannot "Rust target $t is not installed for the toolchain at $sysroot, and there is no rustup to install it."
    echo "cross-check: installing Rust target $t (rustup target add $t)"
    rustup target add "$t" \
        || cannot "Rust target $t is not installed for the toolchain at $sysroot, and 'rustup target add $t' failed."
    [[ -d "$sysroot/lib/rustlib/$t/lib" ]] \
        || cannot "'rustup target add $t' succeeded, but $sysroot/lib/rustlib/$t/lib is still missing."
done

echo "cross-check: zig $zig_version at $zig_path"
echo "cross-check: $(rustc --version)"

target_root="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}/cross-check"
failed=()
for t in "${selected[@]}"; do
    z="$(zig_target "$t")"
    v="${t//-/_}"
    start=$SECONDS
    echo "==> cargo check --workspace --all-targets --target $t (C via zig cc -target $z)"
    rc=0
    env "CC_$v=$SCRIPT_DIR/zig-cc" "AR_$v=$SCRIPT_DIR/zig-ar" ZTARGET="$z" \
        CARGO_TARGET_DIR="$target_root" \
        cargo check --workspace --all-targets --target "$t" 2>&1 || rc=$?
    if [[ $rc -eq 0 ]]; then
        echo "    PASS $t ($(( SECONDS - start ))s)"
    else
        echo "    FAIL $t (cargo exited $rc after $(( SECONDS - start ))s)"
        failed+=("$t")
    fi
done

if [[ ${#failed[@]} -ne 0 ]]; then
    echo "cross-check FAILED: ${failed[*]}"
    exit 1
fi
echo "cross-check PASS (${#selected[@]} targets: ${selected[*]})"
exit 0
