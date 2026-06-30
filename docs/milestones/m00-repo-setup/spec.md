# M0 — Repo & Toolchain Setup (Spec)

> Implements: design §10.M0 · Depends-on: ADR-0001..0004 (structural only) ·
> Touches: `area:ci` `area:cli` `area:core` `area:docs` · Release: v0.1 · Type: `type:epic`

## Objective

Stand up the Cargo workspace, toolchain, lint/guardrail configuration, CI, and developer
hooks so that every later milestone lands in a repo where formatting, linting, and tests are
enforced from the first line of real code. M0 ships no product behavior beyond a CLI that
prints `--help`/`--version`.

## In scope

- Cargo workspace with the six crates from design §3.1 as stubs:
  `tcpvisr-core`, `tcpvisr-ingest`, `tcpvisr-engine`, `tcpvisr-enrich`, `tcpvisr-tui`, and the
  `tcp-visr` binary.
- `rust-toolchain.toml` pinned to the MSRV (1.88.0) with `rustfmt` + `clippy` components.
- Workspace-inherited lint configuration (the CLAUDE.md clippy set) via `[workspace.lints]`.
- `tcp-visr` binary: a `clap` skeleton exposing `--help` and `--version`, plus the v1
  subcommand names as stubs (`replay`, `live`, `parse`, `conns`, `metrics`) that exit with a
  "not implemented yet" message — so the command surface is fixed early.
- `cargo-deny` config (`deny.toml`): advisories, licenses, bans.
- GitHub Actions CI: `fmt --check`, `clippy -D warnings`, `test`, `cargo-deny check` — actions
  SHA-pinned with version comments, `persist-credentials: false`.
- `prek` (pre-commit) hooks running fmt + clippy locally.
- `LICENSE-MIT` + `LICENSE-APACHE` (dual MIT OR Apache-2.0), README update, `.gitignore`.
- `.github/` templates: epic issue template, task issue template, PR template (with the M0 DoD
  checklist pattern). The ADR template already exists at `docs/adr/0000-template.md`.

## Out of scope

- Any packet parsing, TCP logic, TUI rendering, or libpcap linkage (M1+).
- Publishing to crates.io, release binaries (M13).
- Creating GitHub labels/issues on the remote — done as a separate execution step with the
  user's go-ahead, not part of the code PR.

## Definition of Done

1. `cargo build --workspace` succeeds on the pinned toolchain.
2. `cargo fmt --all --check` is clean.
3. `cargo clippy --all-targets --all-features -- -D warnings` is clean.
4. `cargo test --workspace` passes (CLI integration test green).
5. `cargo deny check` passes (advisories, bans, licenses, sources).
6. `cargo run -p tcp-visr -- --help` and `--version` exit 0 with correct output.
7. `prek run --all-files` passes.
8. CI workflow is present, `actionlint`- and `zizmor`-clean, with SHA-pinned actions and
   `persist-credentials: false`.

## Task breakdown (→ sub-issues)

- **Task 1** — Workspace, toolchain, lint config, crate stubs. (`area:core`)
- **Task 2** — `tcp-visr` clap skeleton with `--help`/`--version` + subcommand stubs, TDD via a
  CLI integration test. (`area:cli`)
- **Task 3** — Guardrails & meta: `cargo-deny`, CI workflow, `prek` hooks, licenses, README,
  `.github/` templates. (`area:ci` `area:docs`)

Each task ends with an independently testable deliverable; all three compose into the single
M0 PR.

## Decisions & assumptions

- **License**: dual MIT OR Apache-2.0 (Rust ecosystem convention). Change before first publish
  if desired.
- **Toolchain = MSRV = 1.88.0**: building on the floor in CI doubles as MSRV verification.
- **Edition 2024, resolver 3**: available on 1.88; modern default.
- **Action SHA pinning**: SHAs resolved at execution time via `gh api` (see plan), never copied
  from memory.
- **`tcpvisr-ingest` libpcap dependency** is introduced later (M11) as an optional `live`
  feature (ADR-0003); M0's ingest stub has no dependencies.

## Acceptance verification commands

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
