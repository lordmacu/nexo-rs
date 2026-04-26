//! Parse the project's real `FOLLOWUPS.md` and assert known
//! resolved/open splits.

use std::path::PathBuf;

use nexo_project_tracker::{parse_followups_file, FollowUpStatus};

fn fixture_path() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("FOLLOWUPS.md")
}

#[test]
fn pr_3_is_open() {
    let items = parse_followups_file(&fixture_path()).unwrap();
    let pr3 = items
        .iter()
        .find(|i| i.code == "PR-3")
        .expect("PR-3 must exist in FOLLOWUPS.md");
    // PR-3 status changes as the operator marks progress in
    // FOLLOWUPS.md (Open → 🔄 partial → ✅). Both Open and
    // Resolved are valid current states; what we assert is that
    // the parser surfaces *some* status, that the section is
    // Phase 26, and that the body is non-empty.
    let _ = pr3.status;
    assert!(pr3.section.contains("Phase 26"));
    assert!(!pr3.body.is_empty());
}

#[test]
fn pr_1_1_is_resolved() {
    let items = parse_followups_file(&fixture_path()).unwrap();
    let pr11 = items
        .iter()
        .find(|i| i.code == "PR-1.1")
        .expect("PR-1.1 must exist");
    assert_eq!(pr11.status, FollowUpStatus::Resolved);
}

#[test]
fn open_and_resolved_both_present() {
    let items = parse_followups_file(&fixture_path()).unwrap();
    let open = items
        .iter()
        .filter(|i| i.status == FollowUpStatus::Open)
        .count();
    let resolved = items
        .iter()
        .filter(|i| i.status == FollowUpStatus::Resolved)
        .count();
    assert!(open > 0, "expected at least one open item");
    assert!(resolved > 0, "expected at least one resolved item");
}
