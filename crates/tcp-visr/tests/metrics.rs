// `unwrap` in non-`#[test]` helpers: scope the relaxation to this file (matches conns.rs).
#![allow(clippy::unwrap_used)]

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tcp-visr"))
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn golden(stem: &str) -> String {
    let path = format!(
        "{}/tests/oracle/{stem}.metrics.json",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(path).unwrap()
}

fn metrics_ok(fixture_name: &str, conn: &str) -> String {
    let out = bin()
        .args(["metrics", &fixture(fixture_name), "--conn", conn])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "metrics {fixture_name} exited nonzero: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn seq_wrap_matches_golden() {
    assert_eq!(metrics_ok("seq_wrap.pcap", "0"), golden("seq_wrap"));
}

#[test]
fn metrics_basic_matches_golden() {
    assert_eq!(
        metrics_ok("metrics_basic.pcap", "0"),
        golden("metrics_basic")
    );
}

#[test]
fn metrics_retransmit_matches_golden() {
    assert_eq!(
        metrics_ok("metrics_retransmit.pcap", "0"),
        golden("metrics_retransmit")
    );
}

#[test]
fn metrics_ooo_matches_golden() {
    assert_eq!(metrics_ok("metrics_ooo.pcap", "0"), golden("metrics_ooo"));
}

#[test]
fn metrics_sack_matches_golden() {
    assert_eq!(metrics_ok("metrics_sack.pcap", "0"), golden("metrics_sack"));
}

#[test]
fn out_of_range_conn_exits_nonzero() {
    let out = bin()
        .args(["metrics", &fixture("seq_wrap.pcap"), "--conn", "99"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("out of range"), "{err}");
    assert!(err.contains("conns"), "{err}");
}

#[test]
fn missing_file_exits_nonzero() {
    let out = bin()
        .args(["metrics", "/nonexistent.pcap", "--conn", "0"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn zero_throughput_window_rejected() {
    let out = bin()
        .args([
            "metrics",
            &fixture("seq_wrap.pcap"),
            "--conn",
            "0",
            "--throughput-window-ms",
            "0",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("throughput-window-ms"), "{err}");
}

#[test]
#[ignore = "release gate: cross-check RTT/retransmit against tcptrace/Wireshark on the fixtures"]
fn tcptrace_cross_check() {
    // Run by maintainers before a release (no external tool in CI). For each fixture, run
    // `tcptrace -lr <fixture>` (or Wireshark TCP stream graphs) and confirm the RTT samples and
    // retransmit/OOO counts agree with the committed goldens, using the oldest-acked-per-
    // cumulative-ACK RTT definition (spec "RTT (Karn)"). Document the reference in the release
    // notes. This test intentionally does nothing in CI.
}
