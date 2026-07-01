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
  bytes ↓ (responder→origin). The STATE cell carries a trailing `~` for
  `origin_inferred` (mid-stream) connections. Byte counts are shown as raw integers
  (see §3.7).
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
┌ tcp-visr — capture.pcap  (2 connections, skipped 0) ─────────────────────────┐
│ PEER                    SERVICE   STATE          ↑BYTES     ↓BYTES            │
│▸140.82.121.3:443        https     ESTABLISHED      1234      34000            │
│ 10.0.0.9:22             ssh       ESTABLISHED~       840       2100           │
│ …                                                                            │
├──────────────────────────────────────────────────────────────────────────────┤
│ / filter   s sort:peer▲   S reverse   ↑↓ select   q quit                      │
└──────────────────────────────────────────────────────────────────────────────┘
```
- The peer cell is the **responder** `ip:port` only — no origin endpoint, no DNS name
  (DNS is M10). The second row above (`ESTABLISHED~`) is a mid-stream/`origin_inferred`
  connection.
- The header names the file and the connection / skip counts.
- The footer shows the working key bindings and the current sort field + direction.
- In filter mode the footer is replaced by the filter input line: `/‹query›`.
- Empty capture: the table area shows `no connections in capture`; `q` still quits.
- **Scrolling:** when there are more connections than table rows, `render` scrolls the
  table so the selected row is always visible (the offset is derived from the selected
  row index and the viewport height at render time; `App` stores no offset).

### 3.3 Service labels
`service_name(port) -> Option<&'static str>` maps a small built-in table of well-known
TCP ports to names (e.g. 22→ssh, 53→domain, 80→http, 443→https, 5432→postgresql).
The label shown is for the **responder** (server-side) port — the meaningful service.
An unknown port yields no label (the SERVICE cell is blank). The table is a static,
reviewable constant: no `/etc/services` read, no I/O, no new dependency
(design philosophy: no I/O in testable units, deterministic output).

### 3.4 Sort
Sort field is one of: `Peer`, `State`, `BytesUp`, `BytesDown`. Each field has a natural
default direction: peer/state ascending; byte fields descending.
- `s` cycles the field forward (wrapping) and **resets** the new field to its natural
  default direction.
- `S` toggles ascending/descending for the current field only (it does not change the
  field).

Sort is a total order — ties break by `ConnId` (pair then instance) so the order is
deterministic and stable across identical inputs. The initial field is `Peer` ascending.
The footer shows the current field and direction (`▲` ascending, `▼` descending), e.g.
`sort:peer▲`.

### 3.7 Byte counts
The ↑BYTES / ↓BYTES cells show the raw integer byte counts (`bytes_o2r` and `bytes_r2o`),
right-aligned, with no unit suffix, scaling, or thousands separators. This keeps the
rendered output deterministic and locale-independent for snapshot tests; human-readable
scaling is deferred (not needed for M4).

### 3.5 Filter and key modality
`handle_key` is **modal** — the same key means different things in the two modes:

- **Navigation mode** (default): `s`/`S` sort, `j`/`↓` and `k`/`↑` move the selection,
  `/` enters filter-input mode, `q` quits. `Ctrl-C` quits.
- **Filter-input mode**: every printable character (including `q`, `s`, `/`, space, …)
  **appends to the query** — none of them fire their navigation-mode command.
  `Backspace` deletes the last character; `Enter` leaves input mode keeping the filter
  applied; `Esc` clears the query and leaves input mode. `Ctrl-C` still quits (the one
  command key that works in both modes). `↑`/`↓`/`j`/`k` are inert in filter-input mode
  in M4 (no selection movement while typing).

The visible set is every connection whose composite searchable string —
`"{origin} {responder} {service} {state}"`, lowercased — contains the lowercased query as
a **subsequence** (characters appear in order, not necessarily adjacent). An empty query
matches everything.

### 3.6 Selection
The initial selection is the first row in the initial (Peer-ascending) order, or none if
the capture is empty. Selection is tracked by `ConnId`, not row index, so the highlighted
connection stays put when a re-sort or filter reorders rows. Movement selects the
previous/next row in the current visible order. When the visible set changes (filter
edit) and the selected connection is no longer visible, selection falls back to the first
visible row (or none if the visible set is empty). Movement past an end clamps (no wrap).

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
3. Cycling sort with `s` visits `Peer → State → BytesUp → BytesDown → Peer`; each `s`
   resets the new field to its natural default direction (peer/state ascending, byte
   fields descending); the visible order matches the field + direction; ties are ordered
   by `ConnId`. (Unit test.)
4. `S` toggles the current field's direction and reverses the visible order without
   changing the field. (Unit test.)
5. Entering `/`, typing a subsequence present in exactly one connection's searchable
   string, narrows `visible()` to that one connection; `Backspace` widens it again;
   `Esc` clears; `Enter` keeps. A command character typed in filter-input mode (e.g. `q`,
   `s`) appends to the query and does **not** fire its navigation command. (Unit tests.)
6. Selecting a connection, then re-sorting, keeps the same `ConnId` selected. Filtering
   it out falls back to the first visible row. Moving past either end clamps. (Unit
   tests.)
7. `render` into a `TestBackend` shows the header counts, the column titles, a `▸` on the
   selected row, the trailing-`~` mid-stream marker for an inferred connection, and the
   footer key hints; in filter mode it shows the `/query` line. A `TestBackend` shorter
   than the row count still renders the selected row (scroll-to-selection). (TestBackend
   snapshot tests.)
8. Empty capture: `App::new(vec![])` has no rows, `visible()` is empty, `render` shows
   `no connections`, and `handle_key` still quits on `q`. (Unit + render test.)
9. In navigation mode, `q` and `Ctrl-C` both return `Outcome::Quit`; in filter-input mode
   `q` returns `Outcome::Continue` (appended to the query) while `Ctrl-C` still quits.
   (Unit test.)

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
