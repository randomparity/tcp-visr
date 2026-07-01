# Spec: M5 — Timeline + transport controls (replay)

**Milestone:** M5 (design §5, §6, §10.M5) · **Issue:** #8 · **Release:** v0.1.0 (Replay MVP)
**ADR:** [ADR-0010 — Timeline, transport controls, and as-of-T master list](../../adr/0010-timeline-transport-as-of-t.md)
(builds on [ADR-0004](../../adr/0004-seekable-timeseries-timeline.md) and
[ADR-0009](../../adr/0009-tui-master-list-architecture.md))

## 1. Goal

Make the replay master list **seekable in time**. The user scrubs a replayed capture with a
cursor time `T`: play/pause, 0.1–10× speed, seek, and step-by-event. At every `T` the master
list shows exactly the connections **active at `T`** (via a cross-connection interval index),
and each row's **state** and **↑↓ byte counts** reflect their values **as of `T`**, not
end-of-capture.

## 2. Scope

### In scope

- A pure **`Timeline`** in `tcpvisr-engine` that resolves, for any time `T`, the set of
  connections active at `T` and each one's `(state, bytes_o2r, bytes_r2o)` as of `T`, plus
  the capture's time bounds and the ordered set of event times (for stepping).
- A per-segment **`StateSample { t, state, bytes_o2r, bytes_r2o }`** lifecycle snapshot
  series, collected by the tracker under a new `EngineConfig.collect_state_timeline` flag,
  counting against the existing `max_samples` ceiling.
- A pure **`Transport`** in `tcpvisr-tui` holding the cursor time, speed, and play/pause
  state, advanced by an injected wall-clock delta.
- The **master list resolved as of `T`**: `App` holds the `Timeline` + `Transport`; sort,
  `/` filter, and `ConnId` selection apply to the active-at-`T` row set.
- **Transport keys** in navigation mode: `space` play/pause, `←`/`→` seek, `+`/`-` speed,
  `,`/`.` step-by-event. Existing keys unchanged (`s`/`S` sort, `j`/`k`/`↑`/`↓` select,
  `/` filter, `q`/`Ctrl-C` quit).
- **Header** transport status (`▶`/`⏸`, speed, `t=cursor / total`) and an updated **footer**
  advertising the transport keys.
- `run()` becomes a poll + tick loop so playback advances between key presses.
- `run_replay` in the CLI collects the state timeline for all connections, builds the
  `Timeline`, and hands it to `App`; a `max_samples` overflow fails fast (design §7).

### Out of scope (deferred, do not build)

- **Detail panes** (Time/Sequence, in-flight, RTT, throughput graphs) and the metric series
  for all connections — M6–M9. `Enter` on a row still does nothing.
- **Live timeline** (ring buffer, pause/freeze, eviction) — M11. This is replay-only.
- **Process attribution / DNS names** — M10/M12.
- **Human-readable byte scaling / time formatting knobs** — byte counts stay raw integers
  (M4 §3.7); cursor times render as fixed-precision seconds (§3.6 below).

## 3. User-facing behavior

### 3.1 Entry point

`tcp-visr replay <file>` (unchanged invocation and non-TTY guard from M4 §3.1):

1. Streams `file` through the replay faucet into a `Tracker` configured to collect the state
   timeline (`collect_state_timeline = true`).
2. Builds a `Timeline` from the tracked connections and their `StateSample` series.
3. If collection exceeds `max_samples`, exits non-zero with the existing actionable
   `SampleCeiling` message (names the count, the limit, and `--max-samples`).
4. Requires an interactive terminal; a non-TTY stdout still exits non-zero with
   `replay requires an interactive terminal (stdout is not a tty)`.

### 3.2 Layout

```
┌ tcp-visr — capture.pcap  (47 connections, skipped 0) ──────[ ▶ 2.0x  t=12.480s / 38.200s ]┐
│ PEER                    SERVICE   STATE          ↑BYTES     ↓BYTES                          │
│▸140.82.121.3:443        https     ESTABLISHED      1234      34000                          │
│ 10.0.0.9:22             ssh       ESTABLISHED~       840       2100                         │
│ …                                                                                          │
├────────────────────────────────────────────────────────────────────────────────────────────┤
│ space play/pause  ←→ seek  +/- speed  ,/. step  / filter  s sort:peer▲  q quit               │
└────────────────────────────────────────────────────────────────────────────────────────────┘
```

- The bordered block keeps the M4 title on the left; the transport status is a
  **right-aligned** top-border segment: the play glyph (`▶` playing / `⏸` paused), the
  current speed (`2.0x`), and `t=cursor / total` in seconds.
- Rows are the connections active at `T`, resolved as of `T`. The STATE cell keeps the
  trailing `~` mid-stream marker (M4). ↑↓BYTES are the cumulative counts as of `T`.
- **No connections active at `T`** (e.g. the cursor precedes the first connection's open):
  the table area shows `no connections active at t=<seconds>s`; `q` still quits.
- Scrolling to keep the selection visible is unchanged from M4 §3.2.

### 3.3 The timeline (engine)

`Timeline` is pure (no I/O, no clock) and is built from a `Vec<(Connection, Vec<StateSample>)>`
so it is testable from hand-built vectors (testing convention).

- **Bounds.** `start` = the minimum `opened_at`; `end` = the maximum `last_at`. An empty
  capture has `start == end == 0`.
- **Interval index.** Each connection occupies `[opened_at, effective_end]` where
  `effective_end = last_at` if its final `state` is `Closed`/`Reset`, else `end` (still-open
  connections extend to the running "now"; ADR-0004). `active_at(T)` returns the connections
  whose interval contains `T` (`opened_at ≤ T ≤ effective_end`).
- **Per-connection resolution.** For an active connection, its `(state, bytes_o2r,
  bytes_r2o)` as of `T` is the last `StateSample` with `t ≤ T` (binary search,
  last-value-carried-forward). Because `opened_at` equals the first sample's `t`, an active
  connection always has such a sample.
- **`resolve_at(T)`** returns, for each active connection, its `ConnId` and its
  `(state, bytes_o2r, bytes_r2o)` as of `T`.
- **Event times.** `next_event(T)` / `prev_event(T)` return the nearest event time strictly
  after / before `T` from the merged, de-duplicated, sorted set of all `StateSample` times,
  or `None` at the ends. These drive step-by-event.

### 3.4 Transport (TUI, pure)

`Transport { start, end, cursor, speed, playing }`:

- **Construction** clamps to the capture: `cursor = start`, `speed = 1.0`, `playing = false`.
- **`toggle_play()`** flips `playing`. Toggling to playing while the cursor is already at
  `end` first rewinds the cursor to `start` (so a finished playthrough replays rather than
  no-opping).
- **Speed** is a fixed ladder `[0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0]`. `faster()` /
  `slower()` step one rung, clamped at the ends. Speed only scales cursor advance; it never
  re-parses (ADR-0004).
- **`seek(forward)`** moves the cursor by a fixed step of `max((end - start) / 50, 1ns)`
  (≈2% of the span), clamped to `[start, end]`.
- **`set_cursor(T)`** clamps `T` into `[start, end]` (used by step-by-event).
- **`tick(dt)`** — when `playing`, advances `cursor` by `round(speed · dt)` nanoseconds,
  clamped to `end`; reaching `end` sets `playing = false` (auto-pause at EOF). When paused,
  `tick` is a no-op. `dt` is injected wall-clock nanoseconds; `Transport` never reads a
  clock.

### 3.5 Master list as of `T` (App)

`App` owns the `Timeline`, the `Transport`, a precomputed static projection of each
connection (`ConnId`, peer `Endpoint`, service label, `origin_inferred`, and the lowercased
search string), and the M4 sort/filter/selection state.

- **`visible()`** resolves the active rows at `transport.cursor`, joins each with its static
  projection, applies the `/` filter (subsequence match over the search string, unchanged
  from M4 §3.5), and sorts (M4 §3.4). The searchable string uses the connection's static
  fields (origin/responder/service) plus its **as-of-`T`** state, so filtering by state
  reflects the cursor.
- **Selection** stays keyed by `ConnId` and reconciles to the first visible row (or none)
  when the selected connection is not active at `T` or is filtered out (M4 §3.6). Every
  transport action that changes the active set reconciles the selection.
- **Sort by a time-varying column** (`BytesUp`/`BytesDown`/`State`) uses the as-of-`T`
  values; sort by `Peer` uses the static endpoint.

### 3.6 Time rendering

Cursor and total times render as seconds with fixed 3-decimal precision (`12.480s`), derived
from nanoseconds by integer arithmetic (no locale, no float formatting) so `TestBackend`
snapshots are deterministic. Speed renders as `<n>x` with one decimal (`0.1x`, `1.0x`,
`10.0x`).

### 3.7 Key modality (extends M4 §3.5)

- **Navigation mode** adds: `space` → `toggle_play`; `←` → `seek(back)`, `→` →
  `seek(forward)`; `+` and `=` → `faster`; `-` and `_` → `slower`; `.` → step to
  `next_event`, `,` → step to `prev_event`. All existing navigation keys are unchanged.
- **Filter-input mode** is unchanged: every printable character (including `space`, `+`,
  `,`, `.`) appends to the query; only `Enter`/`Esc`/`Backspace` are commands; `Ctrl-C`
  quits. Transport keys do **not** fire while typing a filter.

### 3.8 Run loop (impure shell)

`run(app)` polls for input with a timeout, and each iteration: draws the frame, reads a key
if one is ready (dispatching to `handle_key`), measures the wall-clock delta since the last
iteration with `Instant::now()`, and calls `app.tick(delta)`. This is the only code that
reads a clock. It stays untested (ADR-0009) and small.

## 4. Architecture

### Engine (`tcpvisr-engine`)

- `timeline.rs`: `StateSample`, `Timeline`, the interval index, and the event-time index.
  Pure; unit-tested from hand-built vectors.
- `config.rs`: add `collect_state_timeline: bool` (default `false`).
- `tracker.rs`: accumulate a `Vec<StateSample>` per connection when the flag is set (one
  snapshot per processed segment, after state/byte accounting), counting against
  `max_samples`; add `into_timeline(self) -> Result<Timeline, MetricError>`.
- `lib.rs`: re-export `StateSample` and `Timeline`.

### TUI (`tcpvisr-tui`)

- `transport.rs`: the pure `Transport` (new).
- `app.rs`: `App` holds `Timeline` + `Transport` + static per-connection projection + M4
  state; `visible()` resolves as of the cursor; add transport-delegating methods
  (`toggle_play`, `seek`, `faster`/`slower`, `step_forward`/`step_back`, `tick`) plus
  accessors the header/footer render.
- `keys.rs`: map the new navigation-mode keys (§3.7).
- `render.rs`: draw the transport status in the header and the transport hints in the
  footer; render rows from the as-of-`T` resolution; the "no connections active" empty state.
- `run.rs`: the poll + tick loop (§3.8).
- `lib.rs`: re-export `Transport`.

### CLI (`tcp-visr`)

- `run_replay`: configure `collect_state_timeline = true` (+ a `max_samples` ceiling),
  observe the capture, `into_timeline()?`, build the title, and `App::new(timeline, title)`.
  Surface a `SampleCeiling` error as a fatal actionable message.

Dependency direction is unchanged (TUI → engine → core).

## 5. Success criteria (falsifiable)

1. **Interval index membership.** A `Timeline` with a connection open on `[100, 200]` and
   another on `[150, +∞]` (still open, capture end 300): `active_at(50)` is empty;
   `active_at(120)` is the first only; `active_at(180)` is both; `active_at(250)` is the
   second only. (Unit test.)
2. **As-of-`T` resolution.** For a connection with `StateSample`s at `t = 100` (SynSent, 0/0),
   `t = 200` (Established, 500/0), `t = 300` (Established, 500/1000): `resolve_at(150)` →
   (SynSent, 0, 0); `resolve_at(250)` → (Established, 500, 0); `resolve_at(999)` →
   (Established, 500, 1000). (Unit test.)
3. **Still-open vs closed bound.** A connection whose final state is `Closed` at `last_at`
   drops out of `active_at(T)` for `T > last_at`; one still `Established` at capture end
   stays active through `end`. (Unit test.)
4. **Event stepping.** `next_event`/`prev_event` return the adjacent distinct event times
   and `None` past the ends; duplicate times across connections collapse to one. (Unit test.)
5. **Transport play/pause + tick.** From a paused cursor at `start`, `toggle_play` then
   `tick(dt)` advances the cursor by `speed · dt`; at `1.0x`, `tick(1s)` moves it 1s.
   Paused, `tick` does not move it. `tick` never moves past `end`, and reaching `end` clears
   `playing`. (Unit test.)
6. **Speed ladder.** `faster`/`slower` step the ladder and clamp at `0.1x` and `10.0x`; a
   `10×` cursor advance is exactly `10 · dt`. (Unit test.)
7. **Seek + step clamp.** `seek` moves ≈2% of the span and clamps at both ends; stepping
   past the last/first event is a no-op (clamped). (Unit test.)
8. **Toggle-at-end rewinds.** With the cursor at `end` and paused, `toggle_play` sets the
   cursor to `start` and starts playing. (Unit test.)
9. **Master list resolves as of `T`.** An `App` built from a two-connection `Timeline`,
   with the cursor moved to a `T` where only one connection is active, shows exactly that
   row via `visible()`, with its as-of-`T` state and bytes; moving the cursor later reveals
   the second row and updates the first row's bytes. (Unit test.)
10. **Selection reconciles across the cursor.** Selecting a connection, then seeking to a
    `T` where it is not active, moves the selection to the first visible row (or none);
    seeking back restores a valid selection. (Unit test.)
11. **Keys.** In navigation mode `space` toggles play, `←`/`→` change the cursor by a seek
    step, `+`/`-` change speed, `.`/`,` step events. In filter-input mode `space`/`+`/`,`/`.`
    append to the query and fire no transport command; `Ctrl-C` still quits. (Unit tests.)
12. **Render.** `render` into a `TestBackend` shows the play/pause glyph, the speed, the
    `t=cursor / total` readout, the transport key hints, and rows resolved as of `T`; at a
    `T` with no active connection it shows `no connections active`. (TestBackend tests.)
13. **CLI wiring.** `tcp-visr replay <file>` on a fixture builds a `Timeline` and enters the
    TUI; with stdout not a TTY it still exits non-zero with the interactive-terminal message.
    (Bin integration test — the non-TTY path, since the loop needs no TTY to reach the
    guard.)
14. **Ceiling fail-fast.** With `max_samples` too small for the capture, `into_timeline`
    returns `SampleCeiling` and `replay` surfaces it as a fatal actionable error. (Unit /
    integration test.)

## 6. Failure modes handled

- **Cursor before the first / after the last connection** → empty active set → empty-state
  render, still quittable (§3.2, criterion 12).
- **Empty capture** → `start == end == 0`, no rows, transport inert, `q` quits.
- **Selection invalidated by a cursor move or filter** → deterministic fallback (§3.5).
- **Sample-ceiling overflow** → fail fast with the existing actionable message (§3.1).
- **Non-monotonic capture time** → bounds use min `opened_at` / max `last_at`; per-connection
  `opened_at ≤ last_at` holds by construction (tracker uses saturating/max on `last_at`);
  seek/tick clamp so the cursor never leaves `[start, end]`.
- **Playhead at EOF** → `tick` auto-pauses; `toggle_play` rewinds (§3.4).

## 7. Testing

- **Pure `Timeline` unit tests** for criteria 1–4, built from hand-constructed
  `(Connection, Vec<StateSample>)` vectors (no capture, no I/O). Include a `u32`-wrap /
  serial-arithmetic-adjacent edge only where the timeline itself compares times (times are
  `Nanos`, monotonic — no serial wrap — so the risk is byte/state carry-forward, not seq).
- **Pure `Transport` unit tests** for criteria 5–8.
- **Pure `App` unit tests** for criteria 9–10 and the key mappings (11), from hand-built
  timelines.
- **`ratatui::TestBackend` render tests** for criterion 12.
- **Bin integration tests** (`crates/tcp-visr/tests/`) for criteria 13–14: the non-TTY
  guard on `replay`, and the ceiling error surfaced as a fatal message.
- Test behavior, not implementation: assert what `visible()` / `resolve_at` / the rendered
  buffer report, not how the interval index or cursor math is computed.
