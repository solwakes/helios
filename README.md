# Helios

an operating system where everything is a memory.

Helios is not a Unix clone. its fundamental abstraction is a persistent, typed knowledge graph rather than a filesystem tree. nodes have IDs, types, content, and named edges to other nodes. devices are nodes. processes are nodes. the system itself is the graph.

written in Rust, targeting RISC-V 64-bit, running on QEMU.

![graph visualization](screenshots/m12-with-tasks.png)

## what it has

- **RISC-V 64-bit** bare-metal kernel with OpenSBI
- **Sv39 virtual memory** with identity-mapped page tables
- **graphical framebuffer** via ramfb (1024x768 XRGB8888)
- **trap handling** with timer interrupts (Sstc extension)
- **interactive shell** over UART with 20+ commands
- **graph memory store** — nodes with types, content blobs, and labeled edges
- **live system nodes** — `cat` a device node to see its current state
- **visual graph rendering** — tree layout on the framebuffer with typed node colors and edge routing
- **cooperative multitasking** — tasks are graph nodes, context switch saves callee-saved registers
- **persistent storage** — graph serialized to virtio-blk disk, auto-loaded on boot
- **proper memory management** — linked-list allocator with coalescing (no more bump allocator leaks)

## building

requires: Rust nightly, QEMU with RISC-V support

```bash
# install dependencies (macOS)
brew install qemu
rustup target add riscv64gc-unknown-none-elf
rustup component add rust-src llvm-tools

# build
make build

# run (UART only)
make run

# run with framebuffer
make run-gui

# exit QEMU: Ctrl-A X
```

## shell commands

```
helios> help
System:     help, info, status, timer, mem, poke, clear, reboot, panic, fault
Graph:      graph, nodes, node, mknode, edge, set, cat, walk, find, rm, render
Tasks:      ps, spawn, kill
Storage:    save, load, disk
```

### exploring the graph

```
helios> walk 1
Node #1 "root" (system)
  --child--> #2 "system" (system)
  --child--> #3 "devices" (dir)
  --child--> #9 "tasks" (dir)

helios> cat 2
Helios v0.1.0
Architecture: RISC-V 64-bit (rv64gc)
Mode: Supervisor
Uptime: 12.5s

helios> mknode text notes
Created node #13 "notes" (text)

helios> set 13 everything is a memory
Set content of node #13 (22 bytes)

helios> save
Graph saved to disk (847 bytes, 2 sectors)
```

### running tasks

```
helios> spawn counter
Spawned task #1 "counter"

helios> spawn fibonacci
Spawned task #2 "fibonacci"

Task 'counter' iteration 1
Task 'fibonacci': fib(1) = 1
Task 'counter' iteration 2
Task 'fibonacci': fib(2) = 1
...
```

## screenshots

**boot splash (M2)**

![splash](screenshots/m2-splash.png)

**graph tree visualization (M12)**

![tree](screenshots/m12-tree.png)

**graph with tasks running (M11+M12)**

![tasks](screenshots/m12-with-tasks.png)

## architecture

```
src/
  main.rs              kernel entry point
  uart.rs              NS16550A UART driver
  trap.rs              trap handling + timer interrupts
  shell.rs             interactive command shell
  alloc_impl.rs        linked-list heap allocator
  framebuffer.rs       pixel rendering + bitmap font
  fwcfg.rs             QEMU fw_cfg driver
  ramfb.rs             ramfb framebuffer driver
  arch/riscv64/        boot assembly, linker script, CSR helpers
  mm/                  Sv39 page tables
  graph/               graph memory store, persistence, rendering
  task/                cooperative multitasking
  virtio/              VirtIO MMIO transport, block device driver
```

## the idea

traditional OSes organize data as files in a tree. Helios organizes data as nodes in a graph. the difference:

- a file lives at a path. a node lives at an ID with named edges.
- a directory contains files. a node has edges — `child`, `depends-on`, `version-of`, whatever you want.
- device files are a hack. device nodes are first-class — `cat` the uart0 node and you get live register state.
- processes are separate from files. in Helios, tasks are graph nodes alongside everything else.

the graph is the filesystem, the process table, and the device tree — unified.

## milestones

| # | what | commit |
|---|------|--------|
| M1 | RISC-V boot + UART | `ca3d9ae` |
| M2 | ramfb framebuffer | `0e9c915` |
| M3 | Sv39 page tables | `e5d72a4` |
| M4 | trap handling + timer | `a3f2450` |
| M5 | interactive shell | `ad1213a` |
| M6 | graph memory store | `f72aa1e` |
| M7 | graph visualization (cards) | `a9aa4d2` |
| M8 | live system nodes | `69e2703` |
| M9 | linked-list allocator | `46b6def` |
| M10 | virtio-blk persistence | `df1e3cc` |
| M11 | cooperative multitasking | `0af857d` |
| M12 | tree graph visualization | `c4d4bd2` |

## license

this is an experiment, not a product. do what you want with it.
