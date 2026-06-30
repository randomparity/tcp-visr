// `unwrap` in the non-`#[test]` helpers below: clippy's in-test exemption only reaches
// `#[test]` bodies, so scope the relaxation to this test file (matches M1's tests/parity.rs).
#![allow(clippy::unwrap_used)]

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tcp-visr"))
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn conns(name: &str) -> String {
    let out = bin().args(["conns", &fixture(name)]).output().unwrap();
    assert!(out.status.success(), "conns {name} exited nonzero");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn mid_stream_is_one_established_inferred_connection() {
    let o = conns("mid_stream.pcap");
    assert!(o.contains("state=Established"), "{o}");
    assert!(o.contains("(mid-stream)"), "{o}");
    assert!(o.contains("1 connections"), "{o}");
}

#[test]
fn sim_open_reaches_established_not_inferred() {
    let o = conns("sim_open.pcap");
    assert!(o.contains("state=Established"), "{o}");
    assert!(!o.contains("(mid-stream)"), "{o}");
}

#[test]
fn mid_rst_is_reset() {
    assert!(conns("mid_rst.pcap").contains("state=Reset"));
}

#[test]
fn tuple_reuse_lists_two_instances() {
    let o = conns("tuple_reuse.pcap");
    assert!(o.contains("inst=0"), "{o}");
    assert!(o.contains("inst=1"), "{o}");
    assert!(o.contains("2 connections"), "{o}");
}

#[test]
fn seq_wrap_stays_one_connection() {
    assert!(conns("seq_wrap.pcap").contains("1 connections"));
}

#[test]
fn missing_file_exits_nonzero_with_actionable_message() {
    let out = bin()
        .args(["conns", "/no/such/file.pcap"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("opening capture")
    );
}
