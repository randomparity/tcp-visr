# ADR-0004: Precomputed time-indexed series + cursor for seek/speed

> Status: Accepted
> Date: 2026-06-30

## Context

v1 requires full transport controls: play/pause, variable speed 0.1–10×, arbitrary
seek/scrub, and step-by-packet — for both replay and (within a retention window) live.
Arbitrary seek means the UI must render every connection's state at any chosen time `T`,
cheaply and repeatedly as the user drags a scrubber.

A naïve approach (re-parse or re-run the state machine from the start to reach `T`) makes
seeking O(n) per scrub frame and couples render latency to parse cost — unworkable at
10× or while dragging.

## Decision

The engine will produce a **time-indexed `MetricSample` series per connection** up front.
The timeline is a **cursor** over these series. Rendering reads precomputed samples and never
re-parses; playback speed only changes how fast the cursor advances.

- **Sample cadence**: one sample per processed segment (per-event). The series time index is
  therefore irregular. "State as of `T`" is the **last sample at or before `T`**
  (last-value-carried-forward) — not interpolation. `throughput_bps` is a trailing
  sliding-window average of fixed length (default 1 s), **frozen into each sample at ingest
  time** because seeking never re-parses; its window cannot be changed at scrub time.
- **Cost is two-level.** Within one connection, seek is binary search, O(log m). Across
  connections, an interval index over `[opened_at, closed_at]` yields the set active at `T`;
  still-open connections (no `closed_at`) are indexed with an open right bound (`+∞` / running
  "now") so they match every `T ≥ opened_at` — this is the common live case and must be
  representable. Let `N_T` be connections active at `T`. A random seek that orders the master
  list by a *time-varying* column resolves all `N_T` — **O(N_T·log m)** per frame, the honest
  worst case. With a *static* sort key only the visible rows are resolved (lazy, display-capped).
  During monotonic playback each connection advances O(1) from its prior sample (O(N_T) per
  frame). "Constant-feeling" holds for bounded `N_T`, not for unbounded connection counts.
- **Replay**: the whole capture is parsed once into complete series, subject to a configurable
  capture-size ceiling (fail-fast when exceeded; design §7).
- **Live**: samples are maintained in a bounded ring buffer (configurable retention) for
  **scroll-back display**. This is distinct from each connection's **running baseline** (ISN,
  highest seq/ack), which is retained for the connection's life regardless of display
  retention — so evicting old display samples never corrupts in-flight derivation. Pause
  freezes the cursor; retention still applies during pause, and the frozen cursor is clamped
  to the eviction horizon so the buffer never grows unbounded during a long pause. The
  consequence is loss of evidence: pausing does **not** extend retention, so inspecting a
  moment longer than the retention window evicts it. The UI warns as the viewed range nears
  the eviction horizon; preserving an arbitrarily long window is what saving a pcap and
  replaying it is for.

## Consequences

- Speed is free: 0.1× and 10× cost the same. Random seek is O(A·log m) per frame (above),
  not a replay; render performance is decoupled from parse/capture performance.
- Cost: memory scales with retained samples. Replay holds the full series in memory, which is
  the direct cause of the large-capture memory risk (design §14). v1 bounds it with a
  configurable sample/size ceiling and fails fast when exceeded (design §7) rather than
  risking OOM; streaming/indexing of very large captures is an explicit **post-v1** item, not
  an open-ended "later." The "expected capture size" the per-event series targets is interactive
  diagnostic captures (≤ low-millions of segments), not multi-hour full-link taps.
- Live engine state has two lifetimes: bounded sample history (display) vs. per-connection
  running baseline (connection lifetime). Many simultaneously-open long-lived connections make
  the baseline set grow with connection count — bounded by the same active-connection cap.
- Dense windows may need per-view downsampling to fit terminal resolution; the series
  granularity is the source, downsampling is a render-time concern.

## Alternatives considered

- **Re-derive state from start on each seek** — rejected: O(n) per scrub frame, render
  latency coupled to parse cost.
- **Periodic state snapshots + replay-from-nearest-snapshot** — rejected for v1: more
  complex than a flat series and unnecessary at expected capture sizes; revisit only if
  full-series memory becomes a problem for very large replays.
