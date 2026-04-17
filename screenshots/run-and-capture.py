#!/usr/bin/env python3
"""Boot Helios in QEMU, drive the shell with a scripted sequence of
commands, capture UART output, and save it to a transcript file.

Used for M31 hello-user integration testing.
"""
import os
import select
import subprocess
import sys
import time

KERNEL = "target/riscv64gc-unknown-none-elf/release/helios"
DISK = "helios.img"
TRANSCRIPT = "screenshots/m31-m32-uart.txt"
DONE_MARKER = "__M31_TEST_DONE__"

QEMU_CMD = [
    "/opt/homebrew/bin/qemu-system-riscv64",
    "-machine", "virt",
    "-nographic",
    "-bios", "default",
    "-serial", "mon:stdio",
    "-drive", f"file={DISK},format=raw,if=none,id=hd0",
    "-device", "virtio-blk-device,drive=hd0",
    # No networking — this is a headless test harness, and port 5555
    # is sometimes held by stray qemu instances from interactive runs.
    "-global", "virtio-mmio.force-legacy=false",
    "-kernel", KERNEL,
]

COMMANDS = [
    # Compact scripted exercise:
    #   - M31 hello (print + self_id + list_edges + EPERM via Result)
    #   - M32 ls (list the root node's 18 child edges)
    #   - M32 cat on a readable node (the M29 demo-text node, id=16)
    #   - M32 cat on a nonexistent node (id=99999) — the kernel refuses
    #     to add a read edge to a missing node, so cat gets EPERM from
    #     SYS_READ_NODE and returns exit=1 gracefully.
    (5.0, "spawn hello\r\n"),
    (5.0, "spawn ls\r\n"),
    (5.0, "spawn cat 16\r\n"),
    (5.0, "spawn cat 99999\r\n"),
]
# After the last scripted command, wait this long for output to drain
# before killing qemu.
QUIT_AFTER_LAST_CMD_SECS = 6.0


def main() -> int:
    script_dir = os.path.dirname(os.path.abspath(__file__))
    helios_root = os.path.abspath(os.path.join(script_dir, ".."))
    os.chdir(helios_root)

    if not os.path.exists(KERNEL):
        print(f"error: kernel not found at {KERNEL}; run `cargo build --release` first", file=sys.stderr)
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
            # Send next scripted command if due.
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

            # Poll qemu stdout.
            rlist, _, _ = select.select([proc.stdout], [], [], 0.2)
            if rlist:
                chunk = os.read(proc.stdout.fileno(), 4096)
                if not chunk:
                    print(f"[harness] qemu stdout EOF at {time.monotonic() - start:.1f}s")
                    break
                sys.stdout.buffer.write(chunk)
                sys.stdout.buffer.flush()
                out.write(chunk)
                out.flush()

            # Exit condition: last command drained.
            if (
                last_cmd_sent_at is not None
                and (time.monotonic() - last_cmd_sent_at) >= QUIT_AFTER_LAST_CMD_SECS
            ):
                print(f"[harness] drained {QUIT_AFTER_LAST_CMD_SECS}s after last cmd; killing qemu")
                break

            # Hard timeout guard.
            if (time.monotonic() - start) > 180.0:
                print("[harness] hard timeout after 180s; killing qemu")
                break

            if proc.poll() is not None:
                print(f"[harness] qemu exited early at {time.monotonic() - start:.1f}s, rc={proc.returncode}")
                break
    finally:
        try:
            # Ctrl-A X to quit QEMU monitor cleanly.
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
