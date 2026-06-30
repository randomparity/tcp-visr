# M0 — Repo & Toolchain Setup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the Cargo workspace, toolchain, lints, CI, and dev hooks so all later milestones land in a guardrail-enforced repo; ship a `tcp-visr` CLI that prints `--help`/`--version`.

**Architecture:** A Cargo workspace (`crates/*`) with five stub library crates (`tcpvisr-core`, `-ingest`, `-engine`, `-enrich`, `-tui`) and the `tcp-visr` binary. Lints are defined once in `[workspace.lints]` and inherited. The binary uses `clap` (derive) for the command surface; unimplemented subcommands return an error (no `panic!`/`exit`/`println!`, per the lint policy).

**Tech Stack:** Rust 1.88.0 (edition 2024, resolver 3), `clap` 4.6.1, `cargo-deny`, GitHub Actions, `prek`.

## Global Constraints

- **Toolchain = MSRV = 1.88.0** — pinned in `rust-toolchain.toml`; CI builds on it (doubles as MSRV check).
- **Edition 2024, resolver 3.**
- **License:** dual `MIT OR Apache-2.0`.
- **Pin exact dependency versions** (`=`, not `^`).
- **Lint policy (CLAUDE.md clippy set)** inherited via `[workspace.lints]`; `-D warnings` in CI. Restriction lints are relaxed in test code via `clippy.toml` (`allow-*-in-tests`).
- **No `unwrap`/`expect`/`panic!`/`println!`/`eprintln!`/`process::exit`/`#[allow]` in non-test code** — these are denied lints. Report errors by returning `Result`; allow them in tests via `clippy.toml`.
- **GitHub Actions:** SHA-pinned with `# vX.Y.Z` comments, `persist-credentials: false`, least-privilege `permissions`.
- **Commit `Cargo.lock`** (this is an application).
- **Conventional Commits**; end commit messages with the `Co-Authored-By` trailer.

---

## File Structure

```
Cargo.toml                          # workspace: members, package defaults, lints
rust-toolchain.toml                 # pin 1.88.0 + rustfmt, clippy
clippy.toml                         # msrv + allow-*-in-tests
deny.toml                           # cargo-deny: advisories, bans, licenses, sources
.gitignore
README.md                           # updated: build/dev instructions
LICENSE-MIT
LICENSE-APACHE
crates/tcpvisr-core/{Cargo.toml, src/lib.rs}
crates/tcpvisr-ingest/{Cargo.toml, src/lib.rs}
crates/tcpvisr-engine/{Cargo.toml, src/lib.rs}
crates/tcpvisr-enrich/{Cargo.toml, src/lib.rs}
crates/tcpvisr-tui/{Cargo.toml, src/lib.rs}
crates/tcp-visr/{Cargo.toml, src/main.rs, tests/cli.rs}
.github/workflows/ci.yml
.github/ISSUE_TEMPLATE/{epic.md, task.md}
.github/pull_request_template.md
```

---

## Task 1: Workspace, toolchain, lint config, crate stubs

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, `.gitignore`
- Create: `crates/tcpvisr-core/Cargo.toml`, `crates/tcpvisr-core/src/lib.rs` (and the same pair for `-ingest`, `-engine`, `-enrich`, `-tui`)
- Create: `crates/tcp-visr/Cargo.toml`, `crates/tcp-visr/src/main.rs`

**Interfaces:**
- Produces: a buildable workspace; the `tcp-visr` binary target (consumed by Task 2's test via `env!("CARGO_BIN_EXE_tcp-visr")`); the `[workspace.lints]` table (inherited by every crate).

- [ ] **Step 1: Create the workspace manifest**

Create `Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
edition = "2024"
rust-version = "1.88"
license = "MIT OR Apache-2.0"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
unwrap_used = "deny"
expect_used = "warn"
panic = "deny"
panic_in_result_fn = "deny"
unimplemented = "deny"
allow_attributes = "deny"
dbg_macro = "deny"
todo = "deny"
print_stdout = "deny"
print_stderr = "deny"
await_holding_lock = "deny"
large_futures = "deny"
exit = "deny"
mem_forget = "deny"
module_name_repetitions = "allow"
similar_names = "allow"
```

- [ ] **Step 2: Pin the toolchain**

Create `rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.88.0"
components = ["rustfmt", "clippy"]
```

- [ ] **Step 3: Configure clippy (MSRV + test relaxations)**

Create `clippy.toml`:

```toml
msrv = "1.88"
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-panic-in-tests = true
allow-dbg-in-tests = true
```

- [ ] **Step 4: Add `.gitignore`**

Create `.gitignore` (note: `Cargo.lock` is committed — this is an application):

```gitignore
/target
**/*.rs.bk
```

- [ ] **Step 5: Create the five stub library crates**

For each of `tcpvisr-core`, `tcpvisr-ingest`, `tcpvisr-engine`, `tcpvisr-enrich`, `tcpvisr-tui`, create `crates/<name>/Cargo.toml` (substitute the name):

```toml
[package]
name = "tcpvisr-core"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lints]
workspace = true
```

And `crates/<name>/src/lib.rs` with a one-line module doc matching the crate's design §3.1 role, e.g. for `tcpvisr-core`:

```rust
//! Shared types for tcp-visr: `FlowKey`, `ConnId`, `Item`, `Segment`, `MetricSample`,
//! time units, and serial-number arithmetic. See docs/design/tcp-visr-design.md §3.1.
```

Role lines for the others:
- `tcpvisr-ingest`: `//! Capture faucets: pcap/pcapng replay and libpcap live capture -> Item stream.`
- `tcpvisr-engine`: `//! Pure TCP connection state machine + metric derivation (no I/O).`
- `tcpvisr-enrich`: `//! Live-only kernel enrichment via sock_diag and /proc.`
- `tcpvisr-tui`: `//! ratatui master/detail UI, timeline cursor, and graph views.`

- [ ] **Step 6: Create the binary crate (trivial main for now)**

Create `crates/tcp-visr/Cargo.toml`:

```toml
[package]
name = "tcp-visr"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[[bin]]
name = "tcp-visr"
path = "src/main.rs"

[lints]
workspace = true
```

Create `crates/tcp-visr/src/main.rs`:

```rust
//! tcp-visr: visualize TCP flow over time from live capture or pcap replay.

fn main() {}
```

- [ ] **Step 7: Verify the workspace builds and is lint/format clean**

Run:

```bash
cargo build --workspace
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

Expected: all succeed; `cargo test` reports 0 tests. The first `cargo` call installs the pinned 1.88.0 toolchain via rustup.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml clippy.toml .gitignore crates/
git commit -m "chore: scaffold cargo workspace, toolchain, and lint policy

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: `tcp-visr` clap skeleton (TDD)

**Files:**
- Modify: `crates/tcp-visr/Cargo.toml` (add `clap`)
- Modify: `crates/tcp-visr/src/main.rs`
- Test: `crates/tcp-visr/tests/cli.rs`

**Interfaces:**
- Consumes: the `tcp-visr` binary target from Task 1.
- Produces: a CLI whose `--version` prints the crate version, `--help` prints usage including `tcp-visr` and `Usage`, and each subcommand (`replay`, `live`, `parse`, `conns`, `metrics`) exits non-zero with a message containing `not implemented`.

- [ ] **Step 1: Write the failing CLI integration test**

Create `crates/tcp-visr/tests/cli.rs`:

```rust
use std::process::Command;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tcp-visr"))
}

#[test]
fn version_flag_prints_version_and_exits_zero() -> TestResult {
    let output = bin().arg("--version").output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
    Ok(())
}

#[test]
fn help_flag_exits_zero_and_shows_usage() -> TestResult {
    let output = bin().arg("--help").output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("tcp-visr"));
    assert!(stdout.contains("Usage"));
    Ok(())
}

#[test]
fn unimplemented_subcommand_exits_nonzero_with_message() -> TestResult {
    let output = bin().arg("replay").output()?;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("not implemented"));
    Ok(())
}

#[test]
fn no_subcommand_exits_nonzero() -> TestResult {
    let output = bin().output()?;
    assert!(!output.status.success());
    Ok(())
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p tcp-visr --test cli`
Expected: FAIL — `replay` is not a known argument (clap not wired yet), so `unimplemented_subcommand...` and `no_subcommand...` fail (the trivial `main` exits 0 and ignores args).

- [ ] **Step 3: Add the `clap` dependency**

Run: `cargo add clap@=4.6.1 --features derive --package tcp-visr`
Verify `crates/tcp-visr/Cargo.toml` `[dependencies]` reads exactly:

```toml
[dependencies]
clap = { version = "=4.6.1", features = ["derive"] }
```

- [ ] **Step 4: Implement the clap skeleton**

Replace `crates/tcp-visr/src/main.rs`:

```rust
//! tcp-visr: visualize TCP flow over time from live capture or pcap replay.

use clap::{Parser, Subcommand};

/// Visualize TCP flow over time from a live system or a pcap/pcapng replay.
#[derive(Parser)]
#[command(name = "tcp-visr", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Replay a pcap/pcapng capture.
    Replay,
    /// Capture live from a network interface.
    Live,
    /// Print decoded TCP segments from a capture.
    Parse,
    /// List connections in a capture.
    Conns,
    /// Dump a connection's metric series.
    Metrics,
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::Replay => "replay",
            Command::Live => "live",
            Command::Parse => "parse",
            Command::Conns => "conns",
            Command::Metrics => "metrics",
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        None => Err("no subcommand given; run `tcp-visr --help`".into()),
        Some(command) => {
            Err(format!("`{}` is not implemented yet (see the milestone roadmap)", command.name()).into())
        }
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p tcp-visr --test cli`
Expected: PASS (4 tests).

- [ ] **Step 6: Verify format + lints are clean**

Run:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: clean. (Returning `Err` from `main` is not a panic; the `assert!`s live in test code allowed by `clippy.toml`.)

- [ ] **Step 7: Commit**

```bash
git add crates/tcp-visr/ Cargo.lock
git commit -m "feat(cli): add clap command surface with --help/--version and subcommand stubs

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Guardrails & meta — cargo-deny, CI, prek, licenses, README, templates

**Files:**
- Create: `deny.toml`, `.github/workflows/ci.yml`, `.pre-commit-config.yaml`
- Create: `LICENSE-MIT`, `LICENSE-APACHE`
- Create: `.github/ISSUE_TEMPLATE/epic.md`, `.github/ISSUE_TEMPLATE/task.md`, `.github/pull_request_template.md`
- Modify: `README.md`

**Interfaces:**
- Consumes: the buildable workspace + CLI from Tasks 1–2.
- Produces: enforced guardrails (`cargo deny check`, CI, prek) and contribution scaffolding.

- [ ] **Step 1: Add the cargo-deny config**

Create `deny.toml`:

```toml
[advisories]
version = 2

[bans]
multiple-versions = "warn"

[licenses]
version = 2
allow = [
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "Unicode-3.0",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
]

[sources]
unknown-registry = "deny"
unknown-git = "deny"
```

- [ ] **Step 2: Install cargo-deny and run the check**

Run:

```bash
cargo install cargo-deny --locked
cargo deny check
```

Expected: PASS. If a transitive license is reported that is not in `allow`, add that exact SPDX id to the `allow` list and re-run.

- [ ] **Step 3: Create the license files**

Run (sets the MIT holder/year; fetches the canonical Apache-2.0 text verbatim):

```bash
cat > LICENSE-MIT <<'EOF'
MIT License

Copyright (c) 2026 David Christensen

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
EOF
curl -fsSL https://www.apache.org/licenses/LICENSE-2.0.txt -o LICENSE-APACHE
```

Expected: both files created; `LICENSE-APACHE` is the full canonical Apache-2.0 text.

- [ ] **Step 4: Resolve action SHAs**

Run (record each SHA for Step 5):

```bash
gh api repos/actions/checkout/commits/v7.0.0 --jq .sha
gh api repos/Swatinem/rust-cache/commits/v2.9.1 --jq .sha
gh api repos/taiki-e/install-action/commits/v2.81.10 --jq .sha
```

- [ ] **Step 5: Create the CI workflow**

Create `.github/workflows/ci.yml`, substituting each `<sha-...>` with the value resolved in Step 4:

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

permissions:
  contents: read

concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: true

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@<sha-checkout>  # v7.0.0
        with:
          persist-credentials: false
      - run: rustup show
      - uses: Swatinem/rust-cache@<sha-rust-cache>  # v2.9.1
      - run: cargo fmt --all --check
      - run: cargo clippy --all-targets --all-features -- -D warnings
      - run: cargo test --workspace

  deny:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@<sha-checkout>  # v7.0.0
        with:
          persist-credentials: false
      - uses: taiki-e/install-action@<sha-install-action>  # v2.81.10
        with:
          tool: cargo-deny
      - run: cargo deny check
```

- [ ] **Step 6: Lint the workflow**

Run:

```bash
actionlint .github/workflows/ci.yml
zizmor .github/workflows/ci.yml
```

Expected: no errors. (`persist-credentials: false`, least-privilege `permissions`, and SHA-pinned actions satisfy zizmor's common checks.)

- [ ] **Step 7: Add the prek (pre-commit) hooks**

Create `.pre-commit-config.yaml`:

```yaml
repos:
  - repo: local
    hooks:
      - id: cargo-fmt
        name: cargo fmt
        entry: cargo fmt --all --check
        language: system
        types: [rust]
        pass_filenames: false
      - id: cargo-clippy
        name: cargo clippy
        entry: cargo clippy --all-targets --all-features -- -D warnings
        language: system
        types: [rust]
        pass_filenames: false
```

Run:

```bash
prek install
prek run --all-files
```

Expected: both hooks pass.

- [ ] **Step 8: Add issue and PR templates**

Create `.github/ISSUE_TEMPLATE/epic.md`:

```markdown
---
name: Epic (milestone)
about: A milestone (M0..M13) tracked with sub-issues
title: "Epic: M_ — <title>"
labels: ["type:epic"]
---

**Design ref:** docs/design/tcp-visr-design.md §10.M_
**Spec:** docs/milestones/m__-<slug>/spec.md
**Release:** v_._

## Definition of Done
- [ ] (copy the milestone's DoD from its spec.md)

## Sub-issues
- [ ] #
```

Create `.github/ISSUE_TEMPLATE/task.md`:

```markdown
---
name: Task (sub-issue)
about: A single task within a milestone epic
title: "<milestone>: <task>"
labels: ["type:task"]
---

**Epic:** #
**Touches:** area:

## Acceptance
- [ ] (testable outcome)
```

Create `.github/pull_request_template.md`:

```markdown
## Summary

<what this PR does — describe what the code does now>

Closes #<epic-or-task>

## Definition of Done
- [ ] `cargo fmt --all --check` clean
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` clean
- [ ] `cargo test --workspace` passes
- [ ] `cargo deny check` passes
- [ ] ADRs touched: <none | ADR-####>
```

- [ ] **Step 9: Update the README**

Replace `README.md` with:

```markdown
# tcp-visr

A Rust TUI for visualizing TCP flow over time, from a live Linux system or a pcap/pcapng
replay. See the design at [docs/design/tcp-visr-design.md](docs/design/tcp-visr-design.md).

## Status

Pre-release (v0.0.0). Building toward v0.1 (replay) per the
[milestone roadmap](docs/design/tcp-visr-design.md#10-milestone-roadmap).

## Build

```bash
cargo build --workspace
cargo run -p tcp-visr -- --help
```

## Develop

Requires Rust 1.88.0 (pinned via `rust-toolchain.toml`). Install hooks with `prek install`.

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check
```

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
```

- [ ] **Step 10: Full acceptance run**

Run every Definition-of-Done command:

```bash
cargo build --workspace
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check
cargo run -p tcp-visr -- --version
cargo run -p tcp-visr -- --help
prek run --all-files
```

Expected: all pass; `--version` prints `tcp-visr 0.0.0`; `--help` prints usage.

- [ ] **Step 11: Commit**

```bash
git add deny.toml .github/ .pre-commit-config.yaml LICENSE-MIT LICENSE-APACHE README.md
git commit -m "ci: add cargo-deny, CI, prek hooks, licenses, and templates

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage** (against `spec.md` DoD):
1. `cargo build` — Task 1 Step 7. ✓
2. `cargo fmt --check` — Tasks 1/2/3. ✓
3. `cargo clippy -D warnings` — Tasks 1/2/3. ✓
4. `cargo test` (CLI test green) — Task 2. ✓
5. `cargo deny check` — Task 3 Steps 1–2. ✓
6. `--help`/`--version` — Task 2 + Task 3 Step 10. ✓
7. `prek run` — Task 3 Step 7. ✓
8. CI present, actionlint/zizmor-clean, SHA-pinned, `persist-credentials: false` — Task 3 Steps 4–6. ✓
- Six crate stubs — Task 1 Steps 5–6. ✓  Lint set inherited — Task 1 Step 1. ✓  Templates — Task 3 Step 8 (ADR template pre-exists). ✓

**Placeholder scan:** The only `<...>` tokens are the action SHAs (Task 3 Step 5), resolved by explicit commands in Step 4 — actionable, not vague.

**Type consistency:** `Command::name(&self) -> &'static str` defined and used in Task 2 Step 4; the test (Step 1) depends only on process output, not internal types. `env!("CARGO_BIN_EXE_tcp-visr")` matches the bin name set in Task 1 Step 6.
