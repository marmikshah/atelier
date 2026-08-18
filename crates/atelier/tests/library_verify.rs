//! End-to-end contract for the document-store integrity command.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use atelier_studio::Studio;
use serde_json::Value;

fn isolated_home() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "atelier-library-verify-{}-{nonce}",
        std::process::id()
    ))
}

fn verify(home: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_atelier"))
        .args(["library", "verify", "--json"])
        .env("ATELIER_HOME", home)
        .env("ATELIER_LOG", "off")
        .output()
        .expect("run atelier library verify")
}

#[test]
fn library_verify_emits_json_and_uses_failure_status_for_corruption() {
    let home = isolated_home();
    let studio = Studio::with_home(home.clone());
    let created = studio.doc_new("integrity", 8, 8).unwrap();
    let id = created["doc_id"].as_str().unwrap();

    let clean = verify(&home);
    assert!(
        clean.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&clean.stderr)
    );
    let report: Value = serde_json::from_slice(&clean.stdout).expect("JSON verification report");
    assert_eq!(report["ok"], true);
    assert_eq!(report["documents"], 1);
    assert_eq!(report["errors"], 0);

    std::fs::write(home.join("documents").join(id).join("doc.json"), "{}\n").unwrap();
    let broken = verify(&home);
    assert_eq!(broken.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&broken.stdout).expect("JSON failure report");
    assert_eq!(report["ok"], false);
    assert!(report["errors"].as_u64().unwrap() >= 1);
    assert!(
        report["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| { issue["document_id"] == id && issue["component"] == "doc.json" })
    );

    let usage = Command::new(env!("CARGO_BIN_EXE_atelier"))
        .args(["library", "verify", "--unknown"])
        .env("ATELIER_HOME", &home)
        .env("ATELIER_LOG", "off")
        .output()
        .expect("run malformed verify command");
    assert_eq!(usage.status.code(), Some(2));

    let _ = std::fs::remove_dir_all(home);
}
