//! End-to-end parse against the project's real `PHASES.md`. The
//! fixture path is resolved from `CARGO_MANIFEST_DIR` so the test
//! works regardless of which directory `cargo test` is invoked from.

use std::path::PathBuf;

use nexo_project_tracker::{parse_phases_file, PhaseStatus};

fn fixture_path() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/project-tracker → walk two parents to reach the workspace.
    manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("PHASES.md")
}

#[test]
fn finds_phase_67_and_subphase_67_9_done() {
    let path = fixture_path();
    assert!(
        path.exists(),
        "fixture missing at {} — adjust `fixture_path` if PHASES.md moved",
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
    // 67.0 .. 67.13 inclusive — 14 subphases. If a future commit adds
    // more, bump this assertion deliberately.
    assert!(
        p67.sub_phases.len() >= 14,
        "expected >=14 subphases for Phase 67, got {}",
        p67.sub_phases.len()
    );
}
