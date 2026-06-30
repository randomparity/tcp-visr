//! Drift guard: the committed `tests/fixtures/*` must byte-match the in-repo builder, so the
//! fixtures stay reviewable as source. Regenerate with `UPDATE_FIXTURES=1 cargo test -p
//! tcpvisr-ingest --test drift`.

mod support;

use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn committed_fixtures_match_builder() {
    let dir = fixtures_dir();
    let update = std::env::var_os("UPDATE_FIXTURES").is_some();
    if update {
        std::fs::create_dir_all(&dir).expect("create fixtures dir");
    }
    for (name, bytes) in support::fixture_set() {
        let path = dir.join(name);
        if update {
            std::fs::write(&path, &bytes).expect("write fixture");
            continue;
        }
        let committed = std::fs::read(&path).unwrap_or_else(|e| {
            panic!("missing fixture {name} ({e}); run UPDATE_FIXTURES=1 to regenerate")
        });
        assert_eq!(
            committed, bytes,
            "committed fixture {name} drifted from the builder"
        );
    }
}
