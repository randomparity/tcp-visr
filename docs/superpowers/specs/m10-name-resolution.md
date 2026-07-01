# Spec: M10 — Name resolution (capture-DNS host labels)

**Milestone:** M10 (design §2, §6, §10.M10) · **Issue:** #13 · **Release:** v0.1.0 (Replay MVP)
**ADR:** [ADR-0015 — Name resolution: capture-DNS host labels via a side name table, the pure engine untouched, and the deferred live-resolver seam](../../adr/0015-name-resolution.md)
(builds on [ADR-0001](../../adr/0001-packet-derived-unified-model.md),
[ADR-0002](../../adr/0002-pure-engine-io-boundary.md),
[ADR-0003](../../adr/0003-libpcap-for-live-capture.md),
[ADR-0005](../../adr/0005-libpcap-file-faucet-at-m1.md),
[ADR-0008](../../adr/0008-usable-frame-headers-present.md),
[ADR-0009](../../adr/0009-tui-master-list-architecture.md))

## 1. Goal

Give each connection a **host (DNS) label** so the master list and detail pane show a peer as
`github.com:443` rather than only `140.82.121.3:443` (design §6). Names come from **capture-DNS**:
the A/AAAA answers in DNS response packets already present in the replayed capture. Labels are a
**static, per-capture** property (latest-wins), independent of the transport cursor. The **live
reverse-DNS with caching** half of the DoD is deferred to the live milestones (M11/M12), where a
live faucet and cache exist; the `NameTable` built here is the seam it will feed (ADR-0015 §6).

## 2. Scope

### In scope

- **DNS decode in the shared decoder** (`tcpvisr-ingest`). `decode_frame` gains a third outcome
  `DecodeOutcome::Names(Vec<NameObservation>)`. A **UDP** packet whose **source port is 53** (a
  response) has its payload parsed as a DNS message; each **A** and **AAAA** answer produces a
  `NameObservation { ts, ip, name }` (`ip` = the record's address, `name` = the record's owner
  name). Other answer types are ignored. A UDP/53 packet with no A/AAAA answers, or one that fails
  to parse, returns `Skipped(NonTcp)` (counted, never fatal — design §7). Parsing uses
  **`simple-dns` (pinned `=0.11.3`)** (ADR-0015 §4). Both faucets call this one decoder, so name
  extraction stays in lock-step (design §3.2) and the parity test asserts it.
- **Core types** (`tcpvisr-core`): a sanitized, bounded **`HostName`** newtype; a
  **`NameObservation { ts: Nanos, ip: IpAddr, name: HostName }`**; and a pure latest-wins
  **`NameTable`** with `observe(obs)`, `resolve(ip) -> Option<&HostName>`, `len()`, and `is_empty()`.
  Latest-wins keeps the name with the greatest observation `ts` per IP (ties → last-seen); a stable
  per-capture label, cursor-independent (ADR-0015 §2). `HostName::new(&str) -> Option<HostName>`
  strips a trailing root dot, drops bytes outside a conservative printable set (rejecting control /
  ESC / DEL — the terminal-escape vector), bounds length to 253, and returns `None` for an
  empty/all-invalid name.
- **Faucet plumbing** (`tcpvisr-ingest`): the streaming `parse_file_visit` is retained as an
  item-only wrapper over a new `parse_file_visit_named(path, item_sink, name_sink)`; the collecting
  `parse_file` and `parse_file_libpcap` populate a new `ReplayParse.names: Vec<NameObservation>`.
  The parity test also asserts `pure.names == lib.names`.
- **TUI host labels** (`tcpvisr-tui`): `App::new` takes a `NameTable`. Each connection's peer
  resolves once at construction. The master row's peer cell renders `host:port` when a name is
  known, else `ip:port`; the connection's fuzzy-search text includes the host name; the detail
  title `DETAIL <origin> → <responder>` renders the responder's host when known. `service`
  (port→name) is unchanged.
- **Observability** (`tcp-visr` bin): the header title gains a resolved-name count
  (`… (N connections, M names, skipped K)`), sourced from `NameTable::len()`.
- **CLI wiring**: `build_replay_app` folds observations into a `NameTable` via
  `parse_file_visit_named` and passes it to `App::new`. No new CLI flags.
- **Dependency**: add `simple-dns = "=0.11.3"` to `tcpvisr-ingest` (default, not gated on `live`);
  MIT, already on the `deny.toml` allow-list.
- **Fixtures**: a committed capture containing a TCP connection plus a UDP/53 DNS response mapping
  the peer IP to a host name, built deterministically from source (support module) and guarded by
  the `drift` test.

### Out of scope (deferred, do not build)

- **Live reverse-DNS + caching** and the `hickory-resolver` dependency — no live faucet exists to
  drive it; deferred to M11/M12 behind the `NameTable` seam (ADR-0015 §6). **No** resolver trait,
  cache, or stub is added now (an unused hook is a phantom feature).
- **Any change to `tcpvisr-engine`, `MetricSample`, the `metrics` JSON, or the `Item` stream** —
  names ride beside the Item stream, not within it; the M3 oracle and the parity `Item` stream stay
  byte-identical (ADR-0015 §2).
- **As-of-`T` (cursor-dependent) names** — labels are static latest-wins (ADR-0015 §2, rejected).
- **Non-address answer types** (CNAME/PTR/TXT) and **TCP/53 DNS** — not required by the DoD
  (ADR-0015 §1, deferred).
- **New detail graph views** — the four-view switcher was finalized in M9.
- **Reverse (PTR) inference from the capture** or resolving the local origin — origins are typically
  private IPs with no DNS answer and keep their `ip:port`.

## 3. User-facing behavior

### 3.1 Entry point

`tcp-visr replay <file>` is unchanged (invocation, non-TTY guard, `--max-samples`). During parse the
faucet now also feeds DNS `NameObservation`s into a `NameTable`; the built `App` renders host labels
from it. `parse`, `conns`, and `metrics` are unchanged except that a DNS response yielding names no
longer counts toward their `non_tcp` skip tally (it is decoded, not skipped; ADR-0015 §1).

### 3.2 Master list

```
┌ tcp-visr — capture.pcap  (3 connections, 2 names, skipped 1) ──[ ▶ 1.0x  t=0.000s / 5.000s ]┐
│ PEER                  SERVICE  STATE        ↑BYTES  ↓BYTES │ …
│▸github.com:443        https    ESTABLISHED    1234   34000 │ …
│ 10.0.0.9:22           ssh      ESTABLISHED     840    2100 │ …   (no DNS answer → raw ip:port)
│ cdn.example.net:443   https    TIME_WAIT         0     512 │ …
└─────────────────────────────────────────────────────────────────────────────────────────────┘
```

- A peer whose IP appears as an A/AAAA answer in the capture renders `host:port`; a peer with no
  such answer renders `ip:port` (IPv6 keeps its `[…]` bracketing).
- The `/` fuzzy filter matches the host name: typing `github` selects the connection whose peer
  resolved to `github.com`. Filtering on the raw IP still works for un-named peers.
- Sort by peer orders by the rendered label’s underlying `Endpoint` (unchanged ordering key —
  `(ip, port)`; the label is display only, so sort is stable regardless of names).

### 3.3 Detail title

With the detail pane open, the shared block reads `DETAIL <origin> → <responder>`; the responder
renders `host:port` when resolved (design §6: `→ github.com:443`). The origin renders its `ip:port`
(no resolution attempted beyond the same table; a private origin resolves to `None`). Which detail
**graph** is shown, and all graph behavior, are unchanged from M6–M9.

### 3.4 Resolution semantics

- **Static, latest-wins.** `resolve(ip)` returns the name from the answer with the greatest `ts`;
  the label does not change as the cursor scrubs. If two responses map the same IP to different
  names, the later-timestamped one wins (ties → last-seen).
- **One IP → one label.** Multiple connections to the same IP share the label.
- **Sanitized.** The rendered name never contains control/escape bytes and is ≤253 chars; an
  answer whose name sanitizes to empty is dropped (peer stays `ip:port`).
- **Absent DNS.** A capture with no DNS responses (or only queries) resolves nothing: every peer is
  `ip:port` and the title shows `0 names`. This is not an error.

## 4. Architecture

### Core (`tcpvisr-core`)

- New `name.rs`: `HostName(String)` with `new(&str) -> Option<Self>` (sanitize + bound) and
  `Display`/`AsRef<str>`; `NameObservation { ts, ip, name }`; `NameTable` wrapping
  `HashMap<IpAddr, (Nanos, HostName)>` with `observe`, `resolve`, `len`, `is_empty`, `Default`.
  Re-export all three from `lib.rs`. No I/O, no clock.

### Ingest (`tcpvisr-ingest`)

- New `dns.rs`: `parse_dns_answers(ts: Nanos, udp_payload: &[u8]) -> Vec<NameObservation>` using
  `simple_dns::Packet::parse`, mapping `RData::A`/`RData::AAAA` answers to observations via
  `HostName::new` (dropping names that fail sanitization). Returns empty on parse error or no
  address answers. Pure, panic-free.
- `decode.rs`: `DecodeOutcome` gains `Names(Vec<NameObservation>)`. Before the `TransportSlice::Tcp`
  check falls through to `classify_no_tcp`, a `TransportSlice::Udp` with `source_port() == 53` calls
  `parse_dns_answers`; non-empty → `Names(vec)`, empty → `Skipped(NonTcp)`. All existing TCP paths
  and skip classifications are unchanged.
- `lib.rs`: `ReplayParse` gains `names: Vec<NameObservation>`. `SkipCounts` is unchanged.
- `replay.rs`: add `parse_file_visit_named(path, item_sink, name_sink)`; `parse_file_visit`
  delegates with a no-op name sink. `process_packet` routes `Names(v)` to the name sink,
  `Decoded`/`Skipped` as today. `parse_file` collects `names` via a name sink into `ReplayParse`.
- `libpcap.rs`: the file faucet routes `Names(v)` into `ReplayParse.names` (shared decoder).
- `dns` module and the new outcome are default (not gated on `live`); `simple-dns` is a default dep.

### TUI (`tcpvisr-tui`)

- `app.rs`: `App::new(timeline, name_table, title)`. `ConnMeta`/`ConnRow` gain
  `host: Option<HostName>` resolved from the table at construction (peer IP). `search_prefix`
  includes `host`. `FocusConn` (or the detail-title path) exposes the responder host. Sorting/filter
  plumbing keys off the existing `Endpoint`/search text.
- `render.rs`: the peer cell renders `host:port` when `host` is `Some`, else the `Endpoint`
  `Display`; the detail title renders the responder host when known. Column widths accommodate a
  name (existing truncation rules apply). No graph changes.
- `run.rs`: unchanged (it drives the event loop over `App`).

### CLI (`tcp-visr`)

- `build_replay_app`: build a `NameTable`, parse via `parse_file_visit_named` folding
  `NameObservation`s into it, pass it to `App::new`, and include `name_table.len()` in the title.
  The `SampleCeiling` path is unchanged.

### Dependencies

- `tcpvisr-ingest` adds `simple-dns = "=0.11.3"`. `deny.toml` unchanged (MIT already allowed).
  Dependency direction is unchanged (TUI → engine, core; ingest → core; bin → all).

## 5. Success criteria (falsifiable)

1. **A/AAAA answers become observations.** `parse_dns_answers` fed a well-formed DNS **response**
   with one A answer `example.com → 93.184.216.34` returns one `NameObservation` with that IP and
   `HostName` "example.com"; an AAAA answer yields the v6 IP. (Ingest unit test.)
2. **Only responses; queries yield nothing.** A UDP/53 **query** (no answers) decodes to
   `Skipped(NonTcp)`, not `Names`. (Ingest/decode unit test.)
3. **Non-DNS UDP unchanged.** A UDP packet on a non-53 port still decodes to `Skipped(NonTcp)`;
   existing decode tests pass unchanged. (Ingest/decode unit test.)
4. **Malformed DNS is skipped, not fatal, not `Malformed`.** A UDP/53 packet with garbage payload
   decodes to `Skipped(NonTcp)` (parse failure → no names), and the faucet counts it under
   `non_tcp`; the parse does not panic. (Ingest/decode unit test.)
5. **Decoder produces `Names` for a DNS response.** `decode_frame` on an Ethernet/IPv4/UDP/53 frame
   carrying an A answer returns `DecodeOutcome::Names` with the expected observation. (Decode unit
   test.)
6. **Faucet routes names; skip tally excludes resolved DNS.** `parse_file` over a capture with one
   TCP SYN and one resolvable DNS response returns `items.len() == 1` (the SYN), `names.len() == 1`,
   and `skipped.non_tcp == 0` (the DNS packet is used, not skipped). A capture whose only UDP/53
   packet is a query returns `names.len() == 0` and `skipped.non_tcp == 1`. (Ingest integration
   test.)
7. **Cross-faucet parity includes names.** For a capture containing a DNS response, the pure-Rust
   and libpcap faucets produce equal `items`, `skipped`, **and** `names`. (Parity test, `live`
   feature.)
8. **`HostName` sanitizes and bounds.** `HostName::new` strips a trailing dot (`"a.com." →
   "a.com"`), drops control/ESC/DEL bytes (a name containing `\x1b[31m` renders without the escape),
   rejects (`None`) a name longer than 253 bytes, and returns `None` for `""` and for a name
   that sanitizes to empty. The rendered `Display` contains no byte `< 0x20` or `== 0x7f`.
   (Core unit test, incl. a property test over arbitrary bytes: output is always printable and
   ≤253.)
9. **`NameTable` is latest-wins per IP.** `observe`ing the same IP with name `a` at `ts=1` then `b`
   at `ts=2` resolves to `b`; observing `b` at `ts=2` then `a` at `ts=1` still resolves to `b`
   (max-ts wins regardless of insertion order); a tie at equal `ts` resolves to the last observed.
   `resolve` of an unknown IP is `None`. `len` counts distinct IPs. (Core unit test.)
10. **App renders host over IP for a resolved peer.** An `App` built with a `NameTable` mapping the
    peer IP to `github.com` yields a visible row whose peer renders `github.com:443`; an unresolved
    peer renders `ip:port`. IPv6 resolved renders `host:port` (no brackets on the host). (App/render
    unit test.)
11. **Filter matches the host name.** With a peer resolved to `github.com`, entering the filter
    `github` keeps that row; `zzz` drops it. A row with no name still filters on its `ip:port`.
    (App unit test.)
12. **Detail title shows the responder host.** With the detail open over a resolved responder, the
    rendered title contains `→ github.com:443`; over an unresolved responder it contains the
    `ip:port`. (Render/TestBackend test.)
13. **Header shows the name count.** The title built by `build_replay_app` over a fixture with one
    resolvable DNS response contains `1 names` (and the connection/skip counts); a fixture with no
    DNS contains `0 names`. (Bin integration test.)
14. **CLI wiring resolves the fixture peer.** `build_replay_app(<dns_fixture>)` returns an `App`
    whose resolved connection renders the fixture's host label on the peer, and whose title includes
    the name count. (Bin integration test.)
15. **Engine/oracle untouched.** The `metrics` command JSON for the existing `metrics_basic.pcap`
    is byte-identical to before M10 (no `MetricSample`/engine change); the existing oracle and
    render-closed snapshots pass unchanged. (Existing tests, unmodified.)
16. **Drift guard.** The committed DNS fixture matches its source builder (the `drift` test passes
    with the new fixture). (Drift test.)

## 6. Failure modes handled

- **No DNS in the capture** → `NameTable` empty; every peer `ip:port`, title `0 names`; not an
  error. (§3.4, criterion 13.)
- **DNS query only / answer-less response** → no observation; packet counted `non_tcp`. (§2,
  criteria 2, 6.)
- **Malformed / truncated DNS payload** → `parse_dns_answers` returns empty (no panic); packet
  counted `non_tcp`. (§4, criterion 4.)
- **Compression-pointer loop / name flood** → bounded by `simple-dns` (parse returns `Result`) and
  by the per-IP `NameTable`; only A/AAAA answers are read. (ADR-0015 §4.)
- **Hostile name bytes (terminal escapes)** → stripped at `HostName::new`; never rendered. (§2,
  criterion 8.)
- **Conflicting answers for one IP** → latest-`ts` wins deterministically. (§3.4, criterion 9.)
- **Non-monotonic capture time** → latest-wins compares stored `ts`, not file order. (ADR-0015 §2.)
- **Very large capture** → only the per-IP table is retained (streaming preserved); names add no
  per-segment memory and are not bounded by `--max-samples` (they are host-scoped, few). (§4.)
- **IPv6 peers** → resolved names render without the address brackets; unresolved keep `[addr]:port`.
  (§3.2, criterion 10.)

## 7. Testing

- **Ingest/DNS unit tests** (criteria 1–5) drive `parse_dns_answers` and `decode_frame` from
  hand-built (or `simple-dns`-built) DNS response/query/garbage payloads, asserting the emitted
  observations and the `Names`/`Skipped(NonTcp)` outcome. Assert behavior (the observations), not
  how the packet is parsed.
- **Ingest integration + parity** (criteria 6–7): `parse_file` over a committed DNS fixture asserts
  `items`/`names`/`skipped`; the parity test (feature `live`) asserts both faucets agree on `names`.
- **Core unit + property tests** (criteria 8–9): `HostName` sanitization/bound (incl. a `proptest`
  that arbitrary input yields printable, ≤253 output or `None`) and `NameTable` latest-wins.
- **App / render tests** (criteria 10–12): host-over-IP row rendering, host in the fuzzy filter,
  the detail title, from hand-built timelines + name tables, incl. a `TestBackend` assertion.
- **Bin integration** (criteria 13–14): `build_replay_app` over the DNS fixture resolves the peer
  label and reports the name count in the title.
- **Regression** (criteria 15–16): the existing `metrics`/oracle/render-closed tests pass unchanged;
  the `drift` test covers the new fixture.
- Test behavior, not implementation: assert the observations, the resolved labels, the rendered
  buffer, and the counts — not the DNS byte layout or the table internals.
