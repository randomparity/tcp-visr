use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tcp-visr"))
}

#[test]
fn version_flag_prints_version_and_exits_zero() {
    let output = bin().arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_flag_exits_zero_and_shows_usage() {
    let output = bin().arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("tcp-visr"));
    assert!(stdout.contains("Usage"));
}

#[test]
fn unimplemented_subcommand_exits_nonzero_with_message() {
    let output = bin().arg("replay").output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("not implemented"));
}

#[test]
fn no_subcommand_exits_nonzero() {
    let output = bin().output().unwrap();
    assert!(!output.status.success());
}

#[test]
fn parse_prints_segments_and_skip_summary() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tcpvisr-ingest/tests/fixtures/ethernet.pcap"
    );
    let output = bin().args(["parse", fixture]).output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("->"),
        "expected a decoded segment line: {stdout}"
    );
    assert!(
        stdout.contains("skipped"),
        "expected a skip summary: {stdout}"
    );
}

#[test]
fn parse_skip_fixture_exits_zero_and_counts_skips() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tcpvisr-ingest/tests/fixtures/skip.pcap"
    );
    let output = bin().args(["parse", fixture]).output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("1 segments"),
        "one TCP segment decoded: {stdout}"
    );
}

#[test]
fn parse_missing_file_exits_nonzero_with_actionable_message() {
    let output = bin()
        .args(["parse", "/no/such/file.pcap"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("opening capture"),
        "actionable error: {stderr}"
    );
}
