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
