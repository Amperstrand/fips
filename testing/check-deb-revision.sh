#!/bin/bash
# Check that every binary in a Debian package carries the revision of the tree
# it was packaged from: the commit and dirty flag build.rs embeds, as printed by
# --version, against the tree's HEAD and state as git reads them here.
#
# Usage: check-deb-revision.sh <deb> <head> <state>
#   <head>   the full commit git read for the tree when the build started, or
#            empty when the tree is not a git checkout
#   <state>  the tree's state for the whole build, as git read it:
#              clean | dirty  check the commit and the dirty flag
#              changed        the state changed during the build; check the
#                             commit only
#              unreadable     git reads HEAD but not the tree's state; fails
#              none           not a git checkout; nothing is checked
#
# Run from the tree's root, in the build image: it needs dpkg-deb and git, and
# runs the binaries, which are linked against the image's C library.
# packaging/debian/build-in-container.sh runs it after every package build.
#
# A package whose binaries name another commit, or the wrong dirty flag, was
# built from another tree or from a stale build of this one. The usual cause is
# a cargo target directory shared between checkouts.
#
# What it cannot see: the revision is embedded by the root crate's build script,
# so a package whose root crate was compiled from this tree against another
# tree's copy of a workspace member passes. build-in-container.sh's clean and
# its check of cargo's report cover the members. Two trees at one commit that
# are both dirty also carry the same revision.
#
# Exit 0 = every binary carries the tree's revision, or there is no checkout to
# compare with. Exit 1 = a mismatch, or a tree that cannot be checked. Exit 2 =
# bad arguments.

set -euo pipefail

if [ $# -ne 3 ]; then
    echo "usage: check-deb-revision.sh <deb> <head> <state>" >&2
    exit 2
fi
DEB=$1
START_HEAD=$2
STATE=$3
BINARIES=(fips fipsctl fips-gateway fipstop)

fail() {
    echo "check-deb-revision: $*" >&2
    exit 1
}

case "$STATE" in
    clean|dirty|changed|unreadable|none) ;;
    *) echo "check-deb-revision: unknown state '$STATE'" >&2; exit 2 ;;
esac
[ -f "$DEB" ] || fail "no package at $DEB"

if ! head=$(git rev-parse --verify --quiet HEAD 2>/dev/null); then
    [ -z "$START_HEAD" ] || fail "git read HEAD $START_HEAD when the build started but cannot read it now"
    echo "check-deb-revision: not a git checkout; the revision is not checked" >&2
    exit 0
fi
if [ -n "$START_HEAD" ] && [ "$head" != "$START_HEAD" ]; then
    fail "the tree's HEAD moved during the build ($START_HEAD, then $head), so the package's version and its binaries may name different commits; build again once the tree has settled"
fi
short=$(git rev-parse --short=10 HEAD)

case "$STATE" in
    clean)      want="$short" ;;
    dirty)      want="$short-dirty" ;;
    changed)    want="" ;;
    unreadable) fail "git here reads HEAD $short but cannot read the tree's state, so the dirty flag cannot be checked; the git mounts are incomplete" ;;
    none)       fail "git here reads HEAD $short, but the build found no git checkout" ;;
esac

unpack=$(mktemp -d) # mount-tmpdir: inside the container, never mounted
trap 'rm -rf "$unpack"' EXIT
dpkg-deb -x "$DEB" "$unpack"

bad=0
for b in "${BINARIES[@]}"; do
    path="$unpack/usr/bin/$b"
    [ -x "$path" ] || fail "$(basename "$DEB") has no usr/bin/$b"
    out=$("$path" --version 2>/dev/null) || fail "usr/bin/$b --version failed"
    line=${out%%$'\n'*}
    got=$(printf '%s\n' "$line" | sed -nE 's/.*\(rev ([0-9a-f]+(-dirty)?)\)$/\1/p')
    if [ -z "$got" ]; then
        echo "check-deb-revision: usr/bin/$b carries no revision ('$line'); the tree is at $short" >&2
        bad=1
    elif [ -n "$want" ] && [ "$got" != "$want" ]; then
        echo "check-deb-revision: usr/bin/$b was built from $got, but the tree is at $want" >&2
        bad=1
    elif [ -z "$want" ] && [ "${got%-dirty}" != "$short" ]; then
        echo "check-deb-revision: usr/bin/$b was built from $got, but the tree is at $short" >&2
        bad=1
    fi
done
if [ "$bad" -ne 0 ]; then
    fail "$(basename "$DEB") does not hold binaries built from this tree"
fi
if [ -n "$want" ]; then
    echo "check-deb-revision: every binary carries $want" >&2
else
    echo "check-deb-revision: every binary carries $short (the dirty flag was not checked)" >&2
fi
