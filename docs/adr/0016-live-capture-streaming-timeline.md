# ADR-0016: Live capture streams into a bounded engine and renders per-redraw Timeline snapshots

> Status: Accepted
> Date: 2026-07-01

## Context

M11 adds `tcp-visr live -i <iface>`: capture TCP off a live Linux interface via libpcap
(ADR-0003) and drive the *same* engine and TUI the replay path uses. Live differs from replay
in three structural ways:

- **It is unbounded.** Replay parses a finite file up front; the engine produces complete
  per-connection series bounded by the capture-size policy ([ADR-0004](0004-seekable-timeseries-timeline.md),
  design §7), and the TUI browses the resulting **immutable** `Timeline`. A live capture runs
  indefinitely, so retained state must be bounded or it will OOM.
- **Time advances without packets.** A silent connection emits no segments, so idle/dead
  transitions and throughput decay-to-zero need `Tick` items to advance time (design §4.1).
- **The wire cannot be back-pressured.** If the engine falls behind libpcap, packets must be
  **dropped and counted**, never buffered unbounded or silently lost (design §7).

The existing engine already has two properties that make this feasible without a rewrite:
`Tracker::observe` already *accepts* `Item::Tick` (it is inert in replay, which never emits
one), and metric derivation (`MetricState`, sequence unwrap, SRTT EWMA) is already a per-segment
fold. What it lacks is bounded retention (its `max_samples` is a fail-fast *ceiling*, not an
evictor) and any live read path for the TUI.

The replay `Timeline` and the four detail renderers are extensively tested. The dominating design
constraint is therefore: **do not disturb the replay path's tested guarantees.**

## Decision

**1. Live capture runs in a background thread that owns the only clock read; the engine stays
pure.** The capture thread reads packets, hands raw frames to the shared `decode_frame`
(ADR-0003/0005 — one decoder, both faucets), stamps each `Segment` from libpcap's timestamp, and
**injects `Item::Tick(now)` on read-timeout** so idle/decay advance during silence. Ticks and
Segments therefore share one clock domain (libpcap's), and the pure engine consumes both as data
([ADR-0002](0002-pure-engine-io-boundary.md)). The engine gains no I/O and no `Instant::now()`.

**2. Retention is a bounded, time-horizon evictor selected by config; replay keeps fail-fast.**
`EngineConfig` gains a `RetentionPolicy`: replay uses `FailFast { max_samples }` (today's
behavior, unchanged), live uses `Evict { window, max_samples }`. Under `Evict`, per-connection
series are `VecDeque`s from which samples older than `now − window` are dropped on each new
sample and each Tick; `max_samples` remains a hard memory backstop that, when hit under `Evict`,
evicts the oldest rather than erroring. Each connection's **cumulative baseline** (state and byte
totals — the `Connection` view / latest `StateSample`) is retained for the connection's life,
independent of the display window, so the master list is always resolvable at "now" even after
detail samples age out.

**3. The TUI renders an immutable `Timeline` snapshot rebuilt per redraw.** The live loop, on each
frame (~20 Hz), builds a fresh immutable `Timeline` from the tracker's *bounded* retained samples
via a non-consuming `Tracker::snapshot()` and hands it to `App::retarget(timeline, horizon, now)`,
which swaps the underlying data while preserving UI state (selection, filter, sort, view tab,
freeze). Because retention bounds the sample count, the rebuild is O(bounded) per frame and the
entire `Timeline` query surface and all four detail renderers are reused **unchanged**. Replay
constructs its `Timeline` exactly as before.

**4. Live transport is follow/freeze, not the speed ladder.** In live mode the cursor follows
"now" (the latest Tick) by default; `space` freezes it in place; seek is permitted back to the
**eviction horizon** and clamped `T = max(T, horizon)`; resume re-attaches to now. The 0.1–10×
speed ladder is inert in live (you cannot fast-forward the future).

**5. Backpressure is a bounded channel with drop-and-count.** The capture thread pushes decoded
`Item`s through a bounded channel; a full channel makes the producer drop the packet and increment
an atomic counter. The drop count is surfaced in the status line and flags affected connections'
derived metrics as approximate (design §7). The unprivileged-open failure mode is a typed error
(privilege) and a startup silent-empty detector, never a silently idle display.

## Consequences

- The replay `Timeline`, `Transport` play/pause/speed, and the four detail projections are
  untouched; live is additive. The blast radius on tested replay code is limited to `App` gaining
  a `retarget` method and live-only fields, and `EngineConfig` gaining a retention policy whose
  `FailFast` arm reproduces current behavior.
- One decoder still serves both faucets, so the parity guarantee (ADR-0003/0005) is unaffected.
- Per-redraw snapshot rebuild trades a bounded amount of CPU/allocation for reusing the entire
  tested query+render path and keeping `Timeline` immutable. At the design's target scale
  (interactive diagnostics, bounded active-connection count, a ~120 s default window) this is
  negligible; a direct-read live view remains a post-v1 option behind the same seam if profiling
  ever demands it.
- Live capture is **not CI-testable** (needs `CAP_NET_RAW` and a real interface). The faucet is
  gated behind the existing `live` Cargo feature, so CI still *compiles and clippy-lints* it
  (`--all-features`, `--features live`); its behavior is covered by unit tests on the pure,
  hardware-independent pieces (retention/eviction, Tick decay/idle, clamp arithmetic, drop
  counting against a stalled consumer, silent-empty detection logic, snapshot equivalence) plus a
  documented local hardware run.
- Live kernel enrichment (real cwnd/srtt via `sock_diag`, `/proc` attribution) stays out of M11;
  it is M12 and joins through the existing empty overlay seams.

## Considered & rejected

- **Generalize `Timeline` into one append-and-evict structure used by both replay and live.**
  Rejected: it forces a fail-fast ceiling and a time-horizon evictor, and a fixed vs. growing
  `event_times`/interval index, into a single type — high blast radius on the most-tested replay
  code for no user-visible gain. Isolating live in a separate bounded tracker + a per-redraw
  snapshot keeps the immutable `Timeline` invariant intact.
- **Rebuild an immutable snapshot from an *unbounded* live tracker.** Rejected: without in-engine
  eviction the tracker grows without bound and the rebuild cost grows with capture duration; the
  bound has to live where the samples live.
- **Count-based retention only (no time window).** Rejected: the design frames the horizon and the
  pause clamp in *time* ("cursor clamped to the eviction horizon"); a pure count boundary does not
  map cleanly onto a wall-clock window. Count survives only as the `max_samples` memory backstop.
- **Engine reads the clock / injects its own Ticks.** Rejected: violates ADR-0002. The capture
  thread is the impure boundary and the natural owner of the timestamp source, keeping Tick and
  Segment timestamps in one domain.
- **Block the capture thread when the engine is behind (back-pressure the wire).** Rejected: the
  kernel's capture buffer would overflow anyway and you would lose packets without knowing;
  explicit drop-and-count is the honest failure mode (design §7).
