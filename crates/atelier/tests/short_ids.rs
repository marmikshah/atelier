//! A unique document-id prefix works on every call and never reaches the journal.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};

fn call(home: &PathBuf, tool: &str, args: Value) -> (bool, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_atelier"))
        .args(["call", tool, &args.to_string(), "--home"])
        .arg(home)
        .env("ATELIER_LOG", "off")
        .output()
        .unwrap();
    let report = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    (output.status.success(), report)
}

#[test]
fn short_ids_read_and_write_and_are_journaled_in_full() {
    let home = std::env::temp_dir().join(format!("atelier-short-ids-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let (ok, created) = call(&home, "doc_new", json!({"name":"d","width":4,"height":4}));
    assert!(ok, "{created}");
    let id = created["doc_id"].as_str().unwrap();
    let short = &id[..8];

    let (ok, info) = call(&home, "doc_info", json!({"doc_id": short}));
    assert!(ok, "{info}");
    assert_eq!(info["doc_id"], id, "results carry the full id");

    let (ok, drawn) = call(
        &home,
        "doc_draw",
        json!({"doc_id": short, "layer":0, "frame":0, "op":"fill_cel", "color":[1,2,3]}),
    );
    assert!(ok, "{drawn}");
    let journal = fs::read_to_string(home.join("documents").join(id).join("doc.recipe")).unwrap();
    assert!(
        journal.starts_with(&format!("atelier 3 {id} ")),
        "the header names the full id: {journal}"
    );
    assert!(!journal.contains(&format!("={short}")), "{journal}");

    let (ok, missing) = call(&home, "doc_info", json!({"doc_id":"00000000"}));
    assert!(!ok);
    assert!(
        missing["error"]
            .as_str()
            .unwrap()
            .contains("no document id starts with")
    );
    let _ = fs::remove_dir_all(&home);
}
