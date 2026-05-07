#!/usr/bin/env python3
"""Boot Helios in QEMU and exercise the post-M34 `gwrite` user
program. Smoke test for the gwrite implementation.

Drives the kernel shell through:
  - `nodes`                          — orient (find scratch id)
  - `spawn cat 21`                   — current scratch content
  - `spawn gwrite 21 hello world`    — happy path; writes 11 bytes
  - `spawn cat 21`                   — verify scratch was overwritten
  - `spawn gwrite 21 second message` — rewrite; verifies content-buf reuse
  - `spawn cat 21`                   — verify rewrite landed
  - `spawn gwrite 99999 nope`        — should ENOENT (target doesn't exist)
  - `spawn gwrite 0 broken`          — should reject pre-spawn (id must be > 0)

`scratch` is the M30 user-scratch text node; after boot init in src/user.rs
it lands at #21. If demo node count changes upstream, update the id here.

Captures the UART transcript to screenshots/gwrite-uart.txt.
"""
import os
import select
import subprocess
import sys
import time

KERNEL = "target/riscv64gc-unknown-none-elf/release/helios"
DISK = "helios.img"
TRANSCRIPT = "screenshots/gwrite-uart.txt"

QEMU_CMD = [
    "/opt/homebrew/bin/qemu-system-riscv64",
    "-machine", "virt",
    "-nographic",
    "-bios", "default",
    "-serial", "mon:stdio",
    "-drive", f"file={DISK},format=raw,if=none,id=hd0",
    "-device", "virtio-blk-device,drive=hd0",
    "-global", "virtio-mmio.force-legacy=false",
    "-kernel", KERNEL,
]

COMMANDS = [
    (5.0, "spawn cat 21\r\n"),
    (3.0, "spawn gwrite 21 hello world\r\n"),
    (3.0, "spawn cat 21\r\n"),
    (3.0, "spawn gwrite 21 second message\r\n"),
    (3.0, "spawn cat 21\r\n"),
    (3.0, "spawn gwrite 99999 nope\r\n"),
    (3.0, "spawn gwrite 0 broken\r\n"),
]
QUIT_AFTER_LAST_CMD_SECS = 5.0


def main() -> int:
    script_dir = os.path.dirname(os.path.abspath(__file__))
    helios_root = os.path.abspath(os.path.join(script_dir, ".."))
    os.chdir(helios_root)

    if not os.path.exists(KERNEL):
        print(f"error: kernel not found at {KERNEL}", file=sys.stderr)
        return 2
    if not os.path.exists(DISK):
        print(f"error: disk image not found at {DISK}", file=sys.stderr)
        return 2

    proc = subprocess.Popen(
        QEMU_CMD,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        bufsize=0,
    )

    transcript_path = os.path.join(helios_root, TRANSCRIPT)
    os.makedirs(os.path.dirname(transcript_path), exist_ok=True)
    out = open(transcript_path, "wb")

    start = time.monotonic()
    script_cursor = 0
    last_send = start
    last_cmd_sent_at = None

    try:
        while True:
            if script_cursor < len(COMMANDS):
                delay, cmd = COMMANDS[script_cursor]
                if (time.monotonic() - last_send) >= delay:
                    print(f"[harness] sending: {cmd!r}")
                    proc.stdin.write(cmd.encode())
                    proc.stdin.flush()
                    script_cursor += 1
                    last_send = time.monotonic()
                    if script_cursor == len(COMMANDS):
                        last_cmd_sent_at = last_send

            rlist, _, _ = select.select([proc.stdout], [], [], 0.2)
            if rlist:
                chunk = os.read(proc.stdout.fileno(), 4096)
                if not chunk:
                    break
                sys.stdout.buffer.write(chunk)
                sys.stdout.buffer.flush()
                out.write(chunk)
                out.flush()

            if (
                last_cmd_sent_at is not None
                and (time.monotonic() - last_cmd_sent_at) >= QUIT_AFTER_LAST_CMD_SECS
            ):
                break

            if (time.monotonic() - start) > 120.0:
                print("[harness] hard timeout after 120s; killing qemu")
                break

            if proc.poll() is not None:
                print(f"[harness] qemu exited early, rc={proc.returncode}")
                break
    finally:
        try:
            proc.stdin.write(b"\x01x")
            proc.stdin.flush()
        except Exception:
            pass
        try:
            proc.terminate()
            proc.wait(timeout=3)
        except Exception:
            proc.kill()
        out.close()

    print(f"\n[harness] transcript saved to {transcript_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
