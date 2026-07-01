//! The committed M2 fixtures must byte-match the builder output (regenerate on change).
mod support;

#[test]
fn committed_fixtures_match_builder() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    for (name, bytes) in support::fixture_set() {
        let path = std::path::Path::new(dir).join(name);
        let on_disk =
            std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert_eq!(
            on_disk, bytes,
            "committed {name} is stale; regenerate fixtures"
        );
    }
}

#[test]
#[ignore = "regenerates committed fixtures; run explicitly"]
fn regenerate_fixtures() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    std::fs::create_dir_all(dir).unwrap();
    for (name, bytes) in support::fixture_set() {
        std::fs::write(std::path::Path::new(dir).join(name), bytes).unwrap();
    }
}

#[test]
fn committed_metrics_fixtures_match_builder() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    for (name, bytes) in support::metrics_fixture_set() {
        let path = std::path::Path::new(dir).join(name);
        let on_disk =
            std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert_eq!(
            on_disk, bytes,
            "committed {name} is stale; regenerate fixtures"
        );
    }
}

#[test]
#[ignore = "regenerates committed metrics fixtures; run explicitly after a reviewed change"]
fn regenerate_metrics_fixtures() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    std::fs::create_dir_all(dir).unwrap();
    for (name, bytes) in support::metrics_fixture_set() {
        std::fs::write(std::path::Path::new(dir).join(name), bytes).unwrap();
    }
}

#[test]
fn committed_name_fixtures_match_builder() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    for (name, bytes) in support::name_fixture_set() {
        let path = std::path::Path::new(dir).join(name);
        let on_disk =
            std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert_eq!(
            on_disk, bytes,
            "committed {name} is stale; regenerate fixtures"
        );
    }
}

#[test]
#[ignore = "regenerates the committed name fixture; run explicitly after a reviewed change"]
fn regenerate_name_fixtures() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    std::fs::create_dir_all(dir).unwrap();
    for (name, bytes) in support::name_fixture_set() {
        std::fs::write(std::path::Path::new(dir).join(name), bytes).unwrap();
    }
}

#[test]
fn committed_oracle_goldens_are_present_json() {
    // The goldens are byte-matched against live output by tests/metrics.rs; here we only assert
    // each exists and is non-empty JSON with a trailing newline, so an accidental deletion or
    // truncation is caught even if a fixture happened to produce empty output.
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/oracle");
    for stem in [
        "seq_wrap",
        "metrics_basic",
        "metrics_retransmit",
        "metrics_ooo",
        "metrics_sack",
    ] {
        let path = std::path::Path::new(dir).join(format!("{stem}.metrics.json"));
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert!(
            body.trim_start().starts_with('{'),
            "{stem} golden is not JSON"
        );
        assert!(
            body.ends_with('\n'),
            "{stem} golden must end with a newline"
        );
    }
}
