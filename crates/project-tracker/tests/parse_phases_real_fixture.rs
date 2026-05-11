//! End-to-end parse against the bundled `tests/fixtures/PHASES.md` —
//! a trimmed, synthetic copy of the real file's dialect (the real
//! roadmap isn't part of the published crate). The path is resolved
//! from `CARGO_MANIFEST_DIR` so the test works regardless of which
//! directory `cargo test` is invoked from.

use std::path::PathBuf;

use nexo_project_tracker::{parse_phases_file, PhaseStatus};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/PHASES.md")
}

#[test]
fn finds_phase_67_and_subphase_67_9_done() {
    let path = fixture_path();
    assert!(
        path.exists(),
        "fixture missing at {} — it ships under crates/project-tracker/tests/fixtures/",
        path.display()
    );
    let phases = parse_phases_file(&path).unwrap();

    let p67 = phases
        .iter()
        .find(|p| p.id == "67")
        .expect("Phase 67 must be parsed");
    let s679 = p67
        .sub_phases
        .iter()
        .find(|s| s.id == "67.9")
        .expect("67.9 must be parsed");
    assert_eq!(s679.status, PhaseStatus::Done, "67.9 is shipped");

    let s6710 = p67
        .sub_phases
        .iter()
        .find(|s| s.id == "67.10")
        .expect("67.10 must be parsed");
    assert_eq!(s6710.status, PhaseStatus::Pending);
}

#[test]
fn phase_67_subphase_count_matches_committed_state() {
    let phases = parse_phases_file(&fixture_path()).unwrap();
    let p67 = phases.iter().find(|p| p.id == "67").unwrap();
    // The fixture declares 67.0 .. 67.13 inclusive — 14 subphases.
    assert!(
        p67.sub_phases.len() >= 14,
        "expected >=14 subphases for Phase 67, got {}",
        p67.sub_phases.len()
    );
}
