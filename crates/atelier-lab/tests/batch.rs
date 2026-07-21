//! Exercise the real command-policy boundary and prove a repeated batch run
//! verifies and skips already completed task/policy/seed keys.

#[cfg(unix)]
#[test]
fn batch_command_resumes_without_duplicate_model_calls() {
    use std::path::PathBuf;
    use std::process::Command;

    let root = std::env::temp_dir().join(format!("atelier-lab-batch-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasks/development.jsonl");
    let policy = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("policy/example_policy.py");

    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_atelier-lab"))
            .arg("run-batch")
            .arg(&manifest)
            .arg(&root)
            .args(["--policy", "python3", "--policy-arg"])
            .arg(&policy)
            .args([
                "--name",
                "deterministic-fixture",
                "--limit",
                "2",
                "--max-turns",
                "5",
                "--timeout-secs",
                "5",
            ])
            .output()
            .unwrap()
    };

    let first = invoke();
    assert!(
        first.status.success(),
        "first batch failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let records = atelier_lab::read_batch_records(&root).unwrap();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|record| record.completed));
    assert_eq!(atelier_lab::completed_batch_keys(&root).unwrap().len(), 2);

    let second = invoke();
    assert!(
        second.status.success(),
        "resume failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let summary: serde_json::Value = String::from_utf8(second.stdout)
        .unwrap()
        .lines()
        .last()
        .map(serde_json::from_str)
        .unwrap()
        .unwrap();
    assert_eq!(summary["attempted"], 0);
    assert_eq!(summary["skipped_completed"], 2);
    assert_eq!(atelier_lab::read_batch_records(&root).unwrap().len(), 2);

    let _ = std::fs::remove_dir_all(root);
}
