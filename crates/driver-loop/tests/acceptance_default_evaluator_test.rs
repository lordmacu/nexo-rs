#![cfg(unix)]

use nexo_driver_loop::{AcceptanceEvaluator, DefaultAcceptanceEvaluator};
use nexo_driver_types::AcceptanceCriterion;

#[tokio::test]
async fn happy_path_two_passing_criteria() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::write(dir.path().join("README.md"), "ok\n")
        .await
        .unwrap();
    let e = DefaultAcceptanceEvaluator::new();
    let criteria = vec![
        AcceptanceCriterion::shell("echo OK"),
        AcceptanceCriterion::file("README.md", "ok"),
    ];
    let v = e.evaluate(&criteria, dir.path()).await.unwrap();
    assert!(v.met);
    assert!(v.failures.is_empty());
}

#[tokio::test]
async fn shell_failure_attaches_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let e = DefaultAcceptanceEvaluator::new();
    let criteria = vec![AcceptanceCriterion::shell("echo bad-output >&2 && exit 1")];
    let v = e.evaluate(&criteria, dir.path()).await.unwrap();
    assert!(!v.met);
    let f = &v.failures[0];
    assert_eq!(f.criterion_index, 0);
    let ev = f.evidence.as_deref().unwrap();
    assert!(ev.contains("bad-output"), "evidence:\n{ev}");
}

#[tokio::test]
async fn unknown_custom_verifier_returns_named_failure() {
    let dir = tempfile::tempdir().unwrap();
    let e = DefaultAcceptanceEvaluator::new();
    let criteria = vec![AcceptanceCriterion::Custom {
        name: "definitely_invented".into(),
        args: serde_json::Value::Null,
    }];
    let v = e.evaluate(&criteria, dir.path()).await.unwrap();
    assert!(!v.met);
    assert!(v.failures[0].message.contains("definitely_invented"));
}
