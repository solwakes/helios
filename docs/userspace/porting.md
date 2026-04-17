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

## The Upstream Integration Question

A tempting plan for ecosystem growth is "get crates to add Helios support upstream." This is **not reliable** and Helios should not be designed around it.

Two constraints:

1. **No compelling reason for upstream maintainers.** A Linux-targeted crate has no motivation to add Helios to its CI matrix, validate against Helios ABI changes, or respond to Helios-specific bugs. Even a cooperative maintainer would reasonably reject Helios-specific changes on scope grounds.

2. **AI-authored contribution politics.** Many projects (especially large, culturally-visible ones) have explicit or de-facto anti-AI-patch policies. A clean, small Helios support patch can be rejected on authorship grounds regardless of quality. This is a contested and unstable norm; it may relax, it may tighten — either way, you can't build on it.

**Therefore:** Helios software planning must assume zero upstream cooperation. If upstream happens to accept a patch, great — a bonus. But the plan has to work without it.

### What This Changes

The ecosystem target isn't "Rust in general" or "C software in general." It's:

1. **Helios-native software, in-tree.** The core toolkit (`ls`, `cat`, shell, editor, init, package manager) is authored for Helios and maintained in the Helios repo or sibling repos. Self-contained.

2. **`no_std` Rust as the natural surface.** The `no_std` ecosystem (embedded Rust, RTOS crates, HAL-ish libraries) already assumes minimal or absent OS. Crates like `heapless`, `hashbrown` (with `alloc` feature), `postcard`, `embedded-hal`, `serde` (with `alloc`), most `nom`-based parsers work on Helios without patches. This is a richer ecosystem than people assume — modern Rust has been going `no_std`-first for years.

3. **Vendored crates for specific needs.** When we need a crate that has non-trivial upstream churn (a crypto lib, a specific parser), vendor a pinned version. `cargo vendor` or a Git submodule. Update on our schedule, not theirs. Not a fork — a snapshot.

4. **POSIX shim as escape hatch, not foundation.** For specific legacy software (`vim`, `sqlite`, a language runtime), ship a Helios-libc shim and link the binary against it. One-program granularity. Not a system-wide POSIX emulation.

### What Helios Is NOT

Helios is not "the OS that runs all of Rust." It is not trying to be the substrate for the mainstream software ecosystem. It's not competing with Linux for "daily driver" position.

**Helios is the graph-native OS for new software designed around typed edges and capabilities.** The curated in-tree toolkit plus `no_std` Rust covers a lot of interesting ground, and the escape hatch covers the rest. That's the scope. Accepting it is part of the thesis — not a compromise but a commitment.

### Distinctive vs Commodity

The principle "self-contained in-tree" is not purity — it's scope discipline. The test:

- **Distinctive** (in-tree): graph ops, cap checks, helios-specific APIs, init, shell, core toolkit, syscall bindings. These are *the thing that makes Helios Helios*. Writing them is not wheel-reinventing; it's writing the wheel for the first time.
- **Commodity** (vendored upstream): parsers (`nom`), allocators, data structures (`hashbrown`, `heapless`), cryptographic primitives (`sha2`, `ed25519-dalek`), serialization (`serde` + `postcard`). Well-solved problems where the correct answer is "use the existing crate."

Rewriting commodity code isn't virtuous, it's a waste of effort that also delays Helios's distinctive work.

### Selecting Crates: The AI-OK Filter

Before vendoring a commodity crate, screen for:

1. **Stated AI-contribution policy.** Check `CONTRIBUTING.md`, issue templates, public statements. Explicit welcoming (or at minimum neutrality) is a green flag.
2. **Maintainer dogfooding.** Maintainers who use AI tools themselves are structurally aligned with AI-assisted contribution.
3. **License clauses.** Some licenses (and license addenda) now include AI-restriction clauses. Respect them — don't vendor a crate whose license excludes AI-assisted use.
4. **Community vibe.** Public blow-ups about AI contributions are a yellow flag even if the official policy is silent.

If a crate passes all four: vendor + feel free to contribute fixes upstream.

If it partially passes: vendor + maintain privately, avoid upstreaming.

If it clearly fails: find an alternative crate.

This isn't adversarial — it's just picking ecosystem partners who are structurally compatible with how Helios is being built. Nothing is gained by provoking a fight over a crate that has alternatives. (Most commodity problems have several crates; the filter usually narrows to one or two candidates, not zero.)

### The Politics: A Note on Reality

A sizeable chunk of the open-source world has adopted a categorical anti-AI-contribution stance, including high-profile projects whose actual practice is more pragmatic. The "open slopware" list — targeting major OSes including Linux and FreeBSD — is broad enough to be diagnostic: it's a tribal marker, not a coherent contribution-quality policy.

Legitimate concern sits underneath the maximalist stance: maintainer burnout from low-quality AI-generated issues/PRs is real. The defensive response ("no AI ever") is understandable even where it's badly calibrated.

Helios's response is to route around the politics entirely. Don't argue, don't crusade, don't try to change minds. Pick partners who are already aligned; vendor quietly where needed; build on paths that don't depend on contested acceptance. The ecosystem we can actually work with is smaller than the universe of Rust crates, but it's still rich. Focus there.

### The Silver Lining

`no_std` Rust is a healthier ecosystem around AI-assisted contributions than the broader open-source world. It's smaller, more pragmatic, often solo-maintainer, and tends to care more about working code than authorial politics. If Helios engages with that part of the ecosystem deliberately — contributing code where welcomed, using existing crates where they work, not fighting the battles that can't be won — there's real room to grow.

The existential risk isn't the AI-contribution politics itself. It's pretending the politics don't exist and planning as though they'll resolve in our favor. They might not. Design for the constraint, and the rest follows.

---

*See also: [capability-edges.md](../design/capability-edges.md) for the cap model, [userspace-tiers.md](../design/userspace-tiers.md) for the three-tier software architecture, [rust-std.md](rust-std.md) for Rust-specific porting strategy.*

*Last reviewed: 2026-04-16 (M28 pre-impl sketch).*
