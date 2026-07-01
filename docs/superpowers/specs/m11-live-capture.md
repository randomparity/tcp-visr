# Spec: M11 — Live capture (libpcap)

**Milestone:** M11 (design §10.M11, §4.1, §5, §7) · **Issue:** #14 · **Release:** v0.2.0 (Live)
**ADR:** [ADR-0016 — Live capture streaming timeline](../../adr/0016-live-capture-streaming-timeline.md)
(builds on [ADR-0002](../../adr/0002-pure-engine-io-boundary.md),
[ADR-0003](../../adr/0003-libpcap-for-live-capture.md),
[ADR-0004](../../adr/0004-seekable-timeseries-timeline.md),
[ADR-0005](../../adr/0005-libpcap-file-faucet-at-m1.md),
[ADR-0010](../../adr/0010-timeline-transport-as-of-t.md))

## 1. Goal

Add `tcp-visr live -i <iface>`: capture TCP off a live Linux interface with libpcap and drive the
**same** pure engine and interactive TUI the replay path already uses. The live path streams
`Item`s from a background capture thread into a **bounded** tracker (retention with time-horizon
eviction), advances time during silence via injected `Tick`s, renders an immutable `Timeline`
snapshot per redraw, and offers a **follow/freeze** transport whose cursor clamps to the eviction
horizon. Capture failures (unprivileged open, silent-empty, engine-behind drops) are surfaced
explicitly, never as an idle or lossless display.

## 2. Scope

### In scope

- **Ingest — libpcap live faucet** (`tcpvisr-ingest`, behind the existing `live` feature):
  - `LiveOptions { iface, filter: Option<String>, snaplen, promisc }` (snaplen/promisc are
    internal knobs with sensible defaults — full-packet snaplen so DNS answers and TCP options
    decode; promiscuous off — not CLI-exposed in v0.2 per YAGNI).
  - `LiveCapture::open(&LiveOptions) -> Result<LiveCapture, LiveError>`: builds the device handle
    with snaplen, promiscuous mode, immediate mode, a bounded read timeout, and
    **nanosecond timestamp precision, falling back to microsecond** when the device cannot supply
    nano (ADR-0003); activates; applies the BPF `filter` if given; records the detected
    `LinkType`. A `LiveError::UnsupportedLinkType` is returned for a link type the shared decoder
    does not handle.
  - `list_interfaces() -> Result<Vec<InterfaceInfo>, LiveError>` enumerating capturable devices
    (name + description/addresses) for interface selection.
  - `LiveCapture::run(self, on_item: impl FnMut(LiveEvent), stop: &AtomicBool)`: the capture loop.
    Each successful read → shared `decode_frame(link, ts, data, orig_len)` → emits
    `LiveEvent::Item(Item::Segment)` or `LiveEvent::Name(NameObservation)`; a per-packet decode
    problem is counted in `SkipCounts` (design §7), never fatal. On read-**timeout** (no packet
    within the read timeout) it emits `LiveEvent::Tick(Item::Tick(now))` where `now` is the
    current libpcap-domain timestamp, so idle/decay advance during silence. The loop exits when
    `stop` is set or the handle ends.
  - Timestamps are normalized to `Nanos` from the capture start (matching the replay faucet's
    baseline-relative convention) so the engine sees a single monotonic-ish origin.
- **Engine — bounded retention + live Ticks** (`tcpvisr-engine`, pure, no feature gate):
  - `EngineConfig` gains `retention: RetentionPolicy` with `FailFast { max_samples }` (default;
    reproduces current replay behavior exactly) and `Evict { window: Nanos, max_samples: usize }`.
    The bare `max_samples` field is folded into the policy.
  - Under `Evict`, each connection's five series (`states`, `seq`, `inflight`, `rtt`,
    `throughput`) are `VecDeque`s; on every recorded sample and every `Tick`, front samples with
    `t < now − window` are dropped, and if the global retained count still exceeds `max_samples`
    the oldest retained sample is evicted (never an error). `states` always retains **at least the
    most recent sample** so the connection stays resolvable at "now".
  - Each connection's **cumulative baseline** (`Connection` view: `state`, `bytes_o2r`,
    `bytes_r2o`, `last_at`, `opened_at`, `segments`) is never evicted.
  - `observe(Item::Tick(t))` becomes active under `Evict`: advances the tracker's `now` to
    `max(now, t)`, evicts, and for each still-active connection emits a decay `ThroughputSample`
    (and, when collected, in-flight sample) at `t` when its trailing window is non-empty, so a
    silenced flow's rate visibly decays toward zero (design §4.1). Under `FailFast`, `Tick` stays
    inert (replay never emits one — no regression).
  - `Tracker::snapshot(&self) -> Timeline`: a **non-consuming** build of the current
    `Timeline` from retained series (mirrors `into_timeline`, but by reference and infallible under
    `Evict`). Still-open connections' `effective_end` extends to the tracker's `now`; a connection
    idle past `dead_after` is bounded at `last_at + dead_after` so it drops out of the active set
    after the idle horizon.
  - `Tracker::now()`, `Tracker::retention_horizon()` (= `now − window`, or the oldest retained
    sample) expose the live cursor domain.
- **TUI — live view seam + follow/freeze transport** (`tcpvisr-tui`):
  - `App::retarget(&mut self, timeline: Timeline, horizon: Nanos, now: Nanos)`: replaces the
    underlying `Timeline`, refreshes per-connection `metas` for connections new since the last
    frame (names resolved once via a `NameTable` passed at construction), updates the transport
    domain to `[horizon, now]`, and — when following — pins the cursor to `now`; otherwise clamps
    the frozen cursor into `[horizon, now]`. Selection, filter, sort, view tab, and detail-open
    state are preserved.
  - Live-only `App` fields: `follow: bool` (default `true`), `LiveStatus { dropped, approximate }`.
    `space` toggles follow/freeze (in live mode) instead of the replay play/pause; `←/→` seek and
    freeze; the speed ladder is inert in live.
  - Status line reports the **dropped-segment counter** and marks the capture (and affected
    connections') metrics **approximate** when drops have occurred.
  - `run_live(app, mut next_frame)` — the impure live event loop (sibling of `run`): init
    terminal, then loop { render; poll a key for the frame cadence; on key, `handle_key`; call
    `next_frame(&mut app)` to pull the latest snapshot and status and `retarget` } until quit;
    restore terminal (also on panic). `next_frame` is a closure supplied by the binary — the tui
    crate never links libpcap.
- **CLI** (`tcp-visr`):
  - `tcp-visr live -i <iface> [--filter <bpf>] [--retention-secs <S=120>] [--list-interfaces]`.
    `--list-interfaces` prints capturable devices and exits. Interface is required otherwise. The
    `max_samples` memory backstop uses an internal default (no flag; live never surfaces a ceiling
    error — it evicts).
  - `run_live` binary wiring: spawn the capture thread (`LiveCapture::run`) with a bounded channel
    (`sync_channel`) + a shared `AtomicBool` stop + atomic drop counter; the main thread owns the
    `Tracker` (Evict policy, all five series on) and a `NameTable`; `next_frame` drains the channel
    (`try_recv`) into the tracker + name table, counts channel-full drops, builds a `snapshot`, and
    `retarget`s the app. On Ctrl-C / `q`, set `stop`, join the thread, restore the terminal.
  - Without the `live` feature, `tcp-visr live` is a clear "built without live capture support
    (rebuild with --features live)" error, and `--help` still lists the subcommand.

### Out of scope (deferred / other milestones)

- **Kernel enrichment** (real cwnd/srtt via `sock_diag`, `/proc` process attribution) — M12; the
  in-flight/RTT overlay seams stay empty in live, exactly as in replay.
- **Live reverse-DNS with caching** — the M10 deferred half. M11 keeps capture-DNS names (parsed
  from observed UDP/53 answers, feeding the same `NameTable`); reverse-DNS lookups are M12.
- **AF_PACKET / eBPF faucets, static `live` binary** — future, behind the same faucet interface
  (ADR-0003).
- **Windows/macOS live capture** — Linux only for v0.2.

## 3. Architecture and data flow

```
capture thread (impure, owns the clock)          main thread (impure shell)
┌───────────────────────────────┐    bounded     ┌────────────────────────────────────┐
│ LiveCapture::run:             │    channel     │ run_live loop (per frame ~20 Hz):   │
│  read packet ──► decode_frame │──LiveEvent────►│  next_frame:                        │
│  timeout ──► Tick(now)        │   (drop+count  │   drain try_recv ► Tracker.observe  │
│  stop flag ◄──────────────────┼───on full)     │   Tracker.snapshot() ► Timeline     │
└───────────────────────────────┘                │   App.retarget(tl, horizon, now)    │
                                                  │  render(App); poll+handle a key     │
                                                  └────────────────────────────────────┘
                                                        │ pure App / Timeline / Transport
```

- The **engine remains pure**: it never reads a clock or does I/O; `Tick` timestamps come from the
  capture thread (ADR-0002/0016).
- **One decoder, both faucets** (ADR-0003/0005): the live loop calls the same `decode_frame`; the
  parity guarantee is unaffected.

## 4. Acceptance criteria

1. `tcp-visr live -i <iface>` opens the interface, applies snaplen/promisc/immediate/nano-precision
   (µs fallback), and (with `--filter`) installs the BPF filter; a bad filter or unknown link type
   fails fast with an actionable `LiveError`.
2. `tcp-visr live --list-interfaces` prints capturable devices and exits 0 without capturing.
3. An unprivileged open that libpcap rejects returns a typed `LiveError::Privilege` naming the
   `setcap cap_net_raw,cap_net_admin+eip` (or run-as-root) fix — **not** a silent-empty capture.
4. A handle that activates but delivers **zero** packets past a startup grace window surfaces a
   distinct "no packets — check privileges/interface/filter" status, not an idle-network display.
5. Segments and `Tick`s emitted by the faucet carry monotonic-origin `Nanos` timestamps in one
   clock domain; a read-timeout injects a `Tick(now)`.
6. Under `RetentionPolicy::Evict { window }`, samples older than `now − window` are evicted from
   every series on each new sample and each `Tick`; `states` always keeps ≥1 (the latest).
7. A connection's cumulative baseline (state, byte totals) survives eviction for the connection's
   life: after its detail samples age out, it still resolves at "now" in the master list.
8. `max_samples` under `Evict` evicts the oldest rather than erroring (no `SampleCeiling` in live);
   under `FailFast` (replay) behavior is byte-for-byte unchanged, including the `SampleCeiling`
   error and its message.
9. A `Tick` past a silent connection's activity drives its throughput sample toward zero (decay),
   and a connection idle past `dead_after` drops out of the snapshot's active set.
10. `Tracker::snapshot()` is non-consuming, infallible under `Evict`, and produces a `Timeline`
    whose `resolve_at`/detail-series answers match an equivalent `into_timeline` over the same
    retained data.
11. `App::retarget` preserves selection, filter query, sort field/dir, detail view tab, and
    detail-open across a frame; when following, the cursor equals `now`; when frozen, the cursor is
    clamped into `[horizon, now]`.
12. In live mode `space` toggles follow/freeze; `←/→` seek and set freeze; a seek never moves the
    cursor before the eviction horizon; resume re-pins to `now`.
13. The status line shows the dropped-segment count and marks metrics **approximate** once any drop
    (channel-full) has occurred; with zero drops it shows an exact/live indicator.
14. The bounded channel drops-and-counts when the consumer stalls: feeding faster than a stalled
    `next_frame` increments the drop counter and never blocks the producer or grows unbounded.
15. Building **without** `--features live`: `tcp-visr live` errors with a clear "built without live
    support" message; the whole default (replay) build and its tests are unchanged and libpcap-free.
16. `q`/Ctrl-C stops the capture thread (sets `stop`, joins) and restores the terminal, including
    on a render/IO error or panic.

## 5. Error handling (design §7)

- **Whole-capture failures** are `LiveError` (open/activate/filter/link-type/privilege) — fail
  fast with what failed, the interface, and the fix. **Per-packet** decode problems are counted in
  `SkipCounts`, never fatal.
- **Silent-empty privilege trap:** zero packets past the grace window → explicit status, per DoD.
- **Engine-behind:** channel-full → drop + atomic count; surfaced and flags metrics approximate.
  Replay's genuine flow control is unaffected (it never drops; ADR-0002).
- **Cursor clamp:** frozen or seeked cursor is clamped to `[horizon, now]`; the eviction horizon
  moves forward as the window slides, dragging a frozen cursor forward with it (never stale before
  data that no longer exists).
- **Non-monotonic capture time:** `now` advances by `max`, never backward; snapshot series are
  stable-sorted by `t` at `Timeline` construction, as today.

## 6. Testing

- **Engine unit tests** (pure, CI): retention eviction by window across all five series; the
  states-keeps-latest rule; baseline-survives-eviction; `max_samples` evict-vs-fail-fast per
  policy; `FailFast` regression (existing tests unchanged); Tick-driven throughput decay and
  idle-drop; `snapshot()` non-consuming + equivalence to `into_timeline`; horizon/now accessors.
- **TUI unit tests** (pure, CI): `retarget` preserves UI state and moves/clamps the cursor for
  follow vs freeze; live `space`/seek semantics and horizon clamp; status-line approximate flag on
  drop; `TestBackend` render of a live snapshot showing the follow/freeze + drop indicators.
- **Ingest unit tests** (CI, `live` feature — compile + hardware-independent logic only):
  timestamp normalization to monotonic-origin `Nanos`; the timeout→`Tick` decision; `SkipCounts`
  accounting on a decode failure; `LiveError` mapping for a privilege/link-type failure using the
  fakeable seam (the actual device open is not exercised in CI). The **parity** guarantee is
  already covered by the existing `parity.rs` over the file faucet; M11 adds no second decoder.
- **Backpressure test** (CI): a bounded channel with a stalled consumer increments the drop count
  and the producer never blocks (drive the channel logic directly, not a real NIC).
- **CLI tests** (CI): arg parsing for `live` (iface required, `--filter`, `--retention-secs`,
  `--list-interfaces`); the `live`-feature-off error path; `--help` lists `live`.
- **Local hardware run** (documented, not CI): `sudo tcp-visr live -i lo` (or `setcap`) against
  self-generated traffic (`curl`, `nc`), confirming capture, follow/freeze, seek-to-horizon, decay,
  and the privilege/silent-empty messages. The un-CI-testable DoD items are verified here and the
  limitation is stated in the PR.
- Test **behavior, not implementation**: assert retained/evicted samples, snapshot answers, cursor
  positions, rendered buffers, and drop counts — not how eviction or the channel drain is coded.
