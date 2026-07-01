# ADR-0009: TUI master-list architecture

> Status: Accepted
> Date: 2026-06-30

## Context

M4 (design §10, §6) introduces the first interactive surface: a full-screen ratatui
master list of a replayed capture's connections, with sort, `/` filter, selection, and
port→service labels. Two forces shape the design:

1. **Testability under CI.** The design mandates `ratatui::TestBackend` frame snapshot
   tests (§8), and the project's discipline is to test behavior deterministically. A
   crossterm event loop reading a real terminal cannot run in CI, and terminal I/O is
   non-deterministic. The interactive logic must be exercisable without a TTY.
2. **Scope discipline.** M4 is the *shell*. The timeline / "as of cursor time T"
   resolution and the cross-connection interval index are M5; detail panes are M6–M9;
   process attribution and DNS names need enrichment (M12) and live capture (M11). M4
   must not grow those.

Sub-decisions in play: how to split pure logic from terminal I/O; how to track the
selection so it survives re-sorts; and where port→service labeling lives and what
sources it.

## Decision

**We will split `tcpvisr-tui` into three pure seams and one thin impure shell.**

- `App` holds all state (rows, sort field + direction, filter query, filter-input flag,
  selected `ConnId`) and mutates it through pure methods; `visible()` returns the
  filtered+sorted rows. No I/O, no clock.
- `handle_key(&mut App, KeyEvent) -> Outcome` is the single pure decision point mapping a
  key to state changes and returning `Continue`/`Quit`.
- `render(frame, &App)` draws purely into a ratatui `Frame`.
- `run(...)` is the only impure code: it enables raw mode + the alternate screen behind a
  restore guard, loops `render → read event → handle_key`, and restores on exit, error,
  or panic. It is deliberately trivial and not unit-tested.

**We will track the selection by `ConnId`, not by row index**, so the highlighted
connection is stable across re-sort and filter reordering, with a deterministic fallback
(first visible / none) when a filter hides the selected row.

**We will source port→service labels from a small built-in static table** in
`tcpvisr-tui`, labeling the responder (server-side) port, returning `None` for unknown
ports.

**We will keep M4 static:** the list reflects end-of-capture state; no timeline, and the
footer advertises only keys that work in M4.

**`tcpvisr-tui` depends on `tcpvisr-engine` and `tcpvisr-core`**, consistent with the
dependency direction flowing toward `tcpvisr-core`.

## Consequences

- Every M4 success criterion except the terminal shell itself is a pure unit or
  `TestBackend` test; CI needs no TTY. The untested surface shrinks to `run()`'s setup /
  loop / teardown.
- Selection-by-`ConnId` costs a lookup to resolve the highlighted row each frame (N is a
  capture's connection count — small) in exchange for correct, non-jumping selection.
- The built-in service table must be maintained by hand and will not know site-local
  service names. That is acceptable for well-known ports; `/etc/services` parsing (I/O,
  platform-dependent, non-deterministic) is explicitly not adopted.
- `run()` remains a latent gap in automated coverage; it is kept small enough to review
  by eye, and its two escape hatches (non-TTY guard, restore-on-panic) are the parts most
  likely to matter.
- M5 will extend `App` with a cursor time and swap the static row set for an "as of T"
  projection without disturbing the render/key seams.

## Alternatives considered

- **Test through the crossterm event loop.** Rejected: needs a real or emulated TTY,
  reintroduces non-determinism, and the event-injection plumbing is more code than the
  logic it would test. The pure `handle_key` seam gets the same coverage for free.
- **`tui` takes `&[Connection]` and defines no state type; the bin owns the loop.**
  Rejected: pushes interactive state into the binary, duplicated per future view, and
  leaves nothing snapshot-testable in `tui`.
- **Track selection by row index.** Rejected: the highlight jumps to an unrelated
  connection whenever a sort or filter reorders the list — a real UX bug that
  `ConnId`-tracking avoids.
- **Read `/etc/services` for labels.** Rejected: I/O in an otherwise-pure unit,
  platform- and host-dependent output, and non-deterministic tests, for marginal gain
  over a well-known-ports table.
- **Add a fuzzy-match dependency (e.g. `nucleo`/`fuzzy-matcher`).** Rejected for M4: a
  case-insensitive subsequence match satisfies "fuzzy filter" for a per-capture list of
  connections with zero new dependency (each dependency is attack surface). Revisit only
  if ranking quality is later shown to matter.
- **Label the origin (client) port.** Rejected: the client port is ephemeral; the
  responder port is the one that identifies the service.
