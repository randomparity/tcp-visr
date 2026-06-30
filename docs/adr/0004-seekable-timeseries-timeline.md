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
The timeline is a **cursor** over these series. Seeking is a binary search to `T`
(O(log n)); playback speed only changes how fast the cursor advances. Rendering reads
precomputed samples and never re-parses.

- **Replay**: the whole capture is parsed once into complete series.
- **Live**: the same series are maintained incrementally in a bounded ring buffer
  (configurable retention). Pause freezes the cursor while samples keep appending;
  scroll-back is bounded by the retention window.

## Consequences

- Seek and speed are cheap and constant-feeling: 0.1× and 10× cost the same; scrubbing is
  a binary search, not a replay.
- Render performance is decoupled from parse/capture performance.
- Cost: memory scales with retained samples. Replay holds the full series in memory (a
  later enhancement can index/stream very large captures); live is bounded by the
  retention window.
- Dense windows may need per-view downsampling to fit terminal resolution; the series
  granularity is the source, downsampling is a render-time concern.

## Alternatives considered

- **Re-derive state from start on each seek** — rejected: O(n) per scrub frame, render
  latency coupled to parse cost.
- **Periodic state snapshots + replay-from-nearest-snapshot** — rejected for v1: more
  complex than a flat series and unnecessary at expected capture sizes; revisit only if
  full-series memory becomes a problem for very large replays.
