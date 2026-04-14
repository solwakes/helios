# Helios

A RISC-V operating system where everything is a memory.

Helios is not a Unix clone. Its fundamental abstraction is a persistent, interconnected, typed knowledge graph rather than files. This is the early boot stage — getting a kernel running on QEMU.

## Building

```bash
make build
```

## Running

UART-only (no graphics):
```bash
make run
```

With virtio-gpu display (M2):
```bash
make run-gui
```

Exit QEMU with `Ctrl-A X`.

## Status

- **M1**: Boots on QEMU virt machine, prints to UART via ns16550a
- **M2**: VirtIO GPU framebuffer, graphical "Helios" rendering (planned)
