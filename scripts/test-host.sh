#!/usr/bin/env bash
# scripts/test-host.sh — run helios-std's pure-data unit tests on the
# developer's host triple.
#
# Why a separate script? The userspace workspace at `crates/`
# pins target=riscv64gc-unknown-none-elf and `[unstable] build-std`
# in `crates/.cargo/config.toml`, because every shipped user binary
# has to be a no_std blob the kernel can copy at 0x40000000. That
# config makes `cargo test` impossible from inside the workspace —
# you can't run riscv64 ELF tests on a host. This script overrides
# the offending env so cargo builds + runs tests against the host
# triple instead, with std available, and with build-std off.
#
# What's tested: pure-data parts of `helios-std` (Label/Errno
# encoding, NodeId display, edge wire-format decode, syscall-number
# constants). Inline-asm bodies are stubbed `unimplemented!()` on
# non-riscv64 targets — see `crates/helios-std/src/sys.rs`
# module-level docs.
#
# Run this before pushing any helios-std change to catch ABI-byte
# regressions in milliseconds. The QEMU integration runs are the
# end-to-end story; these are the unit-of-work check.
#
# Usage:
#   ./scripts/test-host.sh             # run all helios-std host tests
#   ./scripts/test-host.sh -- --nocapture
#                                      # pass-through args after `--`
#
# Exit codes: 0 = all tests passed, non-zero = test or build failure.

set -euo pipefail

# Repo root = parent of this script's directory. Lets the script work
# regardless of cwd.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"
RUSTC="${RUSTC:-$HOME/.cargo/bin/rustc}"

if [ ! -x "$CARGO" ]; then
    echo "test-host.sh: cargo not found at $CARGO" >&2
    echo "Set CARGO=path/to/cargo or install via rustup." >&2
    exit 2
fi

# Detect the host triple. `rustc -vV` includes a `host: ...` line.
HOST_TRIPLE="$("$RUSTC" -vV | sed -n 's/^host: //p')"
if [ -z "$HOST_TRIPLE" ]; then
    echo "test-host.sh: could not detect host triple from rustc" >&2
    exit 2
fi

# Environment overrides:
# - CARGO_BUILD_TARGET= unsets the workspace's riscv64 default.
# - CARGO_UNSTABLE_BUILD_STD= disables build-std for the host
#   compile (host has a real std, no need to rebuild it).
# - CARGO_TARGET_DIR isolated so host artifacts don't fight the
#   riscv64 ones in the userspace workspace's `target/`.
export CARGO_BUILD_TARGET=
export CARGO_UNSTABLE_BUILD_STD=
export CARGO_UNSTABLE_BUILD_STD_FEATURES=
export CARGO_TARGET_DIR="$REPO_ROOT/crates/helios-std/target-host"

# Run cargo from a directory *outside* the helios repo. Cargo
# searches for `.cargo/config.toml` walking up from CWD (not from
# the manifest path), so cd'ing to /tmp escapes the workspace's
# riscv64 + build-std config. We still pass `--manifest-path` to
# point cargo at the crate we want to test.
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT
cd "$WORK_DIR"

MANIFEST="$REPO_ROOT/crates/helios-std/Cargo.toml"

echo "test-host.sh: target   = $HOST_TRIPLE"
echo "test-host.sh: cargo    = $CARGO"
echo "test-host.sh: cwd      = $WORK_DIR (escapes workspace .cargo/config)"
echo "test-host.sh: manifest = $MANIFEST"
echo "test-host.sh: target dir = $CARGO_TARGET_DIR"
echo

# Pass through any caller args after `--` to the test runner. Default
# to nothing extra.
PASS_ARGS=()
if [ "$#" -gt 0 ]; then
    if [ "$1" = "--" ]; then
        shift
        PASS_ARGS=("$@")
    else
        PASS_ARGS=("$@")
    fi
fi

if [ "${#PASS_ARGS[@]}" -gt 0 ]; then
    "$CARGO" test --target "$HOST_TRIPLE" \
        --manifest-path "$MANIFEST" "${PASS_ARGS[@]}"
else
    "$CARGO" test --target "$HOST_TRIPLE" \
        --manifest-path "$MANIFEST"
fi
