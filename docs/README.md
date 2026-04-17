# Helios Documentation

Helios is an experimental RISC-V 64-bit OS in Rust built around one thesis: **everything is a memory**. A single typed graph replaces the filesystem, the process table, the device tree, and the permission system. Processes ARE graph nodes. Capabilities ARE labeled edges. The kernel walks the graph; the graph IS the OS.

This directory collects design rationale, kernel internals, and the userspace story.

## Contents

### Design
Architectural decisions and their reasoning. Start here if you want to understand *why* Helios is shaped this way.

- [Philosophy: Everything Is a Memory](design/philosophy.md) — the core thesis
- [Capability Edges: Graph-Native Security](design/capability-edges.md) — how authority is expressed (M29+)
- [Userspace Tiers: Native, Ported, Kernel](design/userspace-tiers.md) — the three-tier model + the POSIX-compat tension

### Kernel
How the kernel is actually built. Source-of-truth is `src/`, but these docs explain the shape.

- *(Milestone-by-milestone docs coming — for now, see the main [README](../README.md) for the M1–M28 summary.)*

### Userspace
How to write programs for Helios.

- [Porting Software to Helios](userspace/porting.md) — three strategies (graph-native, POSIX shim, hybrid), out-of-tree apps, caps at spawn time
- [Rust on Helios: `std`, Targets, and the Porting Path](userspace/rust-std.md) — why `std` is hard, three strategies, first-native-app sketch

## Contributing to These Docs

Design discussions that matter belong here, not just in chat logs. When a decision is made with reasoning worth preserving:

1. Find the right file (or create one under `docs/design/` for new threads)
2. Capture the *tension* that was resolved, not just the conclusion
3. Link to the relevant milestone or commit
4. Date-stamp significant revisions

Docs should read like engineering letters — direct, opinionated, and honest about uncertainty.
