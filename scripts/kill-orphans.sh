#!/bin/sh
# kill-orphans.sh — kill stray qemu-system-riscv64 processes left over from
# earlier `make run` sessions. M31/M32 overnight produced enough zombies
# holding the disk lock that this finally graduated from a one-liner to a
# script. Safe to run idempotently — exits 0 if nothing matches.
#
# Usage:
#   scripts/kill-orphans.sh          # SIGTERM, then SIGKILL after 2s if needed
#   scripts/kill-orphans.sh -9       # straight SIGKILL
#   scripts/kill-orphans.sh -n       # dry-run: list what would be killed

set -eu

PATTERN='qemu-system-riscv64'

# Parse mode.
SIG=TERM
DRYRUN=0
case "${1:-}" in
    -9)  SIG=KILL ;;
    -n)  DRYRUN=1 ;;
    -h|--help)
        echo "usage: $0 [-9 | -n]"
        echo "  -9: SIGKILL immediately"
        echo "  -n: dry-run (list, don't kill)"
        exit 0
        ;;
    "")  ;;
    *)
        echo "unknown arg: $1" >&2
        exit 2
        ;;
esac

# pgrep -lf prints pid + command. -f matches against the full command line so
# we catch the qemu invocation regardless of how it was launched.
matches=$(pgrep -lf "$PATTERN" || true)
if [ -z "$matches" ]; then
    echo "no orphan $PATTERN processes."
    exit 0
fi

echo "$matches"

if [ "$DRYRUN" = 1 ]; then
    echo "(dry-run; no signals sent)"
    exit 0
fi

# First pass: TERM (or KILL if -9). Then verify; escalate after 2s if any
# survived TERM.
pids=$(printf '%s\n' "$matches" | awk '{print $1}')
echo "$pids" | xargs kill "-$SIG" 2>/dev/null || true

if [ "$SIG" = TERM ]; then
    sleep 2
    survivors=$(pgrep -lf "$PATTERN" || true)
    if [ -n "$survivors" ]; then
        echo "still alive after SIGTERM, escalating to SIGKILL:"
        echo "$survivors"
        echo "$survivors" | awk '{print $1}' | xargs kill -KILL 2>/dev/null || true
    fi
fi

echo "done."
