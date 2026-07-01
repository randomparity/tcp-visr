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
