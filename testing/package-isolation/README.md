# package-isolation

Checks that a Debian package built by
`packaging/debian/build-deb-container.sh` is built from its own tree,
whatever was built before it or beside it in the shared target volume.

## What is here

- `test.sh`: the test. Run it from any checkout:
  `testing/package-isolation/test.sh [--script-dir DIR]`. It takes about ten
  minutes of full CPU and needs docker.
- The run's two scratch trees and their packages go in
  `target/package-isolation.XXXXXX` at the checkout's root, made with
  `shared_tmpdir` so they can be bind-mount sources, and removed when the test
  exits. They stay out of `testing/` because the static checks that sweep it
  would read the scratch trees' scripts as the repository's own.

## Why it exists

Every checkout on a host builds its package in one target volume,
`fips-deb-target-<image>`, and every tree is mounted at the same `/src` in the
container. Cargo judges a workspace crate by file modification times, so
without the protections in `packaging/debian/build-in-container.sh` a tree
whose files are older than another tree's build reuses that build. This test
builds two marked trees in each order and concurrently, and checks which
tree's code each package holds.

## How it decides

Tree 1 is a `git clone -s` of the checkout's HEAD; tree 2 is a
`git worktree add` of tree 1, so its root crate is recompiled on every build,
as in a CI worktree. Each tree marks itself before any build: `fips --version`
gains `isolation: root-N core-N`, with `core-N` taken from a constant added to
the `fips-core` member, or `-` on a tree without one.

Two verdicts are reported separately for each package:

- **marker**: the package's isolation line is its own tree's;
- **revision**: the package's `(rev ...)` is its tree's commit and dirty flag.

The marker verdict is the only one that tells the two trees apart. Both are at
one commit and both are dirty, so their revisions are identical. Do not drop
the marks as redundant with the revision check.

## Rules for changing it

- Each scenario uses a fresh target volume, so a scenario's result is its own
  and not left over from an earlier one. The registry volume is the run's own;
  never point the test at the host's `fips-deb-*` volumes.
- An older `build-deb-container.sh` that cannot take
  `FIPS_DEB_TARGET_VOLUME` is patched to read it. If the patch does not apply,
  the test refuses to run rather than build in the host's volumes.
- The marks must stay in a part of `--version` that is compiled whether or
  not a git revision is embedded: an older script embeds none for a worktree.
- Volumes carry the CI label and a run label equal to the name prefix, so
  `testing/ci-cleanup.sh` reaps them after a killed run.
- It is not in `ci-local.sh`'s default set or on GitHub, which is a recorded
  coverage gap: between runs of this test, isolation rests on the checks every
  package build makes.
