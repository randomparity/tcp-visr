# Spec: M4 — TUI shell, master list

**Milestone:** M4 (design §10, §6) · **Issue:** #7 · **Release:** v0.1.0 (Replay MVP)
**ADR:** [ADR-0009 — TUI master-list architecture](../../adr/0009-tui-master-list-architecture.md)

## 1. Goal

Give the user an interactive terminal view of the connections in a replayed capture:
browse them in a table, **sort**, filter with `/`, move a **selection**, and see
**port→service** labels. This is the shell the detail panes (M6–M9) and the timeline
(M5) will later hang off of.

## 2. Scope

### In scope
- A `tcp-visr replay <file>` subcommand that opens a full-screen TUI over the capture.
- A master-list table of the capture's connections (one row per `Connection` instance
  as returned by `Tracker::into_connections()`).
- Columns: peer (responder `ip:port`), service label, state, bytes ↑ (origin→responder),
  bytes ↓ (responder→origin). A mid-stream marker for `origin_inferred` connections.
- **Sort**: cycle the sort field with `s`; toggle ascending/descending with `S`.
- **Filter**: `/` enters a filter-input line; typing narrows the list by a
  case-insensitive subsequence match; `Enter` keeps the filter and returns to
  navigation; `Esc` clears the filter and returns to navigation.
- **Selection**: `↑`/`k` and `↓`/`j` move a highlighted row within the visible list.
- **Quit**: `q` or `Ctrl-C`.
- Terminal is always restored (raw mode off, alternate screen left) on normal exit,
  error, or panic.

### Out of scope (deferred, do not build)
- **Timeline / "as of T"** resolution, play/pause, seek, speed — M5. M4 shows the
  connection set **as of end-of-capture** (the full `into_connections()` result). The
  footer advertises only the keys that work in M4; no phantom `⏎ open` / `space pause`
  controls.
- **Detail panes** (Time/Sequence, in-flight, RTT, throughput) — M6–M9. `Enter` on a
  row does nothing in M4 (reserved).
- **Process attribution** (a "process" column) — requires kernel enrichment (M12) and
  is live-only. Not available in replay; no process column in M4.
- **DNS host names** for the peer — M10. The peer is shown as `ip:port`.
- **Live capture** — M11. `live` remains a stub.

## 3. User-facing behavior

### 3.1 Entry point
`tcp-visr replay <file>`:
1. Streams `file` through the replay faucet into a `Tracker` (same path as `conns`),
   collecting `Vec<Connection>` and the `SkipCounts`.
2. Requires an interactive terminal. If stdout is not a TTY, it exits non-zero with an
   actionable message (`replay requires an interactive terminal (stdout is not a tty)`)
   rather than hanging or corrupting a pipe. This is what makes the feature CI-safe:
   the event loop never runs under test; the pure state and rendering are tested
   directly.
3. Whole-file ingest failures are fatal with the existing actionable message
   (`opening capture …`); per-packet problems are skipped and counted (design §7), and
   the skip total is shown in the status line.

### 3.2 Layout
```
┌ tcp-visr — <file>  (<N> connections, skipped <S>) ───────────────────────────┐
│ PEER                       SERVICE   STATE        ↑BYTES     ↓BYTES           │
│▸10.0.0.5:51324→github…:443 https     ESTABLISHED   1.2 KB     34.0 KB         │
│ 10.0.0.9:22                ssh       ESTABLISHED     840 B      2.1 KB        │
│ …                                                                            │
├──────────────────────────────────────────────────────────────────────────────┤
│ / filter   s sort:peer▲   S reverse   ↑↓ select   q quit                      │
└──────────────────────────────────────────────────────────────────────────────┘
```
- The header names the file and the connection / skip counts.
- The footer shows the working key bindings and the current sort field + direction.
- In filter mode the footer is replaced by the filter input line: `/‹query›`.
- Empty capture: the table area shows `no connections in capture`; `q` still quits.

### 3.3 Service labels
`service_name(port) -> Option<&'static str>` maps a small built-in table of well-known
TCP ports to names (e.g. 22→ssh, 53→domain, 80→http, 443→https, 5432→postgresql).
The label shown is for the **responder** (server-side) port — the meaningful service.
An unknown port yields no label (the SERVICE cell is blank). The table is a static,
reviewable constant: no `/etc/services` read, no I/O, no new dependency
(design philosophy: no I/O in testable units, deterministic output).

### 3.4 Sort
Sort field is one of: `Peer`, `State`, `BytesUp`, `BytesDown`. `s` cycles the field
forward (wrapping). `S` toggles ascending/descending. Each field has a natural default
direction on first selection (peer/state ascending; byte fields descending). Sort is a
total order — ties break by `ConnId` (pair then instance) so the order is deterministic
and stable across identical inputs.

### 3.5 Filter
`/` enters filter-input mode and starts an empty query (or resumes editing the current
one). Printable characters append; `Backspace` deletes the last character. The visible
set is every connection whose composite searchable string —
`"{origin} {responder} {service} {state}"`, lowercased — contains the lowercased query
as a **subsequence** (characters appear in order, not necessarily adjacent). `Enter`
leaves input mode keeping the filter applied; `Esc` clears the query and leaves input
mode. An empty query matches everything.

### 3.6 Selection
Selection is tracked by `ConnId`, not row index, so the highlighted connection stays put
when a re-sort or filter reorders rows. Movement selects the previous/next row in the
current visible order. When the visible set changes (filter edit) and the selected
connection is no longer visible, selection falls back to the first visible row (or none
if the visible set is empty). Movement past an end clamps (no wrap).

## 4. Architecture (see ADR-0009)

`tcpvisr-tui` gains four seams, three pure and one impure:

- **`App`** (pure): owns the connection rows, sort field + direction, filter query,
  filter-input flag, and selected `ConnId`. Pure methods: `cycle_sort`, `toggle_dir`,
  `enter_filter`, `push_filter`, `pop_filter`, `confirm_filter`, `cancel_filter`,
  `move_up`/`move_down`, and `visible()` (returns the filtered+sorted rows). No I/O.
- **`handle_key(&mut App, KeyEvent) -> Outcome`** (pure): the single decision point that
  maps a key to state changes; returns `Outcome::{Continue, Quit}`. Unit-tested by
  constructing synthetic `KeyEvent`s.
- **`render(frame, &App)`** (pure): draws the table + header + footer/filter line into a
  ratatui `Frame`. Tested with `ratatui::TestBackend` snapshot assertions (design §8).
- **`run(conns, skipped, title) -> io::Result<()>`** (impure): the thin terminal shell —
  enable raw mode + alternate screen behind a restore guard, then
  `loop { render; read event; if handle_key == Quit break }`, then restore. Not
  unit-tested; kept minimal.

`tcpvisr-tui` depends on `tcpvisr-engine` (for `Connection`, `ConnId`, `ConnState`) and
`tcpvisr-core` (for `Endpoint`), consistent with the dependency direction toward
`tcpvisr-core`. Port→service labeling lives in `tcpvisr-tui` (a display concern).

## 5. Success criteria (falsifiable)

1. `tcp-visr replay <file>` with stdout **not** a TTY exits non-zero and prints
   `replay requires an interactive terminal`. (Integration test via piped output.)
2. `App::new` over a fixture's connections produces one row per connection, each with the
   correct peer string, and the correct service label for known responder ports and a
   blank label for an unknown port. (Unit test.)
3. Cycling sort with `s` visits `Peer → State → BytesUp → BytesDown → Peer`; the visible
   order matches the field + direction; ties are ordered by `ConnId`. (Unit test.)
4. `S` reverses the visible order. (Unit test.)
5. Entering `/`, typing a subsequence present in exactly one connection's searchable
   string, narrows `visible()` to that one connection; `Backspace` widens it again;
   `Esc` clears; `Enter` keeps. (Unit tests.)
6. Selecting a connection, then re-sorting, keeps the same `ConnId` selected. Filtering
   it out falls back to the first visible row. Moving past either end clamps. (Unit
   tests.)
7. `render` into a `TestBackend` shows the header counts, the column titles, a `▸` on the
   selected row, the mid-stream marker for an inferred connection, and the footer key
   hints; in filter mode it shows the `/query` line. (TestBackend snapshot tests.)
8. Empty capture: `App::new(vec![])` has no rows, `visible()` is empty, `render` shows
   `no connections`, and `handle_key` still quits on `q`. (Unit + render test.)
9. `q` and `Ctrl-C` both return `Outcome::Quit`. (Unit test.)

## 6. Failure modes handled

- **Non-interactive stdout** → actionable error, no hang (§3.1).
- **Empty capture** → empty-state render, quittable (§3.2, criterion 8).
- **Terminal restore** → a guard restores raw mode and the main screen on normal return,
  early error, and panic, so a crash never leaves the user's terminal wedged.
- **Selection invalidated by filter** → deterministic fallback to first visible / none
  (§3.6).
- **Unknown port** → blank service cell, never a crash or a bogus label (§3.3).
- **Ingest skips** → surfaced as a count in the header, never fatal (design §7).

## 7. Testing

- **Pure `App`/`handle_key` unit tests** in `tcpvisr-tui` for every success criterion
  2–6, 8, 9. Built from hand-constructed `Vec<Connection>` (no capture, no I/O).
- **`ratatui::TestBackend` render tests** for criterion 7 and 8.
- **Bin integration test** (`crates/tcp-visr/tests/`) for criterion 1 (non-TTY error)
  and that `replay` no longer reports "not implemented". The existing
  `unimplemented_subcommand` test is repointed from `replay` to `live` (still a stub).
- Test behavior, not implementation: assert what `visible()` / the rendered buffer
  contains, not how sorting or matching is computed.
