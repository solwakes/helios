#!/usr/bin/env python3
"""Boot Helios, connect to its TCP serial port, drive the shell, capture
output. More robust than the stdio-based harness: QEMU's `mon:stdio`
multiplexing has been flaky for us, so we use `-serial tcp:` instead.
"""
import os
import socket
import subprocess
import sys
import time

KERNEL = "target/riscv64gc-unknown-none-elf/release/helios"
DISK = "helios.img"
TRANSCRIPT = "screenshots/m31-m32-uart.txt"
SERIAL_PORT = 4444

QEMU_CMD = [
    "/opt/homebrew/bin/qemu-system-riscv64",
    "-machine", "virt",
    "-nographic",
    "-bios", "default",
    "-serial", f"tcp::{SERIAL_PORT},server=on,wait=off",
    "-monitor", "none",
    "-drive", f"file={DISK},format=raw,if=none,id=hd0",
    "-device", "virtio-blk-device,drive=hd0",
    "-global", "virtio-mmio.force-legacy=false",
    "-kernel", KERNEL,
]

COMMANDS = [
    (5.0, b"spawn hello\n"),     # M31: full hello demo
    (4.0, b"spawn ls\n"),        # M32: ls default (root)
    (4.0, b"spawn ls 16\n"),     # M32: ls a leaf node (no edges)
    (4.0, b"spawn cat 16\n"),    # M32: read demo-text (readable)
    (4.0, b"spawn cat 12\n"),    # M32: read /user (readable)
    (4.0, b"spawn cat 99999\n"), # M32: read missing -> EPERM
    (4.0, b"spawn who\n"),       # regression: M30 asm demo
]
BOOT_WAIT = 5.0
POST_DRAIN = 6.0


def main() -> int:
    script_dir = os.path.dirname(os.path.abspath(__file__))
    helios_root = os.path.abspath(os.path.join(script_dir, ".."))
    os.chdir(helios_root)

    # Spawn qemu with stdio inherited so kernel panics show in our stderr.
    proc = subprocess.Popen(
        QEMU_CMD,
        stdout=subprocess.DEVNULL,
    )
    print(f"[harness] qemu pid={proc.pid}")

    # Wait for the TCP server.
    sock = None
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline:
        try:
            sock = socket.create_connection(("127.0.0.1", SERIAL_PORT), timeout=1.0)
            break
        except (ConnectionRefusedError, OSError):
            time.sleep(0.25)
    if sock is None:
        print("[harness] never connected to qemu serial", file=sys.stderr)
        proc.kill()
        return 2
    sock.setblocking(False)
    print("[harness] connected")

    transcript_path = os.path.join(helios_root, TRANSCRIPT)
    os.makedirs(os.path.dirname(transcript_path), exist_ok=True)
    out = open(transcript_path, "wb")

    def drain_for(secs: float) -> None:
        """Read+record any available bytes for up to `secs` seconds."""
        end = time.monotonic() + secs
        while time.monotonic() < end:
            try:
                chunk = sock.recv(4096)
                if not chunk:
                    break
                sys.stdout.buffer.write(chunk)
                sys.stdout.buffer.flush()
                out.write(chunk)
                out.flush()
            except (BlockingIOError, OSError):
                time.sleep(0.05)

    try:
        drain_for(BOOT_WAIT)
        for delay, cmd in COMMANDS:
            print(f"[harness] -> {cmd!r}", flush=True)
            sock.sendall(cmd)
            drain_for(delay)
        drain_for(POST_DRAIN)
    finally:
        try:
            sock.close()
        except Exception:
            pass
        try:
            proc.terminate()
            proc.wait(timeout=3)
        except Exception:
            proc.kill()
        out.close()

    print(f"[harness] transcript -> {transcript_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
