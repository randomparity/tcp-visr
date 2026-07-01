# ADR-0010: Timeline, transport controls, and as-of-T master list (M5)

> Status: Accepted
> Date: 2026-06-30

## Context

M4 ([ADR-0009](0009-tui-master-list-architecture.md)) delivered a static master list that
reflects **end-of-capture** state. M5 (design §5, §6, §10.M5) makes the replay view
seekable: play/pause, 0.1–10× speed, arbitrary seek, and step-by-event, with the master
list resolving **every connection active at cursor time `T`** and each row's state and
byte counts **as of `T`**. The overall approach — precompute time-indexed series once and
move a cursor over them, never re-parsing — is already fixed by
[ADR-0004](0004-seekable-timeseries-timeline.md); ADR-0009 anticipated that "M5 will
extend `App` with a cursor time and swap the static row set for an 'as of T' projection
without disturbing the render/key seams."

What ADR-0004/0009 leave open, and this ADR decides:

1. **Where the interval index and per-connection as-of-`T` resolution live**, and **what
   data feeds them**. The M3 `MetricSample` series carries in-flight bytes, throughput, and
   RTT — but not a time-indexed TCP `ConnState` or cumulative byte counts, which the master
   list's STATE and ↑↓BYTES columns need "as of `T`" (design §6).
2. **How playback advances time** without a clock read in the pure layer
   ([ADR-0002](0002-pure-engine-io-boundary.md)).

## Decision

### 1. A pure `Timeline` in the engine, fed by a dedicated as-of-`T` snapshot series

We add a `timeline` module to `tcpvisr-engine`:

- **`StateSample { t, state, bytes_o2r, bytes_r2o }`** — a per-segment lifecycle snapshot,
  distinct from `MetricSample`. The tracker already computes `state` and the cumulative
  byte counters per segment; recording one `StateSample` per processed segment is a small,
  local addition. It is gated by a new `EngineConfig.collect_state_timeline` flag and
  counts against the existing `max_samples` ceiling (fail-fast, design §7).
- **`Timeline`** owns the collected `(Connection, Vec<StateSample>)` set plus:
  - a **cross-connection interval index** over `[opened_at, effective_end]`, where a
    connection that reached `Closed`/`Reset` bounds at its `last_at` and any still-open
    connection is bounded at the capture end (the running "now", §4.1/§5) so it matches
    every `T ≥ opened_at`;
  - **`resolve_at(T)`** — the set of connections active at `T`, each with `(state,
    bytes_o2r, bytes_r2o)` from the last `StateSample ≤ T` (binary search per connection,
    last-value-carried-forward; ADR-0004);
  - a merged, de-duplicated **event-time index** for step-by-event (`next_event`/
    `prev_event`), and capture **`bounds()`**.

We keep `MetricSample` and the `metrics` command's JSON untouched: the master list does not
need in-flight/throughput/RTT (those feed the M6+ detail graphs), so the hand-derived M3
oracle goldens are not disturbed.

### 2. A pure `Transport` in the TUI; the clock is read only in `run()`

`tcpvisr-tui` gains a pure `Transport { start, end, cursor, speed, playing }` with
`toggle_play`, `faster`/`slower` (a fixed 0.1–10× ladder), `seek`, and `tick(dt)`.
`tick(dt)` advances the cursor by `speed · dt` and auto-pauses at the capture end. The
wall-clock delta `dt` is **injected as data** — the same "time is data" boundary the engine
uses for `Tick` (ADR-0002). The impure `run()` loop is the only code that reads
`Instant::now()`: it polls for input with a timeout, measures elapsed wall time between
frames, and feeds that delta to `tick`. Nothing in `tcpvisr-engine` or the pure TUI seams
reads a clock.

### 3. The master list becomes an as-of-`T` projection

`App` holds the `Timeline` and the `Transport`. Per frame it resolves the active rows at
the cursor; sorting, filtering, and `ConnId`-tracked selection (ADR-0009) apply to that
resolved set. The `render(frame, &App)` and `handle_key(&mut App, KeyEvent)` seams are
preserved and extended: the header shows the transport status (`▶/⏸ speed  t=cur / total`),
the footer advertises the transport keys, and navigation-mode keys gain
space (play/pause), `←`/`→` (seek), `+`/`-` (speed), and `,`/`.` (step). Filter-input mode
is unchanged — every printable key still appends to the query.

## Consequences

- Speed and seek stay O(1)/O(log m) per ADR-0004; the master list's honest worst case is
  the O(N_T·log m) random-seek-with-time-varying-sort that §5 already owns.
- The replay path switches from `SeriesCollection::None` to state-timeline collection, so
  it now holds a per-segment snapshot series in memory, bounded by `max_samples` with
  fail-fast (design §7, ADR-0004). The snapshot sample is small and separate from the
  (still uncollected for replay) metric series.
- `run()` changes from a blocking `event::read()` to `event::poll(timeout)` + `tick`, so
  playback advances between key presses. It remains the one untested impure seam
  (ADR-0009).
- STATE and ↑↓BYTES are now correct at `T`: an active row can no longer show a
  contradictory end-of-capture state.

## Considered & rejected

- **Add `state` + cumulative bytes to `MetricSample` and collect the full metric series
  for all connections.** Rejected: it churns the M3 metrics JSON schema and the
  hand-derived oracle goldens, collects heavier per-sample data than the master list needs,
  and conflates a *metric* sample with a *lifecycle* snapshot. A dedicated `StateSample`
  keeps both types honest.
- **Keep the master list at end-of-capture values, indexing only membership.** Rejected:
  an active row would show a contradictory final state (e.g. `Closed` while active at `T`),
  contradicting design §6.
- **Put the cursor/clock in the engine.** Rejected: violates ADR-0002 (pure engine, no
  clock reads). The wall-clock→cursor mapping lives only in the impure `run()` loop.
- **Linear scan for the active set instead of an interval index.** Rejected: the DoD names
  the cross-connection interval index, and ADR-0004 fixes the two-level cost model; the
  index keeps that model explicit and testable.
- **Re-derive state from the start on each seek.** Already rejected by ADR-0004 (O(n) per
  scrub frame).
