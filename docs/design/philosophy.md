# Philosophy: Everything Is a Memory

*Status: Core thesis, stable since M6 (graph store introduced).*

## The Thesis

In Unix, *everything is a file*. A file is a sequence of bytes with a path. The abstraction is so thin and universal that sockets, devices, pipes, and even processes can wear its skin. This was radical in 1970 and has shaped operating systems ever since.

Helios swaps the primitive: **everything is a memory — a typed graph node**.

A node has:
- A unique id
- A type tag (system, device, task, channel, text, directory, code, …)
- An opaque content blob (bytes, whose meaning is typed)
- A set of outgoing edges, each with a label
- An optional reactive computation for its content

A process is a node. A file is a node. A device is a node. A TCP socket is a node. The HTTP server serving this graph as JSON is a node — and its own metrics appear as fields of that node. The graph describes itself.

## What This Gets You

**Unified addressability.** Every piece of system state has a stable id. A device, a task, a socket, a user's note — all addressable by the same mechanism, all queryable with the same tools.

**Typed edges.** An edge isn't just a reference, it has a label. `task_12 --(reads)--> file_42` is a fundamentally different relationship than `task_12 --(writes)--> file_42`. The graph encodes semantics, not just connectivity.

**Reactivity.** A node's content can be a template or a computed value. `"Uptime: $uptime seconds"` re-renders every read. This is baked into the storage layer — no separate notification system needed.

**Observability-by-default.** Because the graph models the system, the system is introspectable by definition. `GET /nodes` over HTTP returns the live state of the OS. Kernel telemetry is just another subgraph. You don't bolt on tracing; you *are* traceable.

**Single security model.** Authority is a graph property (see [capability-edges.md](capability-edges.md)). Revocation is edge removal. Delegation is edge copying. The graph is the authority matrix.

## What This Costs

**Loss of the byte-stream primitive.** Unix programs compose over stdin/stdout. Helios programs compose over subgraphs. The toolchain has to be rethought — `ls | grep | awk` doesn't map one-to-one. ([More on this.](userspace-tiers.md))

**Porting burden.** Existing software assumes POSIX. Running legacy binaries requires a compat shim per program (see [userspace-tiers.md](userspace-tiers.md)). The graph-native advantages don't carry over to ported software.

**Implementation complexity of primitives.** A POSIX kernel can get away with basic bookkeeping; a graph kernel needs a real store (allocator, persistence, query engine) as a *foundational* service. Helios has built one from scratch over M6–M10.

**Risk of collapse into "just a backing store".** If most software ends up going through the POSIX shim, the graph becomes an implementation detail behind a conventional OS surface. Every "better than Unix" OS has died this way. Helios survives only if the graph-native API is *genuinely better* for new work — not equal, better. ([More.](userspace-tiers.md))

## Lineage

Helios sits in a lineage of OSes that tried to change the substrate:

- **Plan 9 / Inferno / 9front** — *everything is a file, and the file namespace is distributed.* Got the "different substrate" insight right; got eaten by POSIX compat (ape). Strong influence.
- **Oberon / A2** — *everything is an object in a shared address space.* No user/kernel split; trusted compiler. The safety-via-language approach. Helios does safety-via-hardware (MMU + caps) instead.
- **TempleOS** — *everything is a HolyC program in a ring-0 playground.* The "no privilege separation" party. Genuinely fun, genuinely dangerous. Helios started here (M1–M28) but is moving toward privilege separation with caps in M29+.
- **seL4 / KeyKOS / EROS** — *everything is a capability.* Capability-based security done at the OS level, formally verified. Caps are a separate addressing system, layered on top of storage. In Helios, caps *are* the storage — edges are both connectivity and authority.
- **Smalltalk image / Lisp machines** — *everything is a live object in a persistent image.* Reactive, introspectable, mutable from within. Helios's reactive nodes and HTTP-mutable graph inherit this vibe.

Helios is specifically: **hardware-enforced capability security + graph-shaped storage as the same data structure, with reactivity + observability baked in**. The novelty isn't any single piece — it's the combination.

## Inviolate Principles

1. **The kernel only speaks graph ops.** The kernel API is nodes, edges, and caps. Never POSIX file descriptors, never path strings, never directory handles. Anything POSIX-flavored lives in userspace.
2. **Caps are expressed as edges.** Authority is not a separate table or handle space. It IS the graph. (See [capability-edges.md](capability-edges.md).)
3. **The graph describes the graph.** The OS's own state is accessible through the graph, not through special introspection APIs. `/stats`, `/tasks`, `/devices` are subgraphs, not special endpoints.
4. **Mistakes are data.** Failed operations are graph events. Capability violations leave traces. Nothing is silently swallowed.

These are load-bearing. Violating any of them is a signal that something has gone wrong in the design.

## Open Questions (as of M28)

- How do subgraphs get persisted selectively? (Whole-graph persistence is easy; selective persistence with transactional guarantees is not.)
- How does the graph scale beyond a single machine? (Distributed graphs — 9P-style mounts? Something capability-aware?)
- How does garbage collection work? (When a node has no incoming edges, is it collectable? Who decides?)
- What's the right shell language? (bash assumes stdin/stdout. A graph-native shell might look more like a query language with IDs flowing between commands.)

These are design questions for future milestones, not answered here.

---

*Last reviewed: 2026-04-16 (M28). Revisit when M29 user-mode lands and the kernel/userspace boundary gets its first hardware enforcement.*
