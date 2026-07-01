# M4 TUI Master List Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the M4 master-list TUI — `tcp-visr replay <file>` opens an interactive table of a capture's connections with sort, `/` filter, selection, and port→service labels.

**Architecture:** `tcpvisr-tui` gains a pure `App` state (rows, sort field/direction, filter query, mode, `ConnId`-tracked selection), a pure `handle_key(&mut App, KeyEvent) -> Outcome` decision point, a pure `render(&mut Frame, &App)`, and a thin impure `run(App)` event loop over `ratatui::DefaultTerminal`. The bin collects `Vec<Connection>` (same path as `conns`), guards on an interactive terminal, and hands off to `run`. See the spec `docs/superpowers/specs/m4-tui-master-list.md` and ADR-0009.

**Tech Stack:** Rust 1.88, ratatui 0.30.2 (crossterm re-exported as `ratatui::crossterm`, default backend), `std::io::IsTerminal`.

## Global Constraints

- Toolchain pinned to Rust 1.88.0; edition 2024.
- Dependencies pinned exactly (`=x.y.z`); add each new crate's SPDX license id to `deny.toml`'s allow-list (an unused allow entry warns — only add ids actually pulled in).
- Zero warnings: `cargo clippy --all-targets --all-features -- -D warnings` must pass. Workspace clippy denies `unwrap_used`, `panic`, `print_stdout`, `expect_used` (warn), `allow_attributes` (so no item-level `#[allow]`; scope test relaxations with a file-level `#![allow(...)]`).
- Guardrail commands (run before every commit; all must be green):
  - `cargo fmt --all --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test --workspace`
  - `cargo test -p tcpvisr-ingest --features live`
  - `cargo deny check`
- Focused runs: `cargo test -p tcpvisr-tui <filter>`, `cargo test -p tcp-visr --test <name>`.
- Absolute imports only (no `..` paths). Google-style docstrings on non-trivial public APIs. ≤100 lines/function, complexity ≤8, ≤100-char lines.
- Commit style: Conventional Commits, imperative, ≤72-char subject, one logical change per commit, trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Engine/core types consumed (verified in-tree):
  - `tcpvisr_engine::{Connection, ConnId, ConnState}`; `Connection { id: ConnId, state: ConnState, origin: Endpoint, responder: Endpoint, origin_inferred: bool, opened_at, last_at, bytes_o2r: u64, bytes_r2o: u64, segments: u64 }`.
  - `ConnId { pair: EndpointPair, instance: u32 }`, `EndpointPair { low: Endpoint, high: Endpoint }` — all `Copy + Eq + Hash`, `EndpointPair: Ord`.
  - `ConnState` enum: `SynSent, SynReceived, Established, FinWait, Closed, Reset` (`Copy + Eq`, `Debug`).
  - `tcpvisr_core::Endpoint { ip: IpAddr, port: u16 }` — `Copy + Ord + Display` (`Display` → `ip:port`, v6 bracketed).

---

## File structure

- `crates/tcpvisr-tui/Cargo.toml` — add `ratatui = "=0.30.2"`, `tcpvisr-core`, `tcpvisr-engine` deps.
- `crates/tcpvisr-tui/src/lib.rs` — module declarations + re-exports.
- `crates/tcpvisr-tui/src/service.rs` — `service_name(port) -> Option<&'static str>`.
- `crates/tcpvisr-tui/src/app.rs` — `App`, `ConnRow`, `SortField`, `SortDir`, `Mode`, `Outcome`, state methods.
- `crates/tcpvisr-tui/src/keys.rs` — `handle_key(&mut App, KeyEvent) -> Outcome`.
- `crates/tcpvisr-tui/src/render.rs` — `render(&mut Frame, &App)`.
- `crates/tcpvisr-tui/src/run.rs` — `run(App) -> io::Result<()>` event loop.
- `crates/tcp-visr/src/main.rs` — add `Replay { file }` handling + TTY guard.
- `crates/tcp-visr/Cargo.toml` — add `tcpvisr-tui` dep.
- `crates/tcp-visr/tests/cli.rs` — repoint the unimplemented test to `live`.
- `crates/tcp-visr/tests/replay.rs` — new: non-TTY guard integration test.

Test tables (`ConnRow`) are built from hand-constructed `Connection` values; a shared test helper builds them.

---

### Task 1: Port→service label table

**Files:**
- Modify: `crates/tcpvisr-tui/src/lib.rs`
- Create: `crates/tcpvisr-tui/src/service.rs`

**Interfaces:**
- Produces: `pub fn service_name(port: u16) -> Option<&'static str>`.

- [ ] **Step 1: Write the failing test**

In `crates/tcpvisr-tui/src/service.rs`:

```rust
//! Well-known TCP port → service name labels (design §6, M4). Static and I/O-free.

/// Returns the well-known service name for a TCP port, or `None` if unknown.
///
/// A small built-in table of common ports (design philosophy: deterministic, no
/// `/etc/services` read). Unknown ports intentionally yield `None` so the caller
/// renders a blank service cell rather than a bogus label.
#[must_use]
pub fn service_name(port: u16) -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::service_name;

    #[test]
    fn known_ports_map_to_names() {
        assert_eq!(service_name(22), Some("ssh"));
        assert_eq!(service_name(53), Some("domain"));
        assert_eq!(service_name(80), Some("http"));
        assert_eq!(service_name(443), Some("https"));
        assert_eq!(service_name(5432), Some("postgresql"));
    }

    #[test]
    fn unknown_port_is_none() {
        assert_eq!(service_name(51324), None);
        assert_eq!(service_name(0), None);
    }
}
```

Add to `crates/tcpvisr-tui/src/lib.rs` (replacing the single doc-comment line):

```rust
//! ratatui master/detail UI, timeline cursor, and graph views.

pub mod service;

pub use service::service_name;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p tcpvisr-tui service::`
Expected: FAIL — `known_ports_map_to_names` asserts `Some("ssh")` but got `None`.

- [ ] **Step 3: Write minimal implementation**

Replace the `service_name` body:

```rust
#[must_use]
pub fn service_name(port: u16) -> Option<&'static str> {
    let name = match port {
        20 | 21 => "ftp",
        22 => "ssh",
        23 => "telnet",
        25 => "smtp",
        53 => "domain",
        67 | 68 => "dhcp",
        80 => "http",
        110 => "pop3",
        123 => "ntp",
        143 => "imap",
        179 => "bgp",
        389 => "ldap",
        443 => "https",
        445 => "microsoft-ds",
        465 => "smtps",
        587 => "submission",
        631 => "ipp",
        993 => "imaps",
        995 => "pop3s",
        3306 => "mysql",
        3389 => "rdp",
        5432 => "postgresql",
        6379 => "redis",
        8080 => "http-alt",
        8443 => "https-alt",
        _ => return None,
    };
    Some(name)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p tcpvisr-tui service::`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test -p tcpvisr-tui
git add crates/tcpvisr-tui/src/service.rs crates/tcpvisr-tui/src/lib.rs
git commit -m "feat(tui): add well-known port to service label table"
```

---

### Task 2: App state, rows, and sorted `visible()`

Adds the ratatui-free dependencies and the core `App` with sort-only `visible()` (filter comes in Task 4). This is the foundational type; later tasks extend it.

**Files:**
- Modify: `crates/tcpvisr-tui/Cargo.toml`
- Create: `crates/tcpvisr-tui/src/app.rs`
- Modify: `crates/tcpvisr-tui/src/lib.rs`

**Interfaces:**
- Consumes: `service_name` (Task 1); engine/core types (Global Constraints).
- Produces:
  - `pub struct ConnRow { pub id: ConnId, pub peer: Endpoint, pub service: Option<&'static str>, pub state: ConnState, pub origin_inferred: bool, pub bytes_up: u64, pub bytes_down: u64, pub search: String }`
  - `pub enum SortField { Peer, State, BytesUp, BytesDown }`
  - `pub enum SortDir { Asc, Desc }`
  - `pub enum Mode { Nav, Filter }`
  - `pub enum Outcome { Continue, Quit }`
  - `pub struct App` with `pub fn new(conns: Vec<Connection>, title: String) -> Self`, `pub fn visible(&self) -> Vec<&ConnRow>`, and read accessors `title(&self) -> &str`, `sort_field(&self) -> SortField`, `sort_dir(&self) -> SortDir`, `mode(&self) -> Mode`, `query(&self) -> &str`, `selected(&self) -> Option<ConnId>`.

- [ ] **Step 1: Add dependencies**

In `crates/tcpvisr-tui/Cargo.toml`, add after the `[package]`/`[lints]` blocks:

```toml
[dependencies]
ratatui = "=0.30.2"
tcpvisr-core = { path = "../tcpvisr-core" }
tcpvisr-engine = { path = "../tcpvisr-engine" }
```

Run `cargo build -p tcpvisr-tui` once to fetch ratatui, then `cargo deny check`. Handle its three gates:
- **licenses:** if it reports a license not in `deny.toml`'s `allow` list, add the exact SPDX id it names (e.g. a `Zlib`/`BSD-3-Clause` transitive dep) to the `allow` array. Only add ids `cargo deny` actually flags.
- **advisories:** if it reports a `RUSTSEC` advisory against a ratatui transitive dep, **stop and surface it** — a real advisory is a blocker to report, not something to silence.
- **bans:** a duplicate-version warning is acceptable unless `deny.toml` sets `multiple-versions = "deny"`; do not add a ban skip without cause. Do not weaken any deny gate to make the build pass.

- [ ] **Step 2: Write the failing test**

Create `crates/tcpvisr-tui/src/app.rs` with the skeleton and tests. Test helper builds `Connection`s:

```rust
//! Pure master-list state: rows, sort, filter, selection (spec §3, §4; ADR-0009).

use tcpvisr_core::Endpoint;
use tcpvisr_engine::{ConnId, ConnState, Connection};

use crate::service::service_name;

/// One master-list row: a display projection of a tracked [`Connection`].
#[derive(Debug, Clone)]
pub struct ConnRow {
    pub id: ConnId,
    pub peer: Endpoint,
    pub service: Option<&'static str>,
    pub state: ConnState,
    pub origin_inferred: bool,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub search: String,
}

/// The column the list is ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Peer,
    State,
    BytesUp,
    BytesDown,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

/// Which key-handling mode the app is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Nav,
    Filter,
}

/// Result of handling a key: keep running or quit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Quit,
}

/// Pure interactive state for the master list. No I/O, no clock.
#[derive(Debug, Clone)]
pub struct App {
    rows: Vec<ConnRow>,
    sort_field: SortField,
    sort_dir: SortDir,
    mode: Mode,
    query: String,
    selected: Option<ConnId>,
    title: String,
}

impl App {
    /// Builds the app from a capture's connections and a header title string.
    #[must_use]
    pub fn new(conns: Vec<Connection>, title: String) -> Self {
        let rows: Vec<ConnRow> = conns.iter().map(ConnRow::from_connection).collect();
        let mut app = Self {
            rows,
            sort_field: SortField::Peer,
            sort_dir: SortDir::Asc,
            mode: Mode::Nav,
            query: String::new(),
            selected: None,
            title,
        };
        app.selected = app.visible().first().map(|r| r.id);
        app
    }

    /// The filtered + sorted rows, in display order.
    #[must_use]
    pub fn visible(&self) -> Vec<&ConnRow> {
        let mut rows: Vec<&ConnRow> = self.rows.iter().collect();
        rows.sort_by(|a, b| self.order(a, b));
        rows
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub fn sort_field(&self) -> SortField {
        self.sort_field
    }
    #[must_use]
    pub fn sort_dir(&self) -> SortDir {
        self.sort_dir
    }
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }
    #[must_use]
    pub fn selected(&self) -> Option<ConnId> {
        self.selected
    }

    fn order(&self, a: &ConnRow, b: &ConnRow) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let base = match self.sort_field {
            SortField::Peer => a.peer.cmp(&b.peer),
            SortField::State => rank(a.state).cmp(&rank(b.state)),
            SortField::BytesUp => a.bytes_up.cmp(&b.bytes_up),
            SortField::BytesDown => a.bytes_down.cmp(&b.bytes_down),
        };
        let base = if self.sort_dir == SortDir::Desc {
            base.reverse()
        } else {
            base
        };
        // Deterministic tie-break by ConnId (pair then instance), never reversed.
        base.then_with(|| a.id.pair.cmp(&b.id.pair))
            .then_with(|| a.id.instance.cmp(&b.id.instance))
            .then(Ordering::Equal)
    }
}

/// Stable numeric rank for ordering `ConnState` (lifecycle order).
fn rank(s: ConnState) -> u8 {
    match s {
        ConnState::SynSent => 0,
        ConnState::SynReceived => 1,
        ConnState::Established => 2,
        ConnState::FinWait => 3,
        ConnState::Closed => 4,
        ConnState::Reset => 5,
    }
}

impl ConnRow {
    fn from_connection(c: &Connection) -> Self {
        let service = service_name(c.responder.port);
        let search = format!(
            "{} {} {} {:?}",
            c.origin,
            c.responder,
            service.unwrap_or(""),
            c.state,
        )
        .to_lowercase();
        Self {
            id: c.id,
            peer: c.responder,
            service,
            state: c.state,
            origin_inferred: c.origin_inferred,
            bytes_up: c.bytes_o2r,
            bytes_down: c.bytes_r2o,
            search,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use core::net::{IpAddr, Ipv4Addr};
    use tcpvisr_core::Endpoint;
    use tcpvisr_engine::{ConnId, EndpointPair};

    fn ep(a: u8, port: u16) -> Endpoint {
        Endpoint {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, a)),
            port,
        }
    }

    /// Builds a Connection with the given endpoints/bytes/state for tests.
    fn conn(origin: Endpoint, responder: Endpoint, up: u64, down: u64, inst: u32) -> Connection {
        Connection {
            id: ConnId {
                pair: EndpointPair::new(origin, responder),
                instance: inst,
            },
            state: ConnState::Established,
            origin,
            responder,
            origin_inferred: false,
            opened_at: tcpvisr_core::Nanos(0),
            last_at: tcpvisr_core::Nanos(1),
            bytes_o2r: up,
            bytes_r2o: down,
            segments: 1,
        }
    }

    fn app_of(conns: Vec<Connection>) -> App {
        App::new(conns, "t".to_string())
    }

    #[test]
    fn new_builds_one_row_per_connection_with_service_label() {
        let c = conn(ep(1, 51324), ep(2, 443), 10, 20, 0);
        let app = app_of(vec![c]);
        let rows = app.visible();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].peer, ep(2, 443));
        assert_eq!(rows[0].service, Some("https"));
        assert_eq!(rows[0].bytes_up, 10);
        assert_eq!(rows[0].bytes_down, 20);
    }

    #[test]
    fn unknown_responder_port_has_no_service() {
        let app = app_of(vec![conn(ep(1, 40000), ep(2, 40001), 0, 0, 0)]);
        assert_eq!(app.visible()[0].service, None);
    }

    #[test]
    fn initial_selection_is_first_visible_row() {
        let c1 = conn(ep(1, 1), ep(2, 22), 0, 0, 0); // peer 10.0.0.2:22
        let c2 = conn(ep(1, 2), ep(3, 22), 0, 0, 0); // peer 10.0.0.3:22
        let app = app_of(vec![c2.clone(), c1.clone()]);
        // Peer-ascending → 10.0.0.2:22 first.
        assert_eq!(app.selected(), Some(c1.id));
        assert_eq!(app.visible()[0].id, c1.id);
    }

    #[test]
    fn empty_capture_has_no_rows_or_selection() {
        let app = app_of(vec![]);
        assert!(app.visible().is_empty());
        assert_eq!(app.selected(), None);
    }
}
```

Add to `crates/tcpvisr-tui/src/lib.rs`:

```rust
pub mod app;

pub use app::{App, ConnRow, Mode, Outcome, SortDir, SortField};
```

Note: `Connection`, `ConnId`, `EndpointPair`, `Nanos` must be constructible in tests — they are (`Connection` fields are all `pub`; `EndpointPair::new` is public). If `Connection` is not `Clone`, drop the `.clone()` calls and rebuild via `conn(...)`. (It is `Clone + Copy`.)

- [ ] **Step 3: Run the tests**

Run: `cargo test -p tcpvisr-tui app::`
Expected: PASS. This is a **scaffolding task**: `App::new`/`visible()` are the
foundational data projection, so implementation and its assertion tests land together
and there is no separate red phase. Behavioral red→green coverage comes from the later
sort/filter/selection tasks (Tasks 3–5), which each write a failing test against the
missing method first. (If this task fails to compile, fix field/type mismatches against
the engine before proceeding.)

- [ ] **Step 4: Guardrails**

Run: `cargo clippy -p tcpvisr-tui --all-targets -- -D warnings` and `cargo fmt --all --check`.
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/tcpvisr-tui/Cargo.toml crates/tcpvisr-tui/src/app.rs crates/tcpvisr-tui/src/lib.rs deny.toml
git commit -m "feat(tui): add App state with sorted master-list rows"
```

---

### Task 3: Sort controls — cycle field and toggle direction

**Files:**
- Modify: `crates/tcpvisr-tui/src/app.rs`

**Interfaces:**
- Produces on `App`: `pub fn cycle_sort(&mut self)`, `pub fn toggle_dir(&mut self)`. `cycle_sort` advances `Peer→State→BytesUp→BytesDown→Peer` and resets the new field to its natural default (`Peer`/`State` → `Asc`; byte fields → `Desc`). `toggle_dir` flips `sort_dir` for the current field only.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `app.rs`:

```rust
    #[test]
    fn cycle_sort_visits_fields_and_resets_natural_direction() {
        let mut app = app_of(vec![conn(ep(1, 1), ep(2, 80), 0, 0, 0)]);
        assert_eq!((app.sort_field(), app.sort_dir()), (SortField::Peer, SortDir::Asc));
        app.cycle_sort();
        assert_eq!((app.sort_field(), app.sort_dir()), (SortField::State, SortDir::Asc));
        app.cycle_sort();
        assert_eq!((app.sort_field(), app.sort_dir()), (SortField::BytesUp, SortDir::Desc));
        app.cycle_sort();
        assert_eq!((app.sort_field(), app.sort_dir()), (SortField::BytesDown, SortDir::Desc));
        app.cycle_sort();
        assert_eq!((app.sort_field(), app.sort_dir()), (SortField::Peer, SortDir::Asc));
    }

    #[test]
    fn cycle_resets_direction_even_after_toggle() {
        let mut app = app_of(vec![conn(ep(1, 1), ep(2, 80), 0, 0, 0)]);
        app.toggle_dir(); // Peer now Desc
        assert_eq!(app.sort_dir(), SortDir::Desc);
        app.cycle_sort(); // → State, reset to natural Asc
        assert_eq!((app.sort_field(), app.sort_dir()), (SortField::State, SortDir::Asc));
    }

    #[test]
    fn bytes_up_sorts_descending_by_default() {
        let small = conn(ep(1, 1), ep(2, 80), 5, 0, 0);
        let big = conn(ep(1, 2), ep(3, 80), 500, 0, 0);
        let mut app = app_of(vec![small.clone(), big.clone()]);
        app.cycle_sort(); // State
        app.cycle_sort(); // BytesUp, Desc
        let order: Vec<_> = app.visible().iter().map(|r| r.bytes_up).collect();
        assert_eq!(order, vec![500, 5]);
    }

    #[test]
    fn toggle_dir_reverses_without_changing_field() {
        let a = conn(ep(1, 1), ep(2, 80), 0, 0, 0); // peer 10.0.0.2:80
        let b = conn(ep(1, 2), ep(3, 80), 0, 0, 0); // peer 10.0.0.3:80
        let mut app = app_of(vec![a.clone(), b.clone()]);
        assert_eq!(app.visible()[0].id, a.id); // Asc
        app.toggle_dir();
        assert_eq!(app.sort_field(), SortField::Peer);
        assert_eq!(app.visible()[0].id, b.id); // Desc
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p tcpvisr-tui app::cycle_sort_visits_fields_and_resets_natural_direction`
Expected: FAIL — `cycle_sort` not found.

- [ ] **Step 3: Write minimal implementation**

Add methods to `impl App` (before the private `order`):

```rust
    /// Advances the sort field (wrapping) and resets it to its natural direction.
    pub fn cycle_sort(&mut self) {
        self.sort_field = match self.sort_field {
            SortField::Peer => SortField::State,
            SortField::State => SortField::BytesUp,
            SortField::BytesUp => SortField::BytesDown,
            SortField::BytesDown => SortField::Peer,
        };
        self.sort_dir = natural_dir(self.sort_field);
    }

    /// Toggles ascending/descending for the current field only.
    pub fn toggle_dir(&mut self) {
        self.sort_dir = match self.sort_dir {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        };
    }
```

Add free function near `rank`:

```rust
/// The default direction a field sorts in when first selected.
fn natural_dir(field: SortField) -> SortDir {
    match field {
        SortField::Peer | SortField::State => SortDir::Asc,
        SortField::BytesUp | SortField::BytesDown => SortDir::Desc,
    }
}
```

- [ ] **Step 4: Run tests + guardrails**

Run: `cargo test -p tcpvisr-tui app::` then clippy/fmt.
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add crates/tcpvisr-tui/src/app.rs
git commit -m "feat(tui): add sort field cycling and direction toggle"
```

---

### Task 4: Filter mode and subsequence matching

**Files:**
- Modify: `crates/tcpvisr-tui/src/app.rs`

**Interfaces:**
- Produces on `App`: `pub fn enter_filter(&mut self)`, `pub fn push_filter(&mut self, c: char)`, `pub fn pop_filter(&mut self)`, `pub fn confirm_filter(&mut self)`, `pub fn cancel_filter(&mut self)`. `visible()` now filters rows whose `search` contains the lowercased `query` as a subsequence, then sorts. Selection is reconciled to stay visible (falls back to first visible / `None`).

**Note on test data — subsequence matching is permissive.** The composite search
string includes the lowercased state, and `is_subsequence` matches non-adjacent
characters. Since almost every fixture connection is `Established`, short queries can
accidentally be subsequences of the shared word `established` (e.g. `ssh` ⊆
`e-s-tabli-s-h-ed`). These tests therefore filter on **service labels that share no
subsequence with `established`** — `postgresql` vs `https` — so each query narrows to
exactly one row deterministically. `https` has no `p`/`o`/`g` and `postgres` has no
`h`, so neither is a subsequence of the other row's string.

- [ ] **Step 1: Write the failing test**

Add to `tests`:

```rust
    // db: responder :5432 → service "postgresql", peer 10.0.0.2 (sorts first)
    // web: responder :443 → service "https", peer 10.0.0.4
    fn db_conn() -> Connection {
        conn(ep(1, 1111), ep(2, 5432), 0, 0, 0)
    }
    fn web_conn() -> Connection {
        conn(ep(3, 2222), ep(4, 443), 0, 0, 0)
    }

    #[test]
    fn filter_narrows_to_one_then_clears() {
        let mut app = app_of(vec![db_conn(), web_conn()]);
        app.enter_filter();
        assert_eq!(app.mode(), Mode::Filter);
        for c in "https".chars() {
            app.push_filter(c);
        }
        // "https" has no 'p'/'o'/'g'/'t'-'t' chain in the db row → web only.
        let ids: Vec<_> = app.visible().iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![web_conn().id]);
        app.pop_filter(); // "http" still matches only the web row
        assert_eq!(app.visible().len(), 1);
        app.cancel_filter();
        assert_eq!(app.mode(), Mode::Nav);
        assert_eq!(app.query(), "");
        assert_eq!(app.visible().len(), 2);
    }

    #[test]
    fn subsequence_matches_non_adjacent_chars() {
        let mut app = app_of(vec![web_conn()]); // search contains "https"
        app.enter_filter();
        for c in "hts".chars() {
            // h,t,s appear in order within "https" though not adjacently.
            app.push_filter(c);
        }
        assert_eq!(app.visible().len(), 1);
    }

    #[test]
    fn confirm_keeps_filter_esc_clears_it() {
        let mut app = app_of(vec![db_conn(), web_conn()]);
        app.enter_filter();
        for c in "postgres".chars() {
            app.push_filter(c);
        }
        app.confirm_filter();
        assert_eq!(app.mode(), Mode::Nav);
        assert_eq!(app.query(), "postgres");
        // "postgres" has no 'h' → the db (postgresql) row only.
        assert_eq!(app.visible().len(), 1);
    }

    #[test]
    fn selection_falls_back_to_first_visible_when_filtered_out() {
        let mut app = app_of(vec![db_conn(), web_conn()]);
        // db is first (peer 10.0.0.2:5432 < 10.0.0.4:443) → initially selected.
        assert_eq!(app.selected(), Some(db_conn().id));
        app.enter_filter();
        for c in "https".chars() {
            app.push_filter(c);
        }
        // db filtered out → selection falls back to the only visible row (web).
        assert_eq!(app.selected(), Some(web_conn().id));
    }

    #[test]
    fn selection_becomes_none_when_nothing_matches() {
        let mut app = app_of(vec![db_conn()]);
        app.enter_filter();
        for c in "zzz".chars() {
            app.push_filter(c);
        }
        assert!(app.visible().is_empty());
        assert_eq!(app.selected(), None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p tcpvisr-tui app::filter_narrows_by_subsequence_and_widens_on_backspace`
Expected: FAIL — `enter_filter` not found.

- [ ] **Step 3: Write minimal implementation**

Change `visible()` to filter then sort:

```rust
    #[must_use]
    pub fn visible(&self) -> Vec<&ConnRow> {
        let q = self.query.to_lowercase();
        let mut rows: Vec<&ConnRow> = self
            .rows
            .iter()
            .filter(|r| is_subsequence(&q, &r.search))
            .collect();
        rows.sort_by(|a, b| self.order(a, b));
        rows
    }
```

Add filter methods to `impl App`:

```rust
    /// Enters filter-input mode, keeping any existing query for editing.
    pub fn enter_filter(&mut self) {
        self.mode = Mode::Filter;
    }

    /// Appends a character to the filter query and reconciles the selection.
    pub fn push_filter(&mut self, c: char) {
        self.query.push(c);
        self.reconcile_selection();
    }

    /// Removes the last character of the filter query and reconciles the selection.
    pub fn pop_filter(&mut self) {
        self.query.pop();
        self.reconcile_selection();
    }

    /// Leaves filter-input mode, keeping the current filter applied.
    pub fn confirm_filter(&mut self) {
        self.mode = Mode::Nav;
    }

    /// Clears the query and leaves filter-input mode.
    pub fn cancel_filter(&mut self) {
        self.query.clear();
        self.mode = Mode::Nav;
        self.reconcile_selection();
    }

    /// Ensures the selected id is still visible; otherwise selects the first
    /// visible row (or `None` if nothing is visible).
    fn reconcile_selection(&mut self) {
        let visible = self.visible();
        let still_visible = self
            .selected
            .is_some_and(|id| visible.iter().any(|r| r.id == id));
        if !still_visible {
            self.selected = visible.first().map(|r| r.id);
        }
    }
```

Add the free helper:

```rust
/// True if `needle` is a subsequence of `haystack` (chars appear in order).
/// Both are expected already-lowercased by the caller. Empty needle matches.
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars();
    for nc in needle.chars() {
        loop {
            match hay.next() {
                Some(hc) if hc == nc => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}
```

- [ ] **Step 4: Run tests + guardrails**

Run: `cargo test -p tcpvisr-tui app::` then clippy/fmt.
Expected: PASS, clean. Confirm `is_subsequence` unit behavior is exercised by the filter tests.

- [ ] **Step 5: Commit**

```bash
git add crates/tcpvisr-tui/src/app.rs
git commit -m "feat(tui): add filter mode with subsequence matching"
```

---

### Task 5: Selection movement

**Files:**
- Modify: `crates/tcpvisr-tui/src/app.rs`

**Interfaces:**
- Produces on `App`: `pub fn move_up(&mut self)`, `pub fn move_down(&mut self)`. Movement is relative to `visible()` order, tracked by `ConnId`, clamped at both ends (no wrap). Re-sorting preserves the selected `ConnId`.

- [ ] **Step 1: Write the failing test**

Add to `tests`:

```rust
    #[test]
    fn move_down_and_up_clamp_at_ends() {
        let a = conn(ep(1, 1), ep(2, 22), 0, 0, 0);
        let b = conn(ep(1, 2), ep(3, 22), 0, 0, 0);
        let c = conn(ep(1, 3), ep(4, 22), 0, 0, 0);
        let mut app = app_of(vec![a.clone(), b.clone(), c.clone()]);
        assert_eq!(app.selected(), Some(a.id)); // first
        app.move_up(); // clamp at top
        assert_eq!(app.selected(), Some(a.id));
        app.move_down();
        assert_eq!(app.selected(), Some(b.id));
        app.move_down();
        assert_eq!(app.selected(), Some(c.id));
        app.move_down(); // clamp at bottom
        assert_eq!(app.selected(), Some(c.id));
        app.move_up();
        assert_eq!(app.selected(), Some(b.id));
    }

    #[test]
    fn resort_keeps_same_conn_selected() {
        let a = conn(ep(1, 1), ep(2, 22), 10, 0, 0); // peer 10.0.0.2:22, up 10
        let b = conn(ep(1, 2), ep(3, 22), 20, 0, 0); // peer 10.0.0.3:22, up 20
        let mut app = app_of(vec![a.clone(), b.clone()]);
        app.move_down(); // select b
        assert_eq!(app.selected(), Some(b.id));
        app.cycle_sort(); // State
        app.cycle_sort(); // BytesUp Desc → order [b(20), a(10)]
        assert_eq!(app.selected(), Some(b.id)); // still b
        assert_eq!(app.visible()[0].id, b.id);
    }

    #[test]
    fn move_is_noop_when_empty() {
        let mut app = app_of(vec![]);
        app.move_down();
        app.move_up();
        assert_eq!(app.selected(), None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p tcpvisr-tui app::move_down_and_up_clamp_at_ends`
Expected: FAIL — `move_up`/`move_down` not found.

- [ ] **Step 3: Write minimal implementation**

Add to `impl App`:

```rust
    /// Moves the selection up one row in the visible order (clamped).
    pub fn move_up(&mut self) {
        self.move_by(-1);
    }

    /// Moves the selection down one row in the visible order (clamped).
    pub fn move_down(&mut self) {
        self.move_by(1);
    }

    fn move_by(&mut self, delta: isize) {
        let visible = self.visible();
        if visible.is_empty() {
            self.selected = None;
            return;
        }
        let cur = self
            .selected
            .and_then(|id| visible.iter().position(|r| r.id == id))
            .unwrap_or(0);
        let last = visible.len() - 1;
        let next = (cur as isize + delta).clamp(0, last as isize) as usize;
        self.selected = Some(visible[next].id);
    }
```

- [ ] **Step 4: Run tests + guardrails**

Run: `cargo test -p tcpvisr-tui app::` then clippy/fmt. `as` casts here are bounded (`clamp` keeps within `0..=last`), acceptable under pedantic; if clippy flags `cast_possible_wrap`/`cast_sign_loss`, restructure with `usize` arithmetic and a match on `delta` sign instead of a file allow.
Expected: PASS, clean.

- [ ] **Step 5: Commit**

```bash
git add crates/tcpvisr-tui/src/app.rs
git commit -m "feat(tui): add ConnId-tracked selection movement"
```

---

### Task 6: Modal key handling

**Files:**
- Create: `crates/tcpvisr-tui/src/keys.rs`
- Modify: `crates/tcpvisr-tui/src/lib.rs`

**Interfaces:**
- Consumes: `App`, `Mode`, `Outcome` (Task 2–5); `ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers}`.
- Produces: `pub fn handle_key(app: &mut App, key: KeyEvent) -> Outcome`.

- [ ] **Step 1: Write the failing test**

Create `crates/tcpvisr-tui/src/keys.rs`:

```rust
//! Modal key handling: the single pure decision point for the master list (spec §3.5).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Mode, Outcome};

/// Maps a key press to state changes on `app`, returning whether to keep running.
///
/// Modal (spec §3.5): in navigation mode the command keys sort / move / filter /
/// quit; in filter-input mode every printable character is appended to the query
/// and only Enter/Esc/Backspace are commands. `Ctrl-C` quits in either mode.
pub fn handle_key(app: &mut App, key: KeyEvent) -> Outcome {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Outcome::Quit;
    }
    match app.mode() {
        Mode::Nav => handle_nav(app, key.code),
        Mode::Filter => {
            handle_filter(app, key.code);
            Outcome::Continue
        }
    }
}

fn handle_nav(app: &mut App, code: KeyCode) -> Outcome {
    match code {
        KeyCode::Char('q') => return Outcome::Quit,
        KeyCode::Char('s') => app.cycle_sort(),
        KeyCode::Char('S') => app.toggle_dir(),
        KeyCode::Char('/') => app.enter_filter(),
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        _ => {}
    }
    Outcome::Continue
}

fn handle_filter(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Enter => app.confirm_filter(),
        KeyCode::Esc => app.cancel_filter(),
        KeyCode::Backspace => app.pop_filter(),
        KeyCode::Char(c) => app.push_filter(c),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::app::{Mode, Outcome, SortField};
    use core::net::{IpAddr, Ipv4Addr};
    use ratatui::crossterm::event::{KeyEvent, KeyModifiers};
    use tcpvisr_core::{Endpoint, Nanos};
    use tcpvisr_engine::{ConnId, ConnState, Connection, EndpointPair};

    fn ep(a: u8, port: u16) -> Endpoint {
        Endpoint {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, a)),
            port,
        }
    }

    fn conn(origin: Endpoint, responder: Endpoint) -> Connection {
        Connection {
            id: ConnId {
                pair: EndpointPair::new(origin, responder),
                instance: 0,
            },
            state: ConnState::Established,
            origin,
            responder,
            origin_inferred: false,
            opened_at: Nanos(0),
            last_at: Nanos(1),
            bytes_o2r: 0,
            bytes_r2o: 0,
            segments: 1,
        }
    }

    fn app() -> App {
        App::new(
            vec![conn(ep(1, 1), ep(2, 22)), conn(ep(1, 2), ep(3, 443))],
            "t".to_string(),
        )
    }

    fn press(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn q_quits_in_nav_mode() {
        let mut a = app();
        assert_eq!(handle_key(&mut a, press('q')), Outcome::Quit);
    }

    #[test]
    fn ctrl_c_quits_in_nav_mode() {
        let mut a = app();
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut a, ev), Outcome::Quit);
    }

    #[test]
    fn q_in_filter_mode_appends_and_does_not_quit() {
        let mut a = app();
        assert_eq!(handle_key(&mut a, press('/')), Outcome::Continue);
        assert_eq!(a.mode(), Mode::Filter);
        assert_eq!(handle_key(&mut a, press('q')), Outcome::Continue);
        assert_eq!(a.query(), "q");
    }

    #[test]
    fn ctrl_c_quits_even_in_filter_mode() {
        let mut a = app();
        handle_key(&mut a, press('/'));
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut a, ev), Outcome::Quit);
    }

    #[test]
    fn s_cycles_sort_in_nav_mode() {
        let mut a = app();
        assert_eq!(a.sort_field(), SortField::Peer);
        handle_key(&mut a, press('s'));
        assert_eq!(a.sort_field(), SortField::State);
    }

    #[test]
    fn esc_clears_filter() {
        let mut a = app();
        handle_key(&mut a, press('/'));
        handle_key(&mut a, press('s'));
        handle_key(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.mode(), Mode::Nav);
        assert_eq!(a.query(), "");
    }
```

Close the `tests` module with `}`.

Add to `crates/tcpvisr-tui/src/lib.rs`:

```rust
pub mod keys;

pub use keys::handle_key;
```

- [ ] **Step 2: Run test to verify it fails, then passes**

Run: `cargo test -p tcpvisr-tui keys::`
Expected: PASS once the module compiles (the implementation is written alongside the test). If `KeyEvent::new` signature differs in 0.30, adjust construction (it takes `(KeyCode, KeyModifiers)`).

- [ ] **Step 3: Guardrails**

Run clippy/fmt. Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/tcpvisr-tui/src/keys.rs crates/tcpvisr-tui/src/lib.rs
git commit -m "feat(tui): add modal key handling"
```

---

### Task 7: Rendering with ratatui `TestBackend` tests

**Files:**
- Create: `crates/tcpvisr-tui/src/render.rs`
- Modify: `crates/tcpvisr-tui/src/lib.rs`

**Interfaces:**
- Consumes: `App`, `Mode`, `SortField`, `SortDir`, `visible()`, `selected()`, `title()`, `query()`.
- Produces: `pub fn render(frame: &mut ratatui::Frame, app: &App)`.

- [ ] **Step 1: Write the failing test**

Create `crates/tcpvisr-tui/src/render.rs`:

```rust
//! Pure ratatui rendering of the master list (spec §3.2). No terminal I/O.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Text;
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState};

use crate::app::{App, Mode, SortDir, SortField};

/// Draws the master list — header, table (or empty state), and footer — into `frame`.
pub fn render(frame: &mut Frame, app: &App) {
    let [main, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    render_main(frame, app, main);
    render_footer(frame, app, footer);
}

fn render_main(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(app.title().to_string());
    let rows = app.visible();
    if rows.is_empty() {
        let p = Paragraph::new("no connections in capture").block(block);
        frame.render_widget(p, area);
        return;
    }

    let header = Row::new([
        Cell::from("PEER"),
        Cell::from("SERVICE"),
        Cell::from("STATE"),
        Cell::from(Text::from("↑BYTES").right_aligned()),
        Cell::from(Text::from("↓BYTES").right_aligned()),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let selected_idx = app
        .selected()
        .and_then(|id| rows.iter().position(|r| r.id == id));

    let body: Vec<Row> = rows
        .iter()
        .map(|r| {
            let state = if r.origin_inferred {
                format!("{:?}~", r.state)
            } else {
                format!("{:?}", r.state)
            };
            Row::new([
                Cell::from(r.peer.to_string()),
                Cell::from(r.service.unwrap_or("")),
                Cell::from(state),
                Cell::from(Text::from(r.bytes_up.to_string()).right_aligned()),
                Cell::from(Text::from(r.bytes_down.to_string()).right_aligned()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Min(20),
        Constraint::Length(12),
        Constraint::Length(14),
        Constraint::Length(10),
        Constraint::Length(10),
    ];
    let table = Table::new(body, widths)
        .header(header)
        .block(block)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▸");

    let mut state = TableState::default().with_selected(selected_idx);
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let text = match app.mode() {
        Mode::Filter => format!("/{}", app.query()),
        Mode::Nav => {
            let arrow = match app.sort_dir() {
                SortDir::Asc => '▲',
                SortDir::Desc => '▼',
            };
            format!(
                "/ filter   s sort:{}{arrow}   S reverse   ↑↓ select   q quit",
                sort_label(app.sort_field()),
            )
        }
    };
    frame.render_widget(Paragraph::new(text), area);
}

fn sort_label(field: SortField) -> &'static str {
    match field {
        SortField::Peer => "peer",
        SortField::State => "state",
        SortField::BytesUp => "up",
        SortField::BytesDown => "down",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::render;
    use crate::app::App;
    use crate::handle_key;
    use core::net::{IpAddr, Ipv4Addr};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tcpvisr_core::{Endpoint, Nanos};
    use tcpvisr_engine::{ConnId, ConnState, Connection, EndpointPair};

    fn ep(a: u8, port: u16) -> Endpoint {
        Endpoint {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, a)),
            port,
        }
    }

    fn conn(origin: Endpoint, responder: Endpoint, inferred: bool) -> Connection {
        Connection {
            id: ConnId {
                pair: EndpointPair::new(origin, responder),
                instance: 0,
            },
            state: ConnState::Established,
            origin,
            responder,
            origin_inferred: inferred,
            opened_at: Nanos(0),
            last_at: Nanos(1),
            bytes_o2r: 10,
            bytes_r2o: 20,
            segments: 1,
        }
    }

    fn buffer_string(buf: &Buffer) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    fn draw(app: &App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        buffer_string(terminal.backend().buffer())
    }

    #[test]
    fn renders_header_columns_selection_and_footer() {
        let app = App::new(
            vec![conn(ep(1, 5), ep(2, 443), false)],
            "tcp-visr — c.pcap  (1 connections, skipped 0)".to_string(),
        );
        let s = draw(&app, 80, 10);
        assert!(s.contains("tcp-visr — c.pcap"), "{s}");
        assert!(s.contains("PEER"), "{s}");
        assert!(s.contains("SERVICE"), "{s}");
        assert!(s.contains("https"), "{s}");
        assert!(s.contains("▸"), "selection marker: {s}");
        assert!(s.contains("q quit"), "footer: {s}");
        assert!(s.contains("sort:peer▲"), "sort indicator: {s}");
    }

    #[test]
    fn renders_mid_stream_marker() {
        let app = App::new(vec![conn(ep(1, 5), ep(2, 443), true)], "t".to_string());
        let s = draw(&app, 80, 6);
        assert!(s.contains("Established~"), "{s}");
    }

    #[test]
    fn filter_mode_shows_query_line() {
        let mut app = App::new(vec![conn(ep(1, 5), ep(2, 443), false)], "t".to_string());
        handle_key(&mut app, KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        let s = draw(&app, 80, 6);
        assert!(s.contains("/h"), "{s}");
    }

    #[test]
    fn empty_capture_shows_placeholder() {
        let app = App::new(vec![], "t".to_string());
        let s = draw(&app, 40, 6);
        assert!(s.contains("no connections in capture"), "{s}");
    }

    #[test]
    fn selected_row_visible_when_viewport_shorter_than_list() {
        // 5 connections, height only fits ~2 body rows; move to the last and
        // assert its peer still renders (scroll-to-selection).
        let conns: Vec<_> = (1..=5).map(|i| conn(ep(1, i), ep(2, 100 + u16::from(i)), false)).collect();
        let mut app = App::new(conns, "t".to_string());
        for _ in 0..4 {
            handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        let s = draw(&app, 60, 5);
        assert!(s.contains(":105"), "last row must be scrolled into view: {s}");
    }
}
```

Add to `crates/tcpvisr-tui/src/lib.rs`:

```rust
pub mod render;

pub use render::render;
```

- [ ] **Step 2: Run test to verify it fails/compiles**

Run: `cargo test -p tcpvisr-tui render::`
Expected: initially FAIL to compile if a ratatui API name differs (0.30). Reconcile against the crate: confirm `Block::bordered`, `Table::new(rows, widths)`, `.row_highlight_style`, `.highlight_symbol`, `TableState::with_selected`, `Text::right_aligned`, `Buffer::cell((x,y)) -> Option<&Cell>`, `terminal.backend().buffer()`. If any differ, use the closest 0.30 equivalent (e.g. `.highlight_style` instead of `.row_highlight_style`). Then re-run to PASS.

- [ ] **Step 3: Guardrails**

Run clippy/fmt. Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/tcpvisr-tui/src/render.rs crates/tcpvisr-tui/src/lib.rs
git commit -m "feat(tui): render master list with TestBackend coverage"
```

---

### Task 8: Terminal event-loop shell

**Files:**
- Create: `crates/tcpvisr-tui/src/run.rs`
- Modify: `crates/tcpvisr-tui/src/lib.rs`

**Interfaces:**
- Consumes: `App`, `Outcome`, `render`, `handle_key`; `ratatui::init`/`restore`, `ratatui::crossterm::event`.
- Produces: `pub fn run(app: App) -> std::io::Result<()>`.

This task is the thin impure shell (spec §4). It is not unit-tested (no TTY in CI); keep it minimal. `ratatui::init()` installs a panic hook that restores the terminal, satisfying restore-on-panic.

- [ ] **Step 1: Write the implementation**

Create `crates/tcpvisr-tui/src/run.rs`:

```rust
//! The impure terminal shell: init, event loop, restore (spec §4, ADR-0009).

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::app::{App, Outcome};
use crate::keys::handle_key;
use crate::render::render;

/// Runs the master-list TUI: sets up the terminal, loops until the user quits,
/// then restores the terminal. Restoration also runs on panic via the hook
/// `ratatui::init` installs.
pub fn run(app: App) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, mut app: App) -> std::io::Result<()> {
    loop {
        terminal.draw(|frame| render(frame, &app))?;
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press && handle_key(&mut app, key) == Outcome::Quit {
                break;
            }
        }
    }
    Ok(())
}
```

Add to `crates/tcpvisr-tui/src/lib.rs`:

```rust
pub mod run;

pub use run::run;
```

- [ ] **Step 2: Build + guardrails**

Run: `cargo build -p tcpvisr-tui` then `cargo clippy -p tcpvisr-tui --all-targets -- -D warnings` and `cargo fmt --all --check`.
Expected: clean. (`DefaultTerminal` is `ratatui::DefaultTerminal`; if the alias name differs, use `Terminal<CrosstermBackend<Stdout>>` via the type `ratatui::init` returns.)

- [ ] **Step 3: Commit**

```bash
git add crates/tcpvisr-tui/src/run.rs crates/tcpvisr-tui/src/lib.rs
git commit -m "feat(tui): add terminal event-loop shell"
```

---

### Task 9: Wire the `replay` subcommand in the bin

**Files:**
- Modify: `crates/tcp-visr/Cargo.toml`
- Modify: `crates/tcp-visr/src/main.rs`
- Modify: `crates/tcp-visr/tests/cli.rs`
- Create: `crates/tcp-visr/tests/replay.rs`

**Interfaces:**
- Consumes: `tcpvisr_tui::{App, run}`; existing `tcpvisr_ingest::parse_file_visit`, `tcpvisr_engine::{Tracker, EngineConfig}`.
- Produces: `tcp-visr replay <file>` behavior.

- [ ] **Step 1: Write the failing integration test**

Create `crates/tcp-visr/tests/replay.rs`:

```rust
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tcp-visr"))
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn replay_without_tty_exits_nonzero_with_actionable_message() {
    // The test harness pipes stdout, so it is not a TTY: the guard must fire
    // instead of the event loop blocking on terminal input.
    let out = bin()
        .args(["replay", &fixture("metrics_basic.pcap")])
        .output()
        .expect("run tcp-visr");
    assert!(!out.status.success(), "should exit nonzero without a tty");
    let stderr = String::from_utf8(out.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("replay requires an interactive terminal"),
        "actionable message: {stderr}"
    );
}

#[test]
fn replay_no_longer_reports_not_implemented() {
    // Under the harness's piped stdout the tty guard fires first, so this does not
    // exercise the ingest path (that is covered by conns/parse tests); it only
    // asserts `replay` is now wired and no longer stubbed as "not implemented".
    let out = bin()
        .args(["replay", "/no/such/file.pcap"])
        .output()
        .expect("run tcp-visr");
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).expect("utf8 stderr");
    assert!(!stderr.contains("not implemented"), "{stderr}");
}
```

Update `crates/tcp-visr/tests/cli.rs`: repoint the `unimplemented_subcommand` test from `replay` to `live`:

```rust
#[test]
fn unimplemented_subcommand_exits_nonzero_with_message() {
    let output = bin().arg("live").output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("not implemented"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p tcp-visr --test replay`
Expected: FAIL — `replay` currently prints "not implemented" (nonzero) so the first test's message assertion fails; the missing-file test may pass incidentally. Confirm the message assertion fails.

- [ ] **Step 3: Add the dependency and wire the command**

In `crates/tcp-visr/Cargo.toml` `[dependencies]`, add:

```toml
tcpvisr-tui = { path = "../tcpvisr-tui" }
```

In `crates/tcp-visr/src/main.rs`:

Change the `Replay` variant to take a file:

```rust
    /// Replay a pcap/pcapng capture in the interactive TUI.
    Replay {
        /// The `.pcap`/`.pcapng` capture file to browse.
        file: PathBuf,
    },
```

Update `Command::name` arm: `Command::Replay { .. } => "replay",` (already returns "replay"; keep it).

In `run()`, replace the `Command::Replay` handling. It currently falls into the `other =>` arm; add an explicit arm before it:

```rust
        Command::Replay { file } => run_replay(&file),
```

Add the function (near `run_conns`):

```rust
/// Streams `file` into the engine (same path as `conns`), then browses the
/// resulting connections in the interactive TUI. Requires an interactive
/// terminal; refuses to run when stdout is redirected so it never blocks a pipe.
fn run_replay(file: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return Err(
            "replay requires an interactive terminal (stdout is not a tty)".into(),
        );
    }
    let mut tracker = Tracker::new(EngineConfig::default());
    let (_link, skipped) =
        tcpvisr_ingest::parse_file_visit(file, &mut |item| tracker.observe(item))?;
    let conns = tracker.into_connections();
    let title = format!(
        "tcp-visr — {}  ({} connections, skipped {})",
        file.display(),
        conns.len(),
        skipped.total(),
    );
    let app = tcpvisr_tui::App::new(conns, title);
    tcpvisr_tui::run(app)?;
    Ok(())
}
```

Note the TTY check runs *before* opening the file, so `replay /no/such/file.pcap` under a pipe reports the tty message, and under a real terminal reports `opening capture`. Both satisfy "not 'not implemented'". If you prefer the missing-file message to win even under a pipe, move the ingest call before the tty check — but keep the tty guard before `tcpvisr_tui::run`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p tcp-visr --test replay` and `cargo test -p tcp-visr --test cli`
Expected: PASS. Then `cargo test --workspace`.

- [ ] **Step 5: Guardrails + commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --workspace
git add crates/tcp-visr/Cargo.toml crates/tcp-visr/src/main.rs crates/tcp-visr/tests/cli.rs crates/tcp-visr/tests/replay.rs
git commit -m "feat(cli): browse a capture's connections with replay TUI"
```

---

## Final verification (before PR)

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo test -p tcpvisr-ingest --features live`
- [ ] `cargo deny check`
- [ ] Manual smoke (interactive, not CI): `cargo run -p tcp-visr -- replay crates/tcp-visr/tests/fixtures/metrics_basic.pcap` — verify sort (`s`/`S`), filter (`/`), selection (`↑↓`), and `q` quits with the terminal restored.

## Self-review notes (spec coverage)

- Spec criterion 1 → Task 9 (`replay.rs`). 2 → Task 2. 3 → Task 3. 4 → Task 3. 5 → Task 4 + Task 6 (`q` in filter). 6 → Task 4 + Task 5. 7 → Task 7. 8 → Task 2 + Task 7. 9 → Task 6.
- Service labels (§3.3) → Task 1. Byte counts raw integers (§3.7) → Task 7 render. Modality (§3.5) → Task 6. Scroll-to-selection (§3.2) → Task 7 render + test. Terminal restore/panic (§2, §6) → Task 8 via `ratatui::init`. Non-TTY guard (§3.1) → Task 9.
