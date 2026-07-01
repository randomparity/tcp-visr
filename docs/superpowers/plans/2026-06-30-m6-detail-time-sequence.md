# M6 — Detail: Time/Sequence (Stevens) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the first per-connection detail pane — a cursor-driven Time/Sequence (Stevens) graph with retransmit/SACK marks — on top of the M5 replay TUI.

**Architecture:** The engine collects a per-connection `SeqSample` series (one point per data segment, one per SACK block) with a wrap-unwrapped `i64` cumulative sequence offset (`rel`), gated by a new `collect_seq_timeline` flag and bounded by `max_samples`. The `Timeline` carries the series and exposes `seq_series(id)` / `x_span(id)`. A pure TUI projection (`detail.rs`) maps the focus connection's series + cursor + plot-rectangle cells to a grid of glyph marks; `App` opens/closes the pane with `Enter`/`Esc`, and `render.rs` splits master|detail when open.

**Tech Stack:** Rust 1.88.0, `ratatui` (TUI), `tcpvisr-core` (`TcpSeq` RFC-1982 arithmetic, `Nanos`, `SampleDir`), workspace crates `tcpvisr-engine` / `tcpvisr-tui` / `tcp-visr`.

## Global Constraints

- **Spec:** `docs/superpowers/specs/m6-detail-time-sequence.md`. **ADR:** `docs/adr/0011-detail-seq-timeline-and-rendering.md`.
- **The engine is pure** — no I/O, no clock reads (ADR-0002). `serial_diff`/serial arithmetic runs only in the engine, never in the TUI (ADR-0011).
- **Never naive subtraction on seq** — all seq comparison via `TcpSeq::serial_lt`/`serial_gt`/`serial_diff` (RFC 1982).
- **`MetricSample` and the `metrics` JSON are frozen** — do not touch them or the oracle goldens (ADR-0011 keeps them untouched, like ADR-0010).
- **Clippy is strict workspace-wide:** `unwrap_used`/`expect_used`/`panic`/`print_stdout` denied; `allow_attributes` denied (no item-level `#[allow]` — use a file-level `#![allow(...)]` in test-support only); `#[must_use]` on non-trivial public getters; ≤100 lines/function, cyclomatic ≤8; 100-char lines; absolute imports only. Tests are exempt via `clippy.toml`; `#[cfg(test)]` modules that need relaxations use `#![allow(clippy::unwrap_used)]` at the module top (matching the existing files).
- **Guardrails before every commit** (run from repo root):
  - `cargo fmt --all --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test --workspace`
  - (focused runs: `cargo test -p tcpvisr-engine seq`, `cargo test -p tcpvisr-tui detail`, `cargo test -p tcp-visr --test replay`)
- **Commits:** Conventional Commits, imperative, ≤72-char subject, one logical change; end every commit body with `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **`SampleDir`** (from `tcpvisr-core`) has variants `OriginToResponder` / `ResponderToOrigin`.

---

## File structure

| File | Responsibility | Change |
|------|----------------|--------|
| `crates/tcpvisr-engine/src/timeline.rs` | `SeqKind`, `SeqSample`, `Timeline` (Entry gains `seq`), `with_seq`, `seq_series`, `x_span` | modify |
| `crates/tcpvisr-engine/src/config.rs` | `collect_seq_timeline` flag | modify |
| `crates/tcpvisr-engine/src/tracker.rs` | per-direction seq unwrap + `SeqSample` collection; `into_timeline` via `with_seq` | modify |
| `crates/tcpvisr-engine/src/lib.rs` | re-export `SeqKind`, `SeqSample` | modify |
| `crates/tcpvisr-tui/src/detail.rs` | pure `SeqPlot` projection (marks grid, mapping, bucketing, cursor, degenerate spans) | create |
| `crates/tcpvisr-tui/src/app.rs` | `detail_open` flag, `open_detail`/`close_detail`/`is_detail_open`, `FocusConn`, `focus()` | modify |
| `crates/tcpvisr-tui/src/keys.rs` | nav-mode `Enter`→open, `Esc`→close | modify |
| `crates/tcpvisr-tui/src/render.rs` | split layout + `render_detail` + footer hints | modify |
| `crates/tcpvisr-tui/src/lib.rs` | wire `detail` module + re-exports | modify |
| `crates/tcp-visr/src/main.rs` | `collect_seq_timeline = true` in replay cfg + in-crate seam test (focus `seq_series` non-empty) | modify |

---

### Task 1: Engine — `SeqKind` / `SeqSample` types + re-exports

**Files:**
- Modify: `crates/tcpvisr-engine/src/timeline.rs` (top, after imports)
- Modify: `crates/tcpvisr-engine/src/lib.rs`
- Test: inline in `crates/tcpvisr-engine/src/timeline.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `pub enum SeqKind { Data { retransmit: bool, out_of_order: bool }, Sack }`;
  `pub struct SeqSample { pub t: Nanos, pub dir: SampleDir, pub rel: i64, pub len: u32, pub kind: SeqKind }` (both `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`).

- [ ] **Step 1: Write the failing test** — append to the `tests` module in `timeline.rs`:

```rust
    #[test]
    fn seq_sample_is_copy_and_holds_fields() {
        use tcpvisr_core::SampleDir;
        let s = SeqSample {
            t: Nanos(5),
            dir: SampleDir::OriginToResponder,
            rel: 42,
            len: 10,
            kind: SeqKind::Data { retransmit: true, out_of_order: false },
        };
        let copy = s; // Copy, not move
        assert_eq!(copy, s);
        assert_eq!(copy.rel, 42);
        assert_ne!(SeqKind::Sack, s.kind);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p tcpvisr-engine --lib timeline::tests::seq_sample_is_copy -- --nocapture`
Expected: FAIL — `cannot find type SeqSample` / `SeqKind`.

- [ ] **Step 3: Add the types** — at the top of `timeline.rs`, change the import line
`use tcpvisr_core::Nanos;` to `use tcpvisr_core::{Nanos, SampleDir};` and add, just above `pub struct StateSample`:

```rust
/// The kind of a Time/Sequence mark (design §6, ADR-0011 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqKind {
    /// A data-carrying segment; `retransmit`/`out_of_order` are the M3 classification.
    Data { retransmit: bool, out_of_order: bool },
    /// A SACK block, plotted in the acknowledged direction's sequence space.
    Sack,
}

/// One point on a connection's Time/Sequence graph (ADR-0011 §1). `rel` is the wrap-unwrapped
/// cumulative sequence offset from `dir`'s first-seen data seq (so a multi-GB transfer rises
/// monotonically instead of folding); `len` is the payload length (0 for a `Sack` mark).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeqSample {
    pub t: Nanos,
    pub dir: SampleDir,
    pub rel: i64,
    pub len: u32,
    pub kind: SeqKind,
}
```

- [ ] **Step 4: Re-export** — in `crates/tcpvisr-engine/src/lib.rs`, change
`pub use timeline::{AsOf, StateSample, Timeline};` to
`pub use timeline::{AsOf, SeqKind, SeqSample, StateSample, Timeline};`.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p tcpvisr-engine --lib timeline::tests::seq_sample_is_copy`
Expected: PASS.

- [ ] **Step 6: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-engine
git add crates/tcpvisr-engine/src/timeline.rs crates/tcpvisr-engine/src/lib.rs
git commit -m "feat(engine): add SeqSample/SeqKind Time/Sequence point types"
```

---

### Task 2: Engine — `Timeline::with_seq`, `seq_series`, `x_span`

**Files:**
- Modify: `crates/tcpvisr-engine/src/timeline.rs` (`Entry`, `Timeline::new`, add `with_seq`/`seq_series`/`x_span`)
- Test: inline in `timeline.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `SeqSample` (Task 1).
- Produces: `Timeline::new(Vec<(Connection, Vec<StateSample>)>)` (unchanged signature, empty seq);
  `Timeline::with_seq(Vec<(Connection, Vec<StateSample>, Vec<SeqSample>)>) -> Timeline`;
  `Timeline::seq_series(&self, id: ConnId) -> &[SeqSample]`;
  `Timeline::x_span(&self, id: ConnId) -> Option<(Nanos, Nanos)>`.

- [ ] **Step 1: Write the failing test** — append to `timeline.rs` `tests`:

```rust
    fn sq(t: u64, rel: i64, len: u32) -> SeqSample {
        use tcpvisr_core::SampleDir;
        SeqSample {
            t: Nanos(t),
            dir: SampleDir::OriginToResponder,
            rel,
            len,
            kind: SeqKind::Data { retransmit: false, out_of_order: false },
        }
    }

    #[test]
    fn with_seq_sorts_and_exposes_series_and_x_span() {
        let c = conn(0, 100, 300, ConnState::Established);
        let id = c.id;
        let tl = Timeline::with_seq(vec![(
            c,
            vec![ss(100, ConnState::Established, 0, 0)],
            vec![sq(300, 20, 10), sq(100, 0, 10)], // supplied out of t-order
        )]);
        let series = tl.seq_series(id);
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].t, Nanos(100), "sorted by t at construction");
        assert_eq!(series[1].t, Nanos(300));
        assert_eq!(tl.x_span(id), Some((Nanos(100), Nanos(300))));
    }

    #[test]
    fn seq_series_and_x_span_are_empty_none_for_unknown_id() {
        let c = conn(0, 0, 10, ConnState::Established);
        let other = ConnId {
            pair: EndpointPair::new(ep(9, 1), ep(9, 2)),
            instance: 7,
        };
        let tl = Timeline::new(vec![(c, vec![ss(0, ConnState::Established, 0, 0)])]);
        assert!(tl.seq_series(other).is_empty());
        assert_eq!(tl.x_span(other), None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p tcpvisr-engine --lib timeline::tests::with_seq`
Expected: FAIL — no `with_seq` / `seq_series` / `x_span`.

- [ ] **Step 3: Add `seq` to `Entry`** — change the `Entry` struct:

```rust
struct Entry {
    conn: Connection,
    samples: Vec<StateSample>,
    seq: Vec<SeqSample>,
    effective_end: Nanos,
}
```

- [ ] **Step 4: Rework `new` to delegate, add `with_seq`** — replace the whole `pub fn new(...) { ... }` body with:

```rust
    /// Builds a state-only timeline (no seq series); every connection's `seq` is empty. This
    /// preserves the M5 constructor so existing call sites and fixtures are unchanged.
    #[must_use]
    pub fn new(conns: Vec<(Connection, Vec<StateSample>)>) -> Self {
        Self::with_seq(conns.into_iter().map(|(c, s)| (c, s, Vec::new())).collect())
    }

    /// Builds the timeline from each connection, its `StateSample` series, and its `SeqSample`
    /// series. Both series are stable-sorted by `t` (capture time is not guaranteed monotonic,
    /// design §14); `start` is the minimum `StateSample.t`, `end` is the maximum `last_at`.
    #[must_use]
    pub fn with_seq(conns: Vec<(Connection, Vec<StateSample>, Vec<SeqSample>)>) -> Self {
        let end = conns
            .iter()
            .map(|(c, _, _)| c.last_at)
            .max()
            .unwrap_or(Nanos(0));
        let start = conns
            .iter()
            .flat_map(|(_, s, _)| s.iter().map(|x| x.t))
            .min()
            .unwrap_or(Nanos(0));
        let mut entries: Vec<Entry> = Vec::with_capacity(conns.len());
        let mut event_times: Vec<Nanos> = Vec::new();
        for (conn, mut samples, mut seq) in conns {
            samples.sort_by_key(|s| s.t);
            seq.sort_by_key(|s| s.t);
            for s in &samples {
                event_times.push(s.t);
            }
            let closed = matches!(conn.state, ConnState::Closed | ConnState::Reset);
            let effective_end = if closed { conn.last_at } else { end };
            entries.push(Entry {
                conn,
                samples,
                seq,
                effective_end,
            });
        }
        event_times.sort_unstable();
        event_times.dedup();
        Self {
            entries,
            start,
            end,
            event_times,
        }
    }
```

- [ ] **Step 5: Add the accessors** — inside `impl Timeline`, after `connections()`:

```rust
    /// The focus connection's `SeqSample` series (`t`-sorted), or an empty slice if `id` is
    /// unknown or its series was not collected.
    #[must_use]
    pub fn seq_series(&self, id: ConnId) -> &[SeqSample] {
        match self.entries.iter().find(|e| e.conn.id == id) {
            Some(e) => &e.seq,
            None => &[],
        }
    }

    /// The connection's `[opened_at, effective_end]` time span for the detail X axis
    /// (`effective_end` is `last_at` if closed, else the capture end), or `None` if unknown.
    #[must_use]
    pub fn x_span(&self, id: ConnId) -> Option<(Nanos, Nanos)> {
        self.entries
            .iter()
            .find(|e| e.conn.id == id)
            .map(|e| (e.conn.opened_at, e.effective_end))
    }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p tcpvisr-engine --lib timeline::`
Expected: PASS (new tests + all existing timeline tests still green — `new` behavior is unchanged).

- [ ] **Step 7: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-engine
git add crates/tcpvisr-engine/src/timeline.rs
git commit -m "feat(engine): carry per-connection seq series in Timeline"
```

---

### Task 3: Engine — `collect_seq_timeline` config flag

**Files:**
- Modify: `crates/tcpvisr-engine/src/config.rs`
- Test: inline in `config.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `EngineConfig.collect_seq_timeline: bool` (default `false`).

- [ ] **Step 1: Extend the `defaults_match_spec` test** — add to that test, after the
`collect_state_timeline` assertion:

```rust
        assert!(!c.collect_seq_timeline);
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-engine --lib config::tests::defaults_match_spec`
Expected: FAIL — no field `collect_seq_timeline`.

- [ ] **Step 3: Add the field** — in `struct EngineConfig`, after `collect_state_timeline`:

```rust
    /// Whether the tracker records a per-segment `SeqSample` Time/Sequence series (M6 detail).
    pub collect_seq_timeline: bool,
```

and in `impl Default`, after `collect_state_timeline: false,`:

```rust
            collect_seq_timeline: false,
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p tcpvisr-engine --lib config::`
Expected: PASS.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-engine
git add crates/tcpvisr-engine/src/config.rs
git commit -m "feat(engine): add collect_seq_timeline config flag"
```

---

### Task 4: Engine — seq unwrap + `SeqSample` collection in the tracker

**Files:**
- Modify: `crates/tcpvisr-engine/src/tracker.rs`
- Test: inline in `tracker.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `SeqSample`/`SeqKind` (Task 1), `Timeline::with_seq` (Task 2), `collect_seq_timeline` (Task 3), `EngineConfig`, `MetricState::observe`, `TcpSeq::serial_gt`/`serial_diff`, `SampleDir`.
- Produces: with `collect_seq_timeline = true`, `Tracker::into_timeline()` returns a `Timeline` whose `seq_series(id)` holds one `Data` point per data segment (dir = segment direction) and one `Sack` point per SACK block (dir = acked/opposite direction), each with an unwrapped `i64 rel`, counted against `max_samples`.

- [ ] **Step 1: Write the failing tests** — add a new `#[cfg(test)] mod seq_tests` at the end of `tracker.rs`:

```rust
#[cfg(test)]
mod seq_tests {
    use super::test_support::{ep, seg};
    use super::*;
    use tcpvisr_core::{
        FlowKey, Item, Nanos, SampleDir, Segment, TcpFlags, TcpOptions, TcpSeq,
    };

    fn seq_cfg() -> EngineConfig {
        EngineConfig {
            collect_state_timeline: true,
            collect_seq_timeline: true,
            ..EngineConfig::default()
        }
    }

    fn only_id(tl: &crate::timeline::Timeline) -> ConnId {
        tl.connections().next().expect("one connection").id
    }

    // A segment carrying a single SACK block (L, R), in src->dst direction.
    fn seg_sack(
        src: (core::net::IpAddr, u16),
        dst: (core::net::IpAddr, u16),
        flags: u16,
        seq: u32,
        ack: u32,
        ts: u64,
        block: (u32, u32),
    ) -> Item {
        let mut options = TcpOptions::default();
        options.sack_blocks.push((TcpSeq(block.0), TcpSeq(block.1)));
        Item::Segment(Segment {
            ts: Nanos(ts),
            flow: FlowKey {
                src_ip: src.0,
                src_port: src.1,
                dst_ip: dst.0,
                dst_port: dst.1,
            },
            seq: TcpSeq(seq),
            ack: TcpSeq(ack),
            flags: TcpFlags(flags),
            window: 0,
            options,
            payload_len: 0,
        })
    }

    #[test]
    fn data_points_carry_unwrapped_rel_and_len() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(seq_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 20, 2_000));
        let tl = t.into_timeline().expect("timeline");
        let series: Vec<_> = tl
            .seq_series(only_id(&tl))
            .iter()
            .filter(|p| p.dir == SampleDir::OriginToResponder)
            .copied()
            .collect();
        assert_eq!(series.len(), 2);
        assert_eq!((series[0].rel, series[0].len), (0, 10));
        assert_eq!((series[1].rel, series[1].len), (10, 20));
        assert_eq!(
            series[1].kind,
            SeqKind::Data { retransmit: false, out_of_order: false }
        );
    }

    #[test]
    fn rel_unwraps_across_a_u32_wrap() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(seq_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, u32::MAX - 100, 1, 50, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 200, 1, 50, 2_000));
        let tl = t.into_timeline().expect("timeline");
        let rels: Vec<i64> = tl
            .seq_series(only_id(&tl))
            .iter()
            .filter(|p| p.dir == SampleDir::OriginToResponder)
            .map(|p| p.rel)
            .collect();
        // 200.serial_diff(u32::MAX-100) == 301 — a forward advance, not a fold.
        assert_eq!(rels, vec![0, 301]);
    }

    #[test]
    fn rel_rises_monotonically_across_multiple_wraps() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(seq_cfg());
        let step: u32 = 1_200_000_000; // ~1.2 GB per segment; 4 segments wrap u32 twice
        let mut seq: u32 = 0;
        let mut ts = 0u64;
        for _ in 0..4 {
            ts += 1_000;
            t.observe(&seg(c, s, TcpFlags::ACK, seq, 1, step, ts));
            seq = seq.wrapping_add(step);
        }
        let tl = t.into_timeline().expect("timeline");
        let rels: Vec<i64> = tl
            .seq_series(only_id(&tl))
            .iter()
            .filter(|p| p.dir == SampleDir::OriginToResponder)
            .map(|p| p.rel)
            .collect();
        assert_eq!(rels.len(), 4);
        assert!(
            rels.windows(2).all(|w| w[1] > w[0]),
            "rel strictly increases across wraps: {rels:?}"
        );
        assert_eq!(rels[3], 3 * i64::from(step), "no fold: 3 steps forward");
    }

    #[test]
    fn sack_point_lands_in_the_acked_direction_frame() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut t = Tracker::new(seq_cfg());
        // O2R data anchors the O2R frame at seq 1000 (rel 0).
        t.observe(&seg(c, s, TcpFlags::ACK, 1000, 1, 100, 1_000));
        // R2O ack carrying a SACK block for O2R bytes [1200, 1300).
        t.observe(&seg_sack(s, c, TcpFlags::ACK, 1, 1101, 2_000, (1200, 1300)));
        let tl = t.into_timeline().expect("timeline");
        let sacks: Vec<_> = tl
            .seq_series(only_id(&tl))
            .iter()
            .filter(|p| p.kind == SeqKind::Sack)
            .copied()
            .collect();
        assert_eq!(sacks.len(), 1);
        assert_eq!(sacks[0].dir, SampleDir::OriginToResponder, "acked direction");
        assert_eq!(sacks[0].rel, 200, "1200 - 1000 in the O2R frame");
        assert_eq!(sacks[0].len, 0);
    }

    #[test]
    fn seq_collection_counts_against_the_ceiling() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        let mut cfg = seq_cfg();
        cfg.max_samples = 1; // first segment already produces state + seq samples
        let mut t = Tracker::new(cfg);
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 10, 1_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 110, 1, 20, 2_000));
        let err = t.into_timeline().expect_err("ceiling");
        assert!(matches!(err, MetricError::SampleCeiling { .. }));
    }

    #[test]
    fn retransmit_and_ooo_classified_on_seq_points() {
        let (c, s) = (ep(1, 1234), ep(2, 80));
        // Retransmit: behind-frontier re-send after a gap >= reorder_window (3ms default).
        let mut t = Tracker::new(seq_cfg());
        t.observe(&seg(c, s, TcpFlags::ACK, 200, 1, 100, 1_000_000));
        t.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 100, 4_000_000)); // 3ms gap -> retransmit
        let tl = t.into_timeline().expect("timeline");
        let kinds: Vec<_> = tl.seq_series(only_id(&tl)).iter().map(|p| p.kind).collect();
        assert_eq!(kinds[1], SeqKind::Data { retransmit: true, out_of_order: false });

        // Out-of-order: behind-frontier within the reorder window.
        let mut t2 = Tracker::new(seq_cfg());
        t2.observe(&seg(c, s, TcpFlags::ACK, 200, 1, 100, 1_000));
        t2.observe(&seg(c, s, TcpFlags::ACK, 100, 1, 100, 1_001)); // 1us gap -> out-of-order
        let tl2 = t2.into_timeline().expect("timeline");
        let kinds2: Vec<_> = tl2.seq_series(only_id(&tl2)).iter().map(|p| p.kind).collect();
        assert_eq!(kinds2[1], SeqKind::Data { retransmit: false, out_of_order: true });
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p tcpvisr-engine --lib seq_tests`
Expected: FAIL — no seq collection yet (empty series / no ceiling trip).

- [ ] **Step 3: Add direction helpers + the unwrap state** — at the top of `tracker.rs`, change the import to add `SampleDir` and the seq types:

```rust
use tcpvisr_core::{Endpoint, Item, MetricSample, Nanos, SampleDir, Segment, TcpFlags, TcpSeq};
```

add `use crate::timeline::{SeqKind, SeqSample, StateSample, Timeline};` (replace the existing `use crate::timeline::{StateSample, Timeline};`), and add these free functions and the `SeqUnwrap` type near the top (after `advance_baseline`):

```rust
fn dir_index(d: Direction) -> usize {
    match d {
        Direction::OriginToResponder => 0,
        Direction::ResponderToOrigin => 1,
    }
}

fn dir_sample(d: Direction) -> SampleDir {
    match d {
        Direction::OriginToResponder => SampleDir::OriginToResponder,
        Direction::ResponderToOrigin => SampleDir::ResponderToOrigin,
    }
}

fn dir_opposite(d: Direction) -> Direction {
    match d {
        Direction::OriginToResponder => Direction::ResponderToOrigin,
        Direction::ResponderToOrigin => Direction::OriginToResponder,
    }
}

/// Per-direction sequence-unwrap state (ADR-0011 §1): anchors the first-seen seq at `rel = 0`
/// and accumulates the bounded signed serial distance from a running frontier into an `i64`, so
/// a stream that wraps the 32-bit space many times rises monotonically instead of folding.
#[derive(Default, Clone, Copy)]
struct SeqUnwrap {
    frontier: Option<(TcpSeq, i64)>,
}

impl SeqUnwrap {
    fn offset(&mut self, seq: TcpSeq) -> i64 {
        match self.frontier {
            None => {
                self.frontier = Some((seq, 0));
                0
            }
            Some((fseq, frel)) => {
                if seq == fseq {
                    frel
                } else if seq.serial_gt(fseq) {
                    let rel = frel + i64::from(seq.serial_diff(fseq));
                    self.frontier = Some((seq, rel));
                    rel
                } else {
                    frel - i64::from(fseq.serial_diff(seq))
                }
            }
        }
    }
}
```

- [ ] **Step 4: Give `ConnTrack` the seq buffers + a builder** — in `struct ConnTrack`, after `states: Vec<StateSample>,`:

```rust
    seq: Vec<SeqSample>,
    unwrap: [SeqUnwrap; 2],
```

and add this method in `impl ConnTrack` (after `snapshot`):

```rust
    /// Appends this segment's Time/Sequence points to `out`: one `Data` point (its own
    /// direction) when the segment carries payload, and one `Sack` point (the acked/opposite
    /// direction) per SACK block. Mutates the per-direction unwrap frontiers.
    fn push_seq_points(
        &mut self,
        seg: &Segment,
        dir: Direction,
        sample: &MetricSample,
        out: &mut Vec<SeqSample>,
    ) {
        if seg.payload_len > 0 {
            let rel = self.unwrap[dir_index(dir)].offset(seg.seq);
            out.push(SeqSample {
                t: seg.ts,
                dir: dir_sample(dir),
                rel,
                len: seg.payload_len,
                kind: SeqKind::Data {
                    retransmit: sample.retransmit,
                    out_of_order: sample.out_of_order,
                },
            });
        }
        if !seg.options.sack_blocks.is_empty() {
            let acked = dir_opposite(dir);
            let ai = dir_index(acked);
            for &(left, _right) in &seg.options.sack_blocks {
                let rel = self.unwrap[ai].offset(left);
                out.push(SeqSample {
                    t: seg.ts,
                    dir: dir_sample(acked),
                    rel,
                    len: 0,
                    kind: SeqKind::Sack,
                });
            }
        }
    }
```

Then initialize the two new fields in the `ConnTrack { ... }` literal in `create_instance` (after `states: Vec::new(),`):

```rust
            seq: Vec::new(),
            unwrap: [SeqUnwrap::default(); 2],
```

- [ ] **Step 5: Add `record_seq` + `collect_seq_points` to `Tracker`** — after `record_state`:

```rust
    /// Stores one `SeqSample` on the instance at `idx`, enforcing `max_samples`.
    fn record_seq(&mut self, idx: usize, sample: SeqSample) {
        if self.overflowed {
            return;
        }
        if self.collected_samples >= self.config.max_samples {
            self.overflowed = true;
            return;
        }
        self.collected_samples += 1;
        self.conns[idx].seq.push(sample);
    }

    /// Builds and records this segment's seq points when seq collection is on and not overflowed.
    fn collect_seq_points(&mut self, idx: usize, seg: &Segment, dir: Direction, sample: &MetricSample) {
        if self.overflowed || !self.config.collect_seq_timeline {
            return;
        }
        let mut points = Vec::new();
        self.conns[idx].push_seq_points(seg, dir, sample, &mut points);
        for p in points {
            self.record_seq(idx, p);
        }
    }
```

- [ ] **Step 6: Derive the metric sample when seq collection is on, in `observe_segment`** — replace the existing block:

```rust
                if !self.overflowed && self.should_collect(self.conns[idx].id) {
                    let sample = self.conns[idx].metrics.observe(seg, dir, &self.config);
                    self.record_sample(idx, sample);
                }
                return;
```

with:

```rust
                let want_metric = self.should_collect(self.conns[idx].id);
                if !self.overflowed && (want_metric || self.config.collect_seq_timeline) {
                    let sample = self.conns[idx].metrics.observe(seg, dir, &self.config);
                    if want_metric {
                        self.record_sample(idx, sample);
                    }
                    self.collect_seq_points(idx, seg, dir, &sample);
                }
                return;
```

- [ ] **Step 7: Same in `create_instance`** — replace:

```rust
        let sample = (!self.overflowed && self.should_collect(track.id))
            .then(|| track.metrics.observe(seg, dir, &self.config));
        let idx = self.conns.len();
        self.conns.push(track);
        self.live.insert(pair, idx);
        if let Some(sample) = sample {
            self.record_sample(idx, sample);
        }
```

with:

```rust
        let want_metric = self.should_collect(track.id);
        let sample = (!self.overflowed && (want_metric || self.config.collect_seq_timeline))
            .then(|| track.metrics.observe(seg, dir, &self.config));
        let idx = self.conns.len();
        self.conns.push(track);
        self.live.insert(pair, idx);
        if let Some(sample) = sample {
            if want_metric {
                self.record_sample(idx, sample);
            }
            self.collect_seq_points(idx, seg, dir, &sample);
        }
```

- [ ] **Step 8: Pass seq through `into_timeline`** — replace its `pairs` construction:

```rust
        let pairs: Vec<(Connection, Vec<StateSample>)> = self
            .conns
            .iter()
            .map(|c| (c.view(), c.states.clone()))
            .collect();
        Ok(Timeline::new(pairs))
```

with:

```rust
        let triples: Vec<(Connection, Vec<StateSample>, Vec<SeqSample>)> = self
            .conns
            .iter()
            .map(|c| (c.view(), c.states.clone(), c.seq.clone()))
            .collect();
        Ok(Timeline::with_seq(triples))
```

- [ ] **Step 9: Run tests to verify they pass**

Run: `cargo test -p tcpvisr-engine --lib seq_tests`
Expected: PASS (5 tests). Then `cargo test -p tcpvisr-engine` — all existing tracker/metrics/timeline tests still green (seq collection is off by default, so `conns`/`metrics` paths are unaffected).

- [ ] **Step 10: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-engine
git add crates/tcpvisr-engine/src/tracker.rs
git commit -m "feat(engine): collect unwrapped SeqSample series for replay"
```

---

### Task 5: TUI — pure `SeqPlot` projection (`detail.rs`)

**Files:**
- Create: `crates/tcpvisr-tui/src/detail.rs`
- Modify: `crates/tcpvisr-tui/src/lib.rs` (add `pub mod detail;`)
- Test: inline in `detail.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `SeqSample`, `SeqKind` (engine), `SampleDir`, `Nanos` (core).
- Produces:
  - `pub struct Mark { pub col: u16, pub row: u16, pub glyph: char }` (row 0 = bottom).
  - `pub struct SeqPlot { pub width: u16, pub height: u16, pub max_rel: i64, pub x_span: (Nanos, Nanos), pub cursor_col: u16, pub marks: Vec<Mark> }`.
  - `pub const MIN_W: u16 = 8; pub const MIN_H: u16 = 3;`
  - glyph consts `DATA_GLYPH`, `OOO_GLYPH`, `RETRANS_GLYPH`, `SACK_GLYPH`, `CURSOR_GLYPH`.
  - `pub fn project(series: &[SeqSample], focus: SampleDir, x_span: (Nanos, Nanos), cursor: Nanos, width: u16, height: u16) -> Option<SeqPlot>` (None when below the minimum rectangle).

- [ ] **Step 1: Register the module + write the failing tests** — first add `pub mod detail;` to `crates/tcpvisr-tui/src/lib.rs` (alongside the other `pub mod` lines) so the new file is compiled. Then create `detail.rs` containing **only** this `#[cfg(test)]` module (the projection code is added in Step 3); because it references `project`/`SeqPlot`/the glyph consts that do not exist yet, the crate will fail to compile — the intended red state. Put this at the bottom of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tcpvisr_core::SampleDir;

    fn data(t: u64, rel: i64, len: u32) -> SeqSample {
        SeqSample {
            t: Nanos(t),
            dir: SampleDir::OriginToResponder,
            rel,
            len,
            kind: SeqKind::Data { retransmit: false, out_of_order: false },
        }
    }

    fn kind(t: u64, rel: i64, k: SeqKind) -> SeqSample {
        SeqSample { t: Nanos(t), dir: SampleDir::OriginToResponder, rel, len: 0, kind: k }
    }

    fn glyph_at(p: &SeqPlot, col: u16, row: u16) -> Option<char> {
        p.marks.iter().find(|m| m.col == col && m.row == row).map(|m| m.glyph)
    }

    #[test]
    fn too_small_viewport_yields_none() {
        let s = [data(0, 0, 10)];
        assert!(project(&s, SampleDir::OriginToResponder, (Nanos(0), Nanos(10)), Nanos(0), MIN_W - 1, MIN_H).is_none());
        assert!(project(&s, SampleDir::OriginToResponder, (Nanos(0), Nanos(10)), Nanos(0), MIN_W, MIN_H - 1).is_none());
    }

    #[test]
    fn corners_place_at_exact_indices() {
        // one point at (opened_at, rel 0), one at (effective_end, rel = max_rel via len).
        let s = [data(0, 0, 0), data(100, 40, 0)]; // max_rel = 40
        let p = project(&s, SampleDir::OriginToResponder, (Nanos(0), Nanos(100)), Nanos(100), 10, 5).expect("plot");
        assert_eq!(p.max_rel, 40);
        assert_eq!(glyph_at(&p, 0, 0), Some(DATA_GLYPH), "bottom-left");
        assert_eq!(glyph_at(&p, 9, 4), Some(DATA_GLYPH), "top-right: col W-1, row H-1");
    }

    #[test]
    fn wrap_rel_places_without_folding() {
        // Engine already produced rel 0 and 301; max_rel = 301 + 50 = 351.
        let s = [data(0, 0, 50), data(100, 301, 50)];
        let p = project(&s, SampleDir::OriginToResponder, (Nanos(0), Nanos(100)), Nanos(100), 20, 10).expect("plot");
        assert_eq!(p.max_rel, 351);
        // rel 301 of 351 over height 10 -> row (301*9)/351 = 7.
        assert!(glyph_at(&p, 19, 7).is_some(), "second point near the top, not folded low");
    }

    #[test]
    fn reveal_hides_marks_after_cursor() {
        let s = [data(0, 0, 10), data(10, 10, 10), data(20, 20, 10)];
        let span = (Nanos(0), Nanos(20));
        let early = project(&s, SampleDir::OriginToResponder, span, Nanos(10), 20, 10).expect("plot");
        let n_early = early.marks.iter().filter(|m| m.glyph == DATA_GLYPH).count();
        assert_eq!(n_early, 2, "t=0 and t=10 revealed, t=20 hidden");
        let all = project(&s, SampleDir::OriginToResponder, span, Nanos(20), 20, 10).expect("plot");
        let n_all = all.marks.iter().filter(|m| m.glyph == DATA_GLYPH).count();
        assert_eq!(n_all, 3);
    }

    #[test]
    fn axes_are_fixed_regardless_of_cursor() {
        let s = [data(0, 0, 10), data(100, 90, 10)];
        let span = (Nanos(0), Nanos(100));
        let a = project(&s, SampleDir::OriginToResponder, span, Nanos(0), 20, 10).expect("plot");
        let b = project(&s, SampleDir::OriginToResponder, span, Nanos(100), 20, 10).expect("plot");
        assert_eq!((a.max_rel, a.x_span), (b.max_rel, b.x_span));
        assert_eq!(a.max_rel, 100);
    }

    #[test]
    fn bucketing_prefers_the_salient_glyph() {
        // A plain data point and a retransmit in the same cell -> retransmit wins.
        let s = [
            data(0, 0, 0),
            kind(0, 0, SeqKind::Data { retransmit: true, out_of_order: false }),
        ];
        let p = project(&s, SampleDir::OriginToResponder, (Nanos(0), Nanos(10)), Nanos(10), 10, 5).expect("plot");
        assert_eq!(glyph_at(&p, 0, 0), Some(RETRANS_GLYPH));
        // A data point and a SACK in one cell -> SACK wins over plain data.
        let s2 = [data(0, 0, 0), kind(0, 0, SeqKind::Sack)];
        let p2 = project(&s2, SampleDir::OriginToResponder, (Nanos(0), Nanos(10)), Nanos(10), 10, 5).expect("plot");
        assert_eq!(glyph_at(&p2, 0, 0), Some(SACK_GLYPH));
    }

    #[test]
    fn degenerate_spans_do_not_divide_by_zero() {
        // Single data segment: zero-width time span, one sample.
        let s = [data(50, 0, 10)];
        let p = project(&s, SampleDir::OriginToResponder, (Nanos(50), Nanos(50)), Nanos(50), 10, 5).expect("plot");
        assert_eq!(glyph_at(&p, 0, 0), Some(DATA_GLYPH));
        // Only a SACK at the baseline: max_rel == 0 -> row 0.
        let s2 = [kind(0, 0, SeqKind::Sack)];
        let p2 = project(&s2, SampleDir::OriginToResponder, (Nanos(0), Nanos(10)), Nanos(10), 10, 5).expect("plot");
        assert_eq!(p2.max_rel, 0);
        assert_eq!(glyph_at(&p2, 0, 0), Some(SACK_GLYPH));
    }

    #[test]
    fn cursor_column_drawn_where_empty() {
        let s = [data(0, 0, 10)]; // occupies col 0
        let p = project(&s, SampleDir::OriginToResponder, (Nanos(0), Nanos(100)), Nanos(50), 11, 5).expect("plot");
        // cursor at t=50 of [0,100] over width 11 -> col (50*10)/100 = 5.
        assert_eq!(p.cursor_col, 5);
        assert_eq!(glyph_at(&p, 5, 0), Some(CURSOR_GLYPH), "cursor fills its empty column");
        assert_eq!(glyph_at(&p, 0, 0), Some(DATA_GLYPH), "data cell not overwritten by cursor");
    }

    #[test]
    fn only_focus_direction_is_plotted() {
        let mut r2o = data(0, 0, 10);
        r2o.dir = SampleDir::ResponderToOrigin;
        let s = [data(0, 0, 10), r2o];
        let p = project(&s, SampleDir::OriginToResponder, (Nanos(0), Nanos(10)), Nanos(10), 10, 5).expect("plot");
        let data_marks = p.marks.iter().filter(|m| m.glyph == DATA_GLYPH).count();
        assert_eq!(data_marks, 1, "only the O2R data point");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p tcpvisr-tui --lib detail`
Expected: FAIL — the crate does not compile: `cannot find function \`project\` in this scope` and `cannot find type \`SeqPlot\`/\`Mark\`` (the module is registered from Step 1, so the compiler actually reaches and rejects the test references).

- [ ] **Step 3: Implement the projection** — put this at the top of `detail.rs` (above the test module):

```rust
//! Pure Time/Sequence (Stevens) projection (ADR-0011 §2–§3): maps a connection's `SeqSample`
//! series + cursor + plot-rectangle cells to a grid of glyph marks. No terminal, no I/O, no
//! serial arithmetic (the engine already unwrapped each point to an `i64` `rel`).

use tcpvisr_core::{Nanos, SampleDir};
use tcpvisr_engine::{SeqKind, SeqSample};

/// Minimum inner plot rectangle; below this the detail pane shows "widen terminal".
pub const MIN_W: u16 = 8;
pub const MIN_H: u16 = 3;

pub const DATA_GLYPH: char = '#';
pub const OOO_GLYPH: char = 'o';
pub const RETRANS_GLYPH: char = '·';
pub const SACK_GLYPH: char = '╎';
pub const CURSOR_GLYPH: char = '┊';

/// One plotted cell. `row` is bottom-origin (0 = sequence 0); a top-down renderer draws it at
/// screen line `height - 1 - row`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub col: u16,
    pub row: u16,
    pub glyph: char,
}

/// A resolved Time/Sequence plot over a `W x H` cell rectangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeqPlot {
    pub width: u16,
    pub height: u16,
    pub max_rel: i64,
    pub x_span: (Nanos, Nanos),
    pub cursor_col: u16,
    pub marks: Vec<Mark>,
}

/// Salience priority (higher wins a shared cell) and the glyph for a kind.
fn kind_glyph(k: SeqKind) -> (u8, char) {
    match k {
        SeqKind::Data { retransmit: true, out_of_order: true } => (3, RETRANS_GLYPH),
        SeqKind::Data { retransmit: true, out_of_order: false } => (3, RETRANS_GLYPH),
        SeqKind::Sack => (2, SACK_GLYPH),
        SeqKind::Data { retransmit: false, out_of_order: true } => (1, OOO_GLYPH),
        SeqKind::Data { retransmit: false, out_of_order: false } => (0, DATA_GLYPH),
    }
}

/// Maps a nanosecond time to a column in `0..width`, clamped; zero-width span -> column 0.
fn col_of(t: u64, t0: u64, span_t: u64, width: u16) -> u16 {
    if span_t == 0 {
        return 0;
    }
    let c = u128::from(t.saturating_sub(t0)) * u128::from(width - 1) / u128::from(span_t);
    u16::try_from(c).unwrap_or(width - 1).min(width - 1)
}

/// Maps a non-negative relative sequence to a bottom-origin row in `0..height`, clamped;
/// `max_rel == 0` -> row 0.
fn row_of(y: i64, max_rel: i64, height: u16) -> u16 {
    if max_rel <= 0 {
        return 0;
    }
    let r = i128::from(y) * i128::from(height - 1) / i128::from(max_rel);
    u16::try_from(r).unwrap_or(height - 1).min(height - 1)
}

/// Projects `series` (only `focus`-direction samples with `t <= cursor` are revealed) onto a
/// `width x height` cell grid. Axes are fixed to `x_span` and to `[0, max_rel]` over the focus
/// direction's full sample set. Returns `None` if the rectangle is below the minimum.
#[must_use]
pub fn project(
    series: &[SeqSample],
    focus: SampleDir,
    x_span: (Nanos, Nanos),
    cursor: Nanos,
    width: u16,
    height: u16,
) -> Option<SeqPlot> {
    if width < MIN_W || height < MIN_H {
        return None;
    }
    let (t0, t1) = (x_span.0.0, x_span.1.0);
    let span_t = t1.saturating_sub(t0);
    let base = series
        .iter()
        .filter(|s| s.dir == focus)
        .map(|s| s.rel)
        .min()
        .unwrap_or(0);
    let max_rel = series
        .iter()
        .filter(|s| s.dir == focus)
        .map(|s| (s.rel - base) + i64::from(s.len))
        .max()
        .unwrap_or(0);

    let cells = usize::from(width) * usize::from(height);
    let mut grid: Vec<Option<(u8, char)>> = vec![None; cells];
    let idx = |col: u16, row: u16| usize::from(row) * usize::from(width) + usize::from(col);

    for s in series.iter().filter(|s| s.dir == focus && s.t.0 <= cursor.0) {
        let col = col_of(s.t.0, t0, span_t, width);
        let row = row_of(s.rel - base, max_rel, height);
        let (prio, glyph) = kind_glyph(s.kind);
        let cell = &mut grid[idx(col, row)];
        match cell {
            Some((p, _)) if *p >= prio => {}
            _ => *cell = Some((prio, glyph)),
        }
    }

    let ct = cursor.0.clamp(t0, t1);
    let cursor_col = col_of(ct, t0, span_t, width);
    for row in 0..height {
        let cell = &mut grid[idx(cursor_col, row)];
        if cell.is_none() {
            *cell = Some((0, CURSOR_GLYPH));
        }
    }

    let mut marks = Vec::new();
    for row in 0..height {
        for col in 0..width {
            if let Some((_, glyph)) = grid[idx(col, row)] {
                marks.push(Mark { col, row, glyph });
            }
        }
    }
    Some(SeqPlot { width, height, max_rel, x_span, cursor_col, marks })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p tcpvisr-tui --lib detail`
Expected: PASS (all projection tests). (`pub mod detail;` was already added in Step 1.)

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-tui
git add crates/tcpvisr-tui/src/detail.rs crates/tcpvisr-tui/src/lib.rs
git commit -m "feat(tui): pure Time/Sequence plot projection"
```

---

### Task 6: TUI — `App` detail state + `focus()`

**Files:**
- Modify: `crates/tcpvisr-tui/src/app.rs`
- Test: inline in `app.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `Timeline::seq_series`/`x_span`/`connections` (Task 2), `SeqSample`, `SampleDir`.
- Produces on `App`: `detail_open` state via `open_detail(&mut self)` (opens only if a row is selected), `close_detail(&mut self)`, `#[must_use] is_detail_open(&self) -> bool`, and `#[must_use] focus(&self) -> Option<FocusConn<'_>>`.
  `pub struct FocusConn<'a> { pub origin: Endpoint, pub responder: Endpoint, pub x_span: (Nanos, Nanos), pub focus_dir: SampleDir, pub series: &'a [SeqSample] }`.

- [ ] **Step 1: Write the failing tests** — append to `app.rs` `tests`:

```rust
    #[test]
    fn enter_opens_only_with_a_selection_esc_closes() {
        let mut app = app_of(vec![entry(ep(1, 51324), ep(2, 443), 10, 20, 0)]);
        assert!(!app.is_detail_open());
        app.open_detail();
        assert!(app.is_detail_open(), "opens when a row is selected");
        app.close_detail();
        assert!(!app.is_detail_open());

        let mut empty = app_of(vec![]);
        empty.open_detail();
        assert!(!empty.is_detail_open(), "no selection -> stays closed");
    }

    #[test]
    fn focus_resolves_selected_connection_and_higher_byte_direction() {
        // O2R 5 bytes, R2O 500 bytes -> focus direction is R2O.
        let mut down = full_conn(ep(1, 1), ep(2, 443), 0, 0, 10, ConnState::Established);
        down.bytes_o2r = 5;
        down.bytes_r2o = 500;
        let samples = vec![ss(0, ConnState::Established, 5, 500)];
        let app = App::new(Timeline::new(vec![(down, samples)]), "t".to_string());
        let f = app.focus().expect("selected connection resolves");
        assert_eq!(f.origin, ep(1, 1));
        assert_eq!(f.responder, ep(2, 443));
        assert_eq!(f.x_span, (Nanos(0), Nanos(10)));
        assert_eq!(f.focus_dir, SampleDir::ResponderToOrigin);
    }

    #[test]
    fn detail_follows_selection() {
        let a = entry(ep(1, 1), ep(2, 22), 0, 0, 0); // peer 10.0.0.2
        let b = entry(ep(1, 2), ep(3, 22), 0, 0, 0); // peer 10.0.0.3
        let mut app = app_of(vec![a, b]);
        app.open_detail();
        let first = app.focus().expect("focus").responder;
        app.move_down();
        let second = app.focus().expect("focus").responder;
        assert_ne!(first, second, "focus follows the moved selection");
        assert_eq!(second, ep(3, 22));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-tui --lib app::tests::enter_opens`
Expected: FAIL — no `open_detail`/`is_detail_open`/`focus`.

- [ ] **Step 3: Add imports + the `FocusConn` type** — in `app.rs`, change
`use tcpvisr_core::{Endpoint, Nanos};` to `use tcpvisr_core::{Endpoint, Nanos, SampleDir};`
and `use tcpvisr_engine::{AsOf, ConnId, ConnState, Timeline};` to
`use tcpvisr_engine::{AsOf, ConnId, ConnState, SeqSample, Timeline};`. Add near the top-level types:

```rust
/// The selected connection projected for the detail pane: its endpoints, X span, focus
/// direction (higher-byte), and its `SeqSample` series (borrowed from the `Timeline`).
#[derive(Debug)]
pub struct FocusConn<'a> {
    pub origin: Endpoint,
    pub responder: Endpoint,
    pub x_span: (Nanos, Nanos),
    pub focus_dir: SampleDir,
    pub series: &'a [SeqSample],
}
```

- [ ] **Step 4: Add the field + methods** — add `detail_open: bool,` to `struct App`, and set
`detail_open: false,` in the `Self { ... }` literal inside `new` (before `app.selected = ...`).
Then add these methods in `impl App` (near `toggle_play`):

```rust
    /// Opens the detail pane for the selected row (no-op when nothing is selected).
    pub fn open_detail(&mut self) {
        if self.selected.is_some() {
            self.detail_open = true;
        }
    }

    /// Closes the detail pane.
    pub fn close_detail(&mut self) {
        self.detail_open = false;
    }

    /// Whether the detail pane is open.
    #[must_use]
    pub fn is_detail_open(&self) -> bool {
        self.detail_open
    }

    /// The selected connection projected for the detail pane, or `None` if nothing is selected
    /// or active at the cursor. The focus direction is the higher-byte direction (tie -> O2R).
    #[must_use]
    pub fn focus(&self) -> Option<FocusConn<'_>> {
        let id = self.selected?;
        let c = self.timeline.connections().find(|c| c.id == id)?;
        let x_span = self.timeline.x_span(id)?;
        let focus_dir = if c.bytes_o2r >= c.bytes_r2o {
            SampleDir::OriginToResponder
        } else {
            SampleDir::ResponderToOrigin
        };
        Some(FocusConn {
            origin: c.origin,
            responder: c.responder,
            x_span,
            focus_dir,
            series: self.timeline.seq_series(id),
        })
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p tcpvisr-tui --lib app::`
Expected: PASS (new + existing app tests).

- [ ] **Step 6: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-tui
git add crates/tcpvisr-tui/src/app.rs
git commit -m "feat(tui): App detail open/close state and focus() accessor"
```

---

### Task 7: TUI — `Enter`/`Esc` key handling

**Files:**
- Modify: `crates/tcpvisr-tui/src/keys.rs`
- Test: inline in `keys.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `App::open_detail`/`close_detail`/`is_detail_open` (Task 6).

- [ ] **Step 1: Write the failing tests** — append to `keys.rs` `tests`:

```rust
    #[test]
    fn enter_opens_and_esc_closes_detail_in_nav_mode() {
        let mut a = app();
        handle_key(&mut a, key(KeyCode::Enter));
        assert!(a.is_detail_open());
        handle_key(&mut a, key(KeyCode::Esc));
        assert!(!a.is_detail_open());
    }

    #[test]
    fn filter_mode_enter_and_esc_do_not_touch_detail() {
        let mut a = app();
        handle_key(&mut a, press('/')); // enter filter mode
        handle_key(&mut a, press('s'));
        handle_key(&mut a, key(KeyCode::Enter)); // confirms filter, does not open detail
        assert!(!a.is_detail_open());
        assert_eq!(a.mode(), Mode::Nav);
        handle_key(&mut a, press('/'));
        handle_key(&mut a, key(KeyCode::Esc)); // clears filter, does not close/open detail
        assert!(!a.is_detail_open());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-tui --lib keys::tests::enter_opens`
Expected: FAIL — `Enter`/`Esc` currently ignored in nav mode.

- [ ] **Step 3: Map the keys** — in `handle_nav`, add before `_ => {}`:

```rust
        KeyCode::Enter => app.open_detail(),
        KeyCode::Esc => app.close_detail(),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p tcpvisr-tui --lib keys::`
Expected: PASS (new + existing; filter-mode `Enter`/`Esc` unchanged because `handle_filter` is dispatched first for `Mode::Filter`).

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-tui
git add crates/tcpvisr-tui/src/keys.rs
git commit -m "feat(tui): Enter opens and Esc closes the detail pane"
```

---

### Task 8: TUI — split layout + `render_detail`

**Files:**
- Modify: `crates/tcpvisr-tui/src/render.rs`
- Test: inline in `render.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `App::is_detail_open`/`focus` (Task 6), `detail::{project, SeqPlot, Mark, MIN_W, MIN_H, RETRANS_GLYPH, SACK_GLYPH}` (Task 5), the existing `fmt_seconds`.

- [ ] **Step 1: Write the failing tests** — append to `render.rs` `tests`. (Reuses the file's existing `draw`, `app_span`, `handle_key`, `key` helpers.)

```rust
    #[test]
    fn detail_closed_still_renders_full_master() {
        let app = app_span(2_000_000_000);
        let s = draw(&app, 100, 10);
        assert!(s.contains("PEER"), "master header present when detail closed: {s}");
        assert!(!s.contains("DETAIL"), "no detail pane when closed");
    }

    #[test]
    fn detail_open_shows_title_legend_and_a_mark() {
        // A connection with one O2R data segment so the focus series is non-empty.
        let c = conn_span(ep(1, 5), ep(2, 443), 0, 1_000, ConnState::Established);
        let sq = tcpvisr_engine::SeqSample {
            t: Nanos(0),
            dir: tcpvisr_core::SampleDir::OriginToResponder,
            rel: 0,
            len: 100,
            kind: tcpvisr_engine::SeqKind::Data { retransmit: false, out_of_order: false },
        };
        let mut c2 = c;
        c2.bytes_o2r = 100;
        let tl = Timeline::with_seq(vec![(c2, vec![ss(0, 100, 0)], vec![sq])]);
        let mut app = App::new(tl, "t".to_string());
        app.open_detail();
        let s = draw(&app, 120, 14);
        assert!(s.contains("DETAIL"), "detail title: {s}");
        assert!(s.contains("retrans") && s.contains("sack"), "mark legend: {s}");
        assert!(s.contains('#'), "at least one plotted data glyph: {s}");
    }

    #[test]
    fn detail_pane_too_narrow_shows_widen_message() {
        let c = conn_span(ep(1, 5), ep(2, 443), 0, 1_000, ConnState::Established);
        let mut c2 = c;
        c2.bytes_o2r = 100;
        let sq = tcpvisr_engine::SeqSample {
            t: Nanos(0),
            dir: tcpvisr_core::SampleDir::OriginToResponder,
            rel: 0,
            len: 100,
            kind: tcpvisr_engine::SeqKind::Data { retransmit: false, out_of_order: false },
        };
        let tl = Timeline::with_seq(vec![(c2, vec![ss(0, 100, 0)], vec![sq])]);
        let mut app = App::new(tl, "t".to_string());
        app.open_detail();
        // Total width 30 -> right pane ~15 inner, minus gutter/borders -> below MIN_W.
        let s = draw(&app, 30, 12);
        assert!(s.contains("widen terminal"), "narrow detail guidance: {s}");
    }

    #[test]
    fn footer_advertises_open_and_close() {
        let app = app_span(1_000_000_000);
        let s = draw(&app, 120, 8);
        assert!(s.contains("open"), "open hint: {s}");
        assert!(s.contains("close"), "close hint: {s}");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcpvisr-tui --lib render::tests::detail_open_shows`
Expected: FAIL — no detail rendering / footer hints.

- [ ] **Step 3: Add imports** — at the top of `render.rs`, add:

```rust
use ratatui::style::Color;

use crate::detail::{self, Mark, SeqPlot};
```

(Keep the existing imports; `Color` augments the existing `use ratatui::style::{Modifier, Style};` — either add this line or extend that one to `{Color, Modifier, Style}`. Do **not** import `FocusConn` — `render_detail` binds `app.focus()` without naming the type, so importing it would be an unused import that fails `-D warnings`.)

- [ ] **Step 4: Split the layout in `render`** — replace the body of `pub fn render`:

```rust
pub fn render(frame: &mut Frame, app: &App) {
    let [main, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    if app.is_detail_open() {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(main);
        render_main(frame, app, left);
        render_detail(frame, app, right);
    } else {
        render_main(frame, app, main);
    }
    render_footer(frame, app, footer);
}
```

- [ ] **Step 5: Add `render_detail` and its helpers** — add these functions to `render.rs`:

```rust
/// Draws the Time/Sequence detail pane for the focused connection into `area`.
fn render_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(focus) = app.focus() else {
        let block = Block::bordered().title("DETAIL");
        frame.render_widget(Paragraph::new("no connection selected").block(block), area);
        return;
    };
    let title = format!("DETAIL {} \u{2192} {}", focus.origin, focus.responder);
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Reserve: legend row (top), time-label row (bottom), Y-label gutter (left).
    const GUTTER: u16 = 8;
    if inner.height < 3 || inner.width <= GUTTER {
        frame.render_widget(Paragraph::new("widen terminal to view graph"), inner);
        return;
    }
    let plot_w = inner.width - GUTTER;
    let plot_h = inner.height - 2; // legend + time labels

    let Some(plot) = detail::project(
        focus.series,
        focus.focus_dir,
        focus.x_span,
        app.cursor(),
        plot_w,
        plot_h,
    ) else {
        frame.render_widget(Paragraph::new("widen terminal to view graph"), inner);
        return;
    };

    draw_legend(frame, inner);
    draw_plot(frame, inner, GUTTER, &plot);
    draw_axes(frame, inner, GUTTER, &plot);
}

fn draw_legend(frame: &mut Frame, inner: Rect) {
    let legend = format!(
        "Time/Sequence   {} retrans  {} sack",
        detail::RETRANS_GLYPH,
        detail::SACK_GLYPH
    );
    let row = Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 };
    frame.render_widget(Paragraph::new(legend), row);
}

fn draw_plot(frame: &mut Frame, inner: Rect, gutter: u16, plot: &SeqPlot) {
    let buf = frame.buffer_mut();
    let x0 = inner.x + gutter;
    let y_top = inner.y + 1; // below the legend row
    for &Mark { col, row, glyph } in &plot.marks {
        // bottom-origin row -> screen line
        let screen_row = plot.height - 1 - row;
        let x = x0 + col;
        let y = y_top + screen_row;
        let color = match glyph {
            detail::RETRANS_GLYPH => Color::Red,
            detail::SACK_GLYPH => Color::Yellow,
            _ => Color::Reset,
        };
        buf.set_string(x, y, glyph.to_string(), Style::default().fg(color));
    }
}

fn draw_axes(frame: &mut Frame, inner: Rect, gutter: u16, plot: &SeqPlot) {
    let buf = frame.buffer_mut();
    let y_top = inner.y + 1;
    // Y labels: max_rel at the top of the plot, 0 at the bottom.
    buf.set_string(inner.x, y_top, format!("{:>7}", plot.max_rel), Style::default());
    let y_bottom = y_top + plot.height - 1;
    buf.set_string(inner.x, y_bottom, format!("{:>7}", 0), Style::default());
    // X labels: start / end seconds on the bottom label row.
    let label_row = inner.y + inner.height - 1;
    let start = fmt_seconds(plot.x_span.0);
    let end = fmt_seconds(plot.x_span.1);
    buf.set_string(inner.x + gutter, label_row, format!("{start}s"), Style::default());
    let end_label = format!("{end}s");
    let end_x = inner
        .x
        .saturating_add(inner.width)
        .saturating_sub(u16::try_from(end_label.len()).unwrap_or(0));
    buf.set_string(end_x, label_row, end_label, Style::default());
}
```

- [ ] **Step 6: Extend the footer** — in `render_footer`, in the `Mode::Nav` arm, change the format string to include the open/close hints:

```rust
            format!(
                "space play/pause  ←→ seek  +/- speed  ,/. step  ⏎ open  esc close  / filter  s sort:{}{arrow}  q quit",
                sort_label(app.sort_field()),
            )
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p tcpvisr-tui --lib render::`
Expected: PASS. Confirm existing M5 render tests (`renders_header_columns_selection_and_footer`, `header_shows_transport_status`, etc.) still pass — they render with the detail closed, so the layout is unchanged.

- [ ] **Step 8: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-tui
git add crates/tcpvisr-tui/src/render.rs
git commit -m "feat(tui): render the Time/Sequence detail pane"
```

---

### Task 9: CLI — collect the seq timeline on the replay path

**Files:**
- Modify: `crates/tcp-visr/src/main.rs` (`run_replay` cfg + a test in the in-crate `#[cfg(test)] mod build_replay_tests`)

**Interfaces:**
- Consumes: `collect_seq_timeline` (Task 3), the existing private `build_replay_app(&Path, EngineConfig)` (main.rs), `App::focus` (Task 6).

> **Where the test lives.** `crates/tcp-visr/tests/replay.rs` is a **black-box** integration
> crate: its tests only run the compiled binary via `Command::new(env!("CARGO_BIN_EXE_tcp-visr"))`
> and inspect stdout/stderr/exit — they cannot call `build_replay_app` or `App::focus()`
> in-process (a binary crate exports no library API). Criterion 17 drives the `build_replay_app`
> seam directly, so its test belongs in `main.rs`'s existing `#[cfg(test)] mod build_replay_tests`
> (which already has a `fixture()` helper and access to `build_replay_app`), next to the M5 seam
> tests `builds_a_timeline_app_with_rows` / `sample_ceiling_is_fatal`. Do **not** edit
> `tests/replay.rs`.

- [ ] **Step 1: Write the seam test** — append to `mod build_replay_tests` in `main.rs`:

```rust
    #[test]
    fn build_replay_app_collects_seq_series_for_the_focus_connection() {
        let cfg = EngineConfig {
            collect_state_timeline: true,
            collect_seq_timeline: true,
            ..EngineConfig::default()
        };
        let app = build_replay_app(&fixture(), cfg).expect("build");
        let focus = app.focus().expect("a connection is selected at the initial cursor");
        assert!(
            !focus.series.is_empty(),
            "fixture with data segments yields a non-empty focus seq series"
        );
    }
```

(`build_replay_tests` uses `use super::*;`, so `EngineConfig`, `build_replay_app`, and `fixture()`
are all in scope. If the fixture's peer-first-sorted connection happens to carry no data in its
focus direction, `app.move_down()` to a data-bearing row before asserting, or assert over
`app.visible()` — but `metrics_basic.pcap` is a data-carrying fixture, so the initial selection
suffices.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tcp-visr --lib build_replay_app_collects_seq_series`
Expected: FAIL — `focus.series` is empty because `run_replay` has not yet enabled
`collect_seq_timeline`. **Wait:** the test passes its *own* cfg to `build_replay_app`, so it
actually exercises Tasks 1–6 end-to-end and will already pass once those are merged. If it passes
here, that is expected — it is the criterion-17 guard for the engine→build→focus path; treat
Step 3 as the production-wiring change and keep this test as the regression guard.

- [ ] **Step 3: Turn on seq collection in `run_replay`** — in `main.rs` `run_replay`, extend the cfg (this is the production wiring the interactive `replay` uses; the black-box `tests/replay.rs` tty/validation tests are unchanged by it):

```rust
    let cfg = EngineConfig {
        collect_state_timeline: true,
        collect_seq_timeline: true,
        max_samples,
        ..EngineConfig::default()
    };
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p tcp-visr`
Expected: PASS — the new seam test, the M5 seam tests (`sample_ceiling_is_fatal` still trips
because seq samples count against `max_samples`), and the unchanged black-box `tests/replay.rs`.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcp-visr
git add crates/tcp-visr/src/main.rs
git commit -m "feat(cli): collect the seq timeline on the replay path"
```

---

### Task 10: Docs — mark M6 implemented

**Files:**
- Modify: `CLAUDE.md` (the "Current state" paragraph)

- [ ] **Step 1: Update the current-state paragraph** — in `CLAUDE.md`, change the milestone
line to note M6 and the detail view. Replace `milestones M0–M5 are implemented` with
`milestones M0–M6 are implemented`, and extend the state description with:

> `replay` now also opens a per-connection Time/Sequence (Stevens) detail pane (`Enter` on a
> row, `Esc` to close): a cursor-driven seq-vs-time graph with retransmit/SACK marks (M6). The
> remaining detail views (in-flight, RTT, throughput) and the `Tab` view-switcher (M7–M9),
> `live`, and kernel enrichment are not built yet.

(Keep the existing "Do not assume a feature exists…" sentence.)

- [ ] **Step 2: Run the full workspace guardrails** (markdown is not linted, but run the suite before committing docs at the end of the milestone):

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace
```
Expected: all green.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: mark M6 (Time/Sequence detail) implemented"
```

---

## Self-review checklist (run before handing off to review)

1. **Spec coverage** — map each success criterion to a task:
   - 1 (data point) → Task 4 `data_points_carry_unwrapped_rel_and_len`; 2 (retransmit/OOO) → Task 4 `retransmit_and_ooo_classified_on_seq_points`; 3 (SACK dir) → Task 4 `sack_point_lands_in_the_acked_direction_frame`; 4 (timeline carries) → Task 2 + Task 4 `only_id`; 5 (ceiling) → Task 4 `seq_collection_counts_against_the_ceiling`; 6/6a (wrap) → Task 4 `rel_unwraps_across_a_u32_wrap` / `rel_rises_monotonically_across_multiple_wraps` + Task 5 `wrap_rel_places_without_folding`; 7 (reveal) → Task 5 `reveal_hides_marks_after_cursor`; 8 (fixed axes) → Task 5 `axes_are_fixed_regardless_of_cursor`; 9 (bucketing) → Task 5 `bucketing_prefers_the_salient_glyph`; 10/10a (placement/degenerate) → Task 5 `corners_place_at_exact_indices` / `degenerate_spans_do_not_divide_by_zero`; 11 (cursor col) → Task 5 `cursor_column_drawn_where_empty`; 12 (narrow) → Task 5 `too_small_viewport_yields_none` + Task 8 `detail_pane_too_narrow_shows_widen_message`; 13 (Enter/Esc) → Task 6 `enter_opens_only_with_a_selection_esc_closes` + Task 7; 14 (follows selection) → Task 6 `detail_follows_selection`; 15 (closed unchanged) → Task 8 `detail_closed_still_renders_full_master` + existing M5 tests; 16 (open shows graph) → Task 8 `detail_open_shows_title_legend_and_a_mark`; 17 (CLI wiring) → Task 9.
2. **Placeholder scan** — no TBD/`handle edge cases`/"similar to Task N"; every code step shows the code.
3. **Type consistency** — `SeqSample`/`SeqKind` fields (`t`,`dir`,`rel`,`len`,`kind`) identical across Tasks 1/4/5/6/8; `project(series, focus, x_span, cursor, width, height)` argument order identical in Task 5 and Task 8; `Mark { col, row, glyph }` and glyph consts identical; `FocusConn` fields identical in Tasks 6 and 8; `Timeline::with_seq` triple shape identical in Tasks 2/4.

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-30-m6-detail-time-sequence.md`. Two execution options:

1. **Subagent-Driven (recommended)** — a fresh implementer subagent per task with two-stage review (spec compliance, then code quality) between tasks.
2. **Inline Execution** — execute the tasks in this session with checkpoints.
