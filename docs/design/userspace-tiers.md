# Userspace Tiers: Native, Ported, Kernel

*Status: Design committed M28, first implementation starts M29.*

This document describes how software is structured in Helios — the three tiers, the tensions between them, and the cultural as well as technical forces that keep the system from collapsing into a different shape.

## The Three Tiers

### Tier 1: Kernel

The kernel only knows graph ops and capabilities. It never speaks POSIX. Its entire exposed ABI is:

- Create / read / update / delete nodes
- Add / remove / traverse edges (with capability checks)
- Cap-aware syscall dispatch
- MMU management (per-task page tables built from cap edges)
- Trap handling, scheduling, timers

There is no `open()`, no `read()`, no `write()` in the POSIX sense at this layer. There are only the graph primitives, cap-checked.

This is inviolate. The kernel stays pure or the thesis is dead.

### Tier 2: Helios-Native Userspace

Programs written *for* Helios, targeting the graph-native ABI directly. They:

- Link against `helios-std` (or eventually `std` once Rust's std learns Helios)
- Speak in terms of node IDs, typed content, cap edges
- Get the full expressive power of the graph — typed data, structured queries, reactive content, observability-by-default

Expected examples: `ls`, `cat`, `cp`, `grep`, `wc`, a future shell, a future editor, networked services, the core toolkit of the OS.

These are the programs that MAKE Helios Helios. They're the reason the thesis exists.

### Tier 3: POSIX-Ported Userspace

Programs written *for* POSIX, ported to Helios by linking against a compat library (`helios-libc`). They:

- See a POSIX surface — `fd`s, paths, `open`/`read`/`write`, a fake filesystem hierarchy
- Don't know about caps (the libc maps POSIX perms onto caps invisibly)
- Don't know about typed edges (the libc fakes inodes-and-dentries over node storage)
- Can be statically compiled with the shim linked in, so no kernel-level POSIX emulator is needed

Expected use case: porting `vim`, `nano`, `busybox`, anything where rewriting for graph-native would be a poor use of effort.

## Why Not Just POSIX-Kernel + Graph-Backing-Store?

The tempting alternative is to make the kernel speak POSIX and have the graph be a storage engine underneath. Every program would "just work" with minor porting.

**This is exactly how Plan 9 got eaten.** Plan 9 had its own native API (`mount`, `bind`, 9P namespaces) AND a POSIX compat layer called `ape`. Over time, most applications came in through ape because that's what developers knew. The Plan 9 native ecosystem atrophied. The project still exists (9front is actively developed) but culturally it became "weird Unix" rather than "fundamentally different OS."

BSD went through a similar pattern with Mach — Mach was a beautiful microkernel; practical systems bolted BSD-API monoliths on top; the microkernel contribution got diluted. macOS today is a descendant but the Mach-ness is largely invisible.

The pattern: **if the POSIX layer lives in the kernel, it becomes load-bearing for everything, and the native layer becomes vestigial.**

So Tier 3 lives strictly in userspace. The kernel never learns POSIX. A ported POSIX program carries its own compat library. Removing POSIX support from Helios is "don't link the shim" — never a kernel reconfiguration.

## Why Not Just a POSIX Emulator Process?

Another alternative: one big "POSIX VM" process that forks off a fake Unix kernel inside a sandbox, and POSIX binaries run inside *it*. Think WSL1 on Windows, or macOS's old Classic environment for OS 9 apps, or Wine.

This is better than in-kernel POSIX because it's contained. But it's also a whole extra kernel-like piece of software, heavy, slow, and shareable-state-between-apps. One bug in the POSIX VM affects all POSIX programs on the system.

The better pattern is **per-binary shims**, not per-system emulators. Each POSIX program links against `helios-libc` statically; the shim translates libc calls to Helios syscalls at the library layer. No extra process, no shared emulator state, no "POSIX kernel." The POSIX-ness lives inside each binary that wants it.

This is analogous to:
- **musl** vs **glibc** on Linux — different libc, same kernel
- **Cosmopolitan libc** — single binary that pretends POSIX on Windows/Mac/Linux
- **Emscripten** — POSIX-ish libc compiled to WASM, hosted by the browser's fetch/storage
- **Dinosaur ape binaries** on Plan 9 — each Unix binary was linked against ape's libc

The **kernel ABI is minimal** (graph + caps). **POSIX-ness is an implementation detail of specific binaries.** Different binaries can even use different shim policies — a strict POSIX one, a Plan-9-ish path-namespace one, a graph-aware "modern libc" one. The kernel doesn't care.

## The Existential Tension

The honest risk is: even with per-binary shims, if **most software** ends up going through the shim, the graph-native userspace (Tier 2) becomes vestigial. Helios de facto becomes "Unix with a different storage engine." The research contribution collapses.

Every "better than Unix" OS has died this way. The Unix ABI has 50+ years of polish, tools, documentation, training. Graph-native has... M28 worth of experimentation. The pull toward the familiar is enormous.

**What keeps Tier 2 alive is not purity. It's pleasantness.** The graph-native API has to be *genuinely better* for new work than the POSIX shim. Not equal, not "purer," but actually nicer to program against.

The claim: **it can be.** A lot of what POSIX programs do manually:
- Parse `/etc/foo` text files → typed, reactive config nodes in Helios
- Glob directories → structured edge traversal with typed filters
- Check permissions imperatively → authority expressed in the graph, enforced by MMU
- Discover plugins → typed edges to capability-granted child nodes
- Watch for config changes → reactive content subscription (native concept)
- Marshal data to pipe partners → pass node IDs, get typed structured data, no parsing

...is *primitive* in a typed-edge-cap-graph world. A task that would be a 200-line shell script in Unix might be a 20-line graph query in Helios.

That's the bet. The bet can fail. If the native API isn't palpably better, developers will reach for the familiar POSIX shim every time, and the ecosystem will calcify around Tier 3. Then Helios is done.

## Design Burden (Not Just Architecture)

This means the designers of Tier 2 carry a constant burden:

1. **Every common Unix idiom** should have a graph-native equivalent that's at least as pleasant. Not "equally hard" — *nicer*.
2. **The first impression** of writing a graph-native program should be "oh, this is cleaner than I expected." Not "I see why it's interesting but it's annoying to use."
3. **Documentation and tutorials** for new developers have to open with graph-native, not POSIX compat. POSIX compat should be a footnote, not the welcome mat.
4. **The standard toolkit (`ls`, `cat`, etc.)** must feel first-class and complete, not like placeholder substitutes.

This is work that can't be punted. It's the difference between Helios being interesting and Helios being a curiosity.

## A Middle Path: Multiple API Flavors in One stdlib

A further option, not exclusive with the above: the Helios stdlib could expose multiple API flavors over the same kernel ops:

- `helios::graph::*` — first-class, typed, cap-aware (the default)
- `helios::posix::*` — fd-based, path-based, POSIX-flavored (for compatibility-minded code)
- `helios::plan9::*` — 9P-style file-namespace (if that flavor is ever wanted)

All three talk to the same kernel. Developer picks what fits the task. New projects pick `helios::graph` because it's nicer. Legacy-adjacent projects use `helios::posix` as a bridge without abandoning Helios entirely.

This is analogous to Rust's `std::fs` + `std::os::unix::fs` duality — same underlying syscalls, different idioms for different mental models. It means a developer can *migrate gradually* from POSIX habits to graph-native, rather than having to cross a chasm.

## Cultural, Not Just Technical

The three-tier model is architectural, but keeping the balance healthy is *cultural*:

- Helios's docs should privilege native examples
- Helios's community (if it ever has one) should value native-first design
- POSIX compat should be presented as a pragmatic escape hatch for third-party software, not a primary development target
- Contributors should feel empowered to propose graph-native alternatives to existing POSIX idioms, not to "just port the standard thing"

This is the lesson from Plan 9's fate. The architecture didn't doom it; the gravity of familiarity did.

## Summary

- **Kernel (Tier 1):** Graph + caps only. Never POSIX. Inviolate.
- **Native userspace (Tier 2):** Graph-native tools. Where the interesting software lives. Must be *pleasantly* better than POSIX for new work.
- **POSIX userspace (Tier 3):** Per-binary shims. Bounded blast radius. Not allowed to leak into the kernel or become the default.

The thesis stays alive only if Tier 2 stays *preferable*, not just possible. That's a design responsibility, not just an architectural arrangement.

---

*Last reviewed: 2026-04-16 (M28). Revisit annually, or whenever Tier 3 starts feeling heavier than Tier 2.*
