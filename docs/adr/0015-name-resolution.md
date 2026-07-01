# ADR-0015: Name resolution — capture-DNS host labels via a side name table, the pure engine untouched, and the deferred live-resolver seam (M10)

> Status: Accepted
> Date: 2026-07-01

## Context

Design §2/§6 list **host (DNS) labels** as a v1 goal: the master list and detail pane should
show a peer as `github.com:443`, not only `140.82.121.3:443`. §10.M10 (issue #13) is the milestone
that adds them; its Definition of Done is **"capture-DNS (IP→name from DNS packets) + live
reverse-DNS with caching"**. Today the only label is the static port→service name
(`tcpvisr-tui/service.rs`, M4); peers render as raw `ip:port`.

Two facts constrain the design:

1. **Live capture (M11) does not exist yet.** The `live` subcommand returns "not implemented
   yet"; there is no wire faucet, no `Tick` injection, and no place to run or cache reverse-DNS
   lookups. The "live reverse-DNS with caching" half of the DoD has nothing to attach to and
   cannot be tested end-to-end in the replay-only v0.1 tree.
2. **The ingest decoder is TCP-only.** `decode_frame` (design §3.2) produces a `Segment` or a
   counted `SkipReason`; UDP — including DNS on port 53 — is `Skipped(NonTcp)`. DNS is the one L7
   protocol design §2 *does* want decoded (host labels are a goal), distinct from the excluded
   "deep L7 decode (TLS SNI, HTTP)". Capture-DNS therefore means decoding one L7 protocol from
   **untrusted** capture bytes.

A third fact shapes *where* the data lives: **DNS names are host-scoped, not connection-scoped.**
An `IP→name` mapping applies to every connection to that IP; the `tcpvisr-engine` state machine is
strictly **per-connection and pure** (ADR-0002), and its per-connection series feed the frozen M3
metric oracle. Threading host names through the engine would (a) not fit its per-connection shape,
(b) risk perturbing the oracle, and (c) give names an as-of-`T` semantics (a label that appears
mid-scrub) that is worse UX than a stable per-capture label.

This ADR decides, for the **capture-DNS** half delivered now:

1. Where DNS is parsed and how it reaches the rest of the system without touching the engine.
2. The `NameObservation` / `NameTable` / `HostName` types and their latest-wins semantics.
3. The faucet plumbing (a second sink; `ReplayParse` carries names; parity extended).
4. Hostile-input handling and the DNS-parser dependency.
5. How the TUI renders host labels (rows, detail title, filter) and surfaces a resolved-name count.
6. Why the **live reverse-DNS + caching** half is deferred to the live milestones, and how the
   `NameTable` is the seam it plugs into — with no phantom/unused code added now.

## Decision

### 1. DNS is parsed in the shared decoder; a third `DecodeOutcome` variant carries names

Capture-DNS is decoded in the **same** `decode_frame` both faucets call (design §3.2 invariant:
one decoder, never two). `DecodeOutcome` gains a third variant:

```rust
pub enum DecodeOutcome {
    Decoded(Segment),
    Names(Vec<NameObservation>),   // A/AAAA answers from a DNS response (UDP src port 53)
    Skipped(SkipReason),
}
```

- After the IP header is sliced, a **UDP** transport whose **source port is 53** (a response:
  server→client) has its payload parsed as a DNS message. Each **A** / **AAAA** answer yields one
  `NameObservation { ts, ip, name }` — `ip` from the record's address, `name` the record's owner
  name (for a CNAME chain this is the canonical name that actually maps to the address, which is
  the correct label). Non-address answer types (CNAME/PTR/TXT/…) are ignored for M10.
- A UDP/53 packet that parses but has **no A/AAAA answers** (a query, or an answer-less response),
  or that **fails** to parse, returns `Skipped(NonTcp)` — exactly as any non-TCP packet does today.
  DNS parse failure is never `Malformed` (that reason is about IP/TCP structure) and never fatal
  (design §7: per-packet problems are skipped-and-counted).
- Consequence for skip counts: a DNS response that *does* yield names moves **out of** the
  `non_tcp` skip tally (it is used, not skipped). Non-DNS UDP and answer-less/failed DNS still
  count `non_tcp`. This is more honest (those packets were decoded, not dropped) and is covered by
  new fixtures; existing TCP-only fixtures contain no DNS, so their skip counts are unchanged.

Because both faucets route through this one decoder, they stay byte-for-byte in lock-step on names
as they already are on segments (design §3.2), and the parity test is extended to assert it.

### 2. `NameObservation`, `HostName`, and a latest-wins `NameTable` in `tcpvisr-core`

The types are pure shared types, so they live in `tcpvisr-core` (which `tcpvisr-tui` and the bin
depend on; `tcpvisr-engine` does **not** gain them and is untouched):

```rust
pub struct HostName(String);            // validated, sanitized, ≤253 bytes, trailing dot stripped
pub struct NameObservation { pub ts: Nanos, pub ip: IpAddr, pub name: HostName }

pub struct NameTable { /* HashMap<IpAddr, (Nanos, HostName)> */ }
impl NameTable {
    pub fn observe(&mut self, obs: NameObservation);   // keep the max-ts name per IP (last wins on tie)
    pub fn resolve(&self, ip: IpAddr) -> Option<&HostName>;
    pub fn len(&self) -> usize;                          // distinct IPs resolved (observability)
}
```

- **Latest-wins, cursor-independent.** `resolve(ip)` returns the name with the greatest observation
  `ts` (ties broken by last-seen), i.e. a **static per-capture** label. It does *not* vary with the
  transport cursor: a peer keeps one stable label while scrubbing (deliberately unlike the as-of-`T`
  master-list state). Storing the `ts` and comparing makes this robust to non-monotonic capture
  time (§14) rather than relying on file order.
- **Memory is bounded by distinct IPs, not by DNS-packet count.** Observations are folded into the
  table as they arrive; no growing observation log is retained. A name-flood capture costs one
  entry per distinct answer IP.
- **`HostName` sanitizes at construction.** A DNS name is attacker-controlled and is rendered into
  a terminal. `HostName::new` strips the trailing root dot, drops bytes outside a
  conservative printable set (control characters, ESC, DEL — the terminal-escape-injection vector),
  and bounds length to the DNS maximum (253). Construction returns `None` for an empty/all-invalid
  name so it is dropped rather than rendered blank. This is the single choke point; nothing
  downstream re-validates.

The engine, `MetricSample`, and the `metrics` command's JSON are **entirely untouched**, so the
hand-derived M3 oracle goldens and the parity `Item` stream are undisturbed (the property ADR-0010
through 0014 all preserved).

### 3. Faucet plumbing: a name sink, `ReplayParse.names`, extended parity

- The streaming faucet keeps its item sink and gains an optional **name sink**. `parse_file_visit`
  (item-only) is retained as a thin wrapper over a new `parse_file_visit_named(path, item_sink,
  name_sink)`; the four existing item-only call sites (`parse`, `conns`, both `metrics` passes) are
  unchanged. Only `build_replay_app` switches to the `_named` form and folds each `NameObservation`
  into a `NameTable`. This keeps the "hold only the current frame" streaming property — the table,
  bounded by distinct IPs, is the only retained name state.
- The collecting APIs `parse_file` and `parse_file_libpcap` populate a new `ReplayParse.names:
  Vec<NameObservation>` (both faucets, via the shared decoder). The parity test asserts
  `pure.names == lib.names` alongside the existing `items`/`skipped` assertions.

### 4. Hostile-input handling and the parser dependency (`simple-dns`)

DNS name decompression (the `0xC0` back-pointer) is the classic parser DoS (pointer loops,
quadratic blow-up). Design §14 already resolves the analogous risk for Ethernet/IP/TCP by
preferring a **fuzz-tested slicing crate** (`etherparse`) over hand-rolled parsing. The same logic
applies here: rather than own the compression-pointer state machine, M10 parses with
**`simple-dns` (pinned `=0.11.3`)**:

- **Small and focused.** One transitive dependency (`bitflags`, already resolved at `2.13.0` in the
  tree via `etherparse`→`arrayvec`… ), MIT-licensed (already on the `deny.toml` allow-list). It
  parses a message and exposes `answers` with typed `RData::A`/`RData::AAAA`; it returns `Result`
  (no panics) and handles compression internally.
- **Bounded regardless.** M10 reads only A/AAAA answers and drops everything else; even a
  well-formed name flood is bounded by the per-IP `NameTable`. `simple-dns` is exercised on
  malformed and compression-bearing inputs in ingest tests, and the replay path stays libpcap-free
  (ADR-0003) — `simple-dns` is a default dependency, not gated on `live`.

Adding the crate is pinned exactly and audited by `cargo-deny`; no new SPDX id is required (MIT is
already allowed).

### 5. TUI renders the host label on rows, in the detail title, and in the filter

`App::new` gains a `NameTable` argument; `App` resolves each connection's peer once at construction
(names are static, so this is not per-frame work):

- **Master row.** `ConnRow`/`ConnMeta` gain `host: Option<HostName>`. The peer cell renders
  `host:port` when a name is known, else `ip:port` (design §6 shows `github.com:443`). `service`
  (port→name) is unchanged and independent.
- **Search.** The connection's `search_prefix` includes the host name, so `/` fuzzy-filter matches
  on hostnames (e.g. filtering `github` selects the connection whose peer resolved to
  `github.com`).
- **Detail title.** The shared `DETAIL <origin> → <responder>` block resolves the responder's host
  (design §6 shows `→ github.com:443`). Origins (typically private IPs with no DNS answer) resolve
  to `None` and keep their `ip:port`.
- **Observability.** The header title gains a resolved-name count next to the skip count
  (`… (47 connections, 12 names, skipped 3)`), so a capture with unparsed/absent DNS is visibly
  distinct from one with names (design §7: surface counts, no silent fallbacks).

No detail **graph** changes — M10 adds no view (the four-view switcher was finalized in M9).

### 6. The live reverse-DNS + caching half is deferred to the live milestones; `NameTable` is the seam

The "live reverse-DNS with caching" half is **deferred to M11/M12**, where the live faucet, the
`Tick` clock, and a place to run async lookups exist. This mirrors the M7/M8 precedent of landing a
replay-complete feature with a typed seam that live enrichment fills later (the empty-on-replay
kernel overlays).

The seam is the **`NameTable` itself as the single label-resolution point**, not a new abstraction:

- On replay it is populated from capture-DNS (this PR).
- On live it will be *additionally* populated by reverse-DNS lookups (`hickory-resolver`, design
  §9) with an in-process cache, folding results into the same `NameTable::observe` the TUI already
  reads. `resolve(ip)` is the one call site both paths flow through.

**No unused resolver trait, cache, or `hickory-resolver` dependency is added now.** An unused hook
with no caller is a phantom feature, which this project forbids; the real, exercised seam is the
`NameTable` API. The design roadmap's M10 DoD is split accordingly: capture-DNS lands here; live
reverse-DNS is recorded as belonging to the live milestones.

## Consequences

- Ingest gains a small L7 surface (DNS answer parsing) confined to one module and one new
  `DecodeOutcome` arm, behind the fuzz-tested `simple-dns`; the hostile-input surface grows by
  exactly "parse A/AAAA answers from UDP/53", bounded by the per-IP table and sanitized at the
  `HostName` boundary.
- `tcpvisr-core` gains `HostName`, `NameObservation`, and `NameTable`; `tcpvisr-engine` gains
  nothing and stays pure — the M3 oracle and the cross-faucet `Item` parity are byte-identical.
- The faucet keeps its streaming contract; the only retained name state is the per-IP `NameTable`.
  `ReplayParse` grows a `names` vector for the collecting/parity path.
- The TUI resolves labels once at `App::new` (static names, no per-frame cost); rows, the detail
  title, and the filter all key on the same resolved `Option<HostName>`.
- Skip counts shift only for captures that actually contain resolvable DNS (those packets leave the
  `non_tcp` tally); TCP-only fixtures are unaffected.
- The live half is a clean future addition: it feeds the existing `NameTable` and needs no rework
  of the replay path.

## Considered & rejected

- **Carry DNS as a new `Item::Dns` variant through the engine (single unified stream, ADR-0001).**
  Rejected: names are host-scoped, not connection dynamics; the per-connection pure engine is the
  wrong home, it would risk the frozen M3 oracle and the parity stream, and it would give labels an
  unwanted as-of-`T` flicker. A side table keeps the engine untouched. (This was the explicit
  fork; the side table was chosen.)
- **Hand-roll the DNS parser (zero new dependency).** Rejected: name decompression is the classic
  hostile-input DoS; design §14 already prefers a fuzz-tested slicing crate for the analogous
  packet layers. `simple-dns` is one tiny, MIT, already-in-tree-transitively dependency and owns
  the risky logic.
- **Use `hickory-proto` to match the `hickory-resolver` ecosystem chosen for the live half.**
  Rejected for the replay path: `hickory-proto` is a full DNS implementation with a large
  transitive footprint and license surface; the replay path must stay lean and libpcap-free
  (ADR-0003). Parsing capture answers and performing live reverse lookups are different concerns
  and need not share a crate; `hickory-resolver` still enters for the live half in M11/M12.
- **Add the live reverse-DNS resolver + cache (and `hickory-resolver`) now.** Rejected: no live
  faucet exists to drive it and nothing would exercise or test it — a phantom feature. Deferred to
  the live milestones behind the `NameTable` seam. (The user chose "capture-DNS now + deferred
  seam".)
- **Add a `ReverseResolver` trait / `NoopResolver` now as the "seam".** Rejected: an unused trait
  with no caller is itself a phantom feature. The `NameTable` API is the real seam; the live path
  will call `observe`/`resolve`, needing no new abstraction.
- **As-of-`T` (cursor-dependent) name resolution.** Rejected: a label that pops in mid-scrub is
  worse UX and needlessly couples names to the transport; latest-wins gives one stable label per
  peer. The observation `ts` is still stored, only to make latest-wins robust to non-monotonic
  capture time.
- **Retain the full `NameObservation` log in the app.** Rejected: unbounded in DNS-packet count for
  no benefit under latest-wins; fold into the per-IP table at ingest instead. The `Vec` on
  `ReplayParse` exists only for the collecting/parity API over bounded fixtures.
- **Skip sanitizing the name before display.** Rejected: DNS names are attacker-controlled and are
  written to a terminal; unsanitized bytes are a terminal-escape-injection vector. `HostName`
  sanitizes at its single constructor.
- **Parse TCP/53 DNS too.** Deferred: TCP-carried DNS is rare and its bytes are already consumed as
  TCP segments; M10 targets the common UDP case. Not required by the DoD.
