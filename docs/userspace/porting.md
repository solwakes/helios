# Porting Software to Helios

*Status: Design sketch, pre-implementation. M29 lands the syscall ABI that makes any of this concrete. Updated as ports happen.*

This document covers how existing software gets onto Helios, from graph-native reimplementations to POSIX-shim'd legacy code.

## Three Porting Strategies

### 1. Graph-Native Rewrite (Preferred for New Work)

Write the program *for* Helios. Link against `helios-std`. Speak in terms of nodes, edges, and caps. Examples in the initial toolkit: `ls`, `cat`, `cp`, `grep`, `wc`.

This gets you:
- Typed data (not just byte streams)
- Structured composition (pass node IDs between programs, not parsed text)
- Reactive content (subscribe to changes, not poll files)
- Observability (your program's state is graph-visible)
- Cap-aware by default (your manifest declares needs; the graph enforces)

Cost: you're writing a new program. For things that already exist well (`vim`, `sqlite`, `python`), this isn't pragmatic.

### 2. POSIX Shim Compilation

Link the existing POSIX-flavored code against `helios-libc`. The shim translates libc calls (`open`, `read`, `write`, `stat`, `exec`, `fork`, ...) to Helios syscalls at the library layer.

Three sub-strategies for the libc mapping:

**Flat filesystem.** The shim gives the program a familiar `/foo/bar/baz` hierarchy. Internally, each "directory" is a node with outgoing edges labeled by name; each "file" is a node with content. `open("/etc/hostname")` walks the graph from `/`, following name-labeled edges. `fstat` returns a synthesized stat struct with fake inode numbers.

**Path-to-id.** A more Helios-native twist: paths encode node IDs directly. `open("@42")` opens node 42 without any filesystem translation. Programs written to this convention bypass the fake-FS cost. Not POSIX-compliant but closer to the metal.

**Shim-per-program.** Each ported binary links its own libc with policies tuned to its needs. A text editor might get a flat filesystem shim; a network daemon might skip filesystem emulation entirely.

This gets you: most POSIX software running with minor configuration. It's how `nano`, `lua`, `sqlite`, etc. would ship.

Cost: the shim hides the graph from the program. Ported software doesn't benefit from typed edges, reactivity, or cap-awareness beyond what the libc translator gives it.

### 3. Hybrid: `helios-std` as a Rust Target

For the Rust ecosystem specifically, a third option: a new `riscv64-helios` rustc target where `std::fs`, `std::net`, `std::process` etc. are implemented on top of Helios syscalls directly. See [rust-std.md](rust-std.md) for details.

This gives Rust programs the ability to use standard `std` (for portability) *and* reach for `helios::*` (for native primitives) in the same codebase. Programs pick what they need per-call-site.

## Out-of-Tree Apps: The Capability Question

A program you didn't write needs caps at spawn time to do anything useful. How are they declared?

### Options

**A. Manifest file** — `caps.toml` packaged with the binary, declarative:
```toml
[caps]
"/devices/framebuffer" = ["write"]
"/devices/keyboard" = ["read"]
"/user/$USER/notes" = ["read", "write"]
```

**B. Runtime request** — the program calls `request_cap("/devices/framebuffer", "write")`, spawner decides. Flexible, but allows in-band escalation.

**C. Spawn-time CLI** — `spawn editor.hx --cap /user/notes:rw --cap /devices/framebuffer:write`. Most explicit, least convenient.

**D. Interactive approval** — OS prompts the user "this program wants to write to framebuffer, ok?" (macOS tccd pattern).

### Recommended: A + D + C as override

- Apps declare **minimum caps** in their manifest.
- On first launch, the user **reviews and approves** (D). The graph records the approval as edges with provenance (`granted_at`, `granted_by`).
- Power users can pre-approve via CLI (C) to skip the interactive flow.
- **Later revocation** = edge removal. The app loses access cleanly.

### Why Manifest + Enforcement Beats Android

Mobile OSes rely on *trusted* manifests — the OS trusts the app's declared permission list. A malicious manifest can lie or escalate at runtime via OS bugs.

Helios's manifests are enforced by the MMU. If the manifest says `write` on `/devices/framebuffer` and the app tries to write elsewhere, the page fault kills it. **A lying manifest fails at runtime**, hardware-enforced. This is structurally stronger than app-store manifest validation.

## Dependency Management

An out-of-tree app probably uses libraries. Options:

**Static linking.** App bundles its deps. Simple, heavy, self-contained. Recommended for M29 and for a while.

**Shared libraries as nodes.** A `.so`-equivalent is a graph node of type `library`. Apps have `exec` edges to the libraries they depend on. The kernel maps the library pages into the app's PT just like any other `exec` capability.

Dynamic linking needs:
- Stable library ABIs
- A way to resolve library references at load time
- Versioning (probably a `requires_version` field on the edge)

This is M32+ material. Static linking covers the gap until then.

## Verification and Trust

If an app is going to be handed caps, the user should be able to trust the app. A spectrum:

- **No verification:** anyone can ship anything. Helios caps + user approval + manifest-lying-fails-at-runtime covers the blast radius, but the user's approval decision is blind.
- **Signed apps:** the manifest is signed by the author; user can trust "this is the app I intended to install."
- **Reproducible builds:** someone else can verify the binary matches the source. `nixpkgs`-style.
- **Formal verification:** certain critical apps (`init`, shell, libc shim itself) can be proven cap-compliant.

Helios's baseline should be signed manifests + user approval, with a path to reproducible builds for the critical toolkit.

## Ecosystem Hygiene

This is the cultural layer. How do we keep out-of-tree apps from turning Helios into Unix-with-extra-steps?

1. **First-class native apps** — the toolkit (`ls`, `cat`, `cp`, shell, editor) must be native, not ported. Users see graph-native software first.
2. **Documentation privileges native** — tutorials and examples use `helios-std`. POSIX shim is covered, but as a compat layer, not the welcome mat.
3. **Manifests expose the cost** — an app that declares 50 caps should feel heavier than one that declares 3. Users learn to prefer narrow-cap software. App stores can rank on this.
4. **Cultural expectation**: porting a POSIX binary is *one* way to get software on Helios, but it's not the *default* way. Native is default.

---

*See also: [capability-edges.md](../design/capability-edges.md) for the cap model, [userspace-tiers.md](../design/userspace-tiers.md) for the three-tier software architecture, [rust-std.md](rust-std.md) for Rust-specific porting strategy.*

*Last reviewed: 2026-04-16 (M28 pre-impl sketch).*
