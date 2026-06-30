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
