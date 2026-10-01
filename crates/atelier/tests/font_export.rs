//! CLI, replay, and persisted font metadata must produce the same font bytes.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

fn call(home: &Path, tool: &str, args: Value) -> Output {
    Command::new(env!("CARGO_BIN_EXE_atelier"))
        .args(["call", tool, &args.to_string()])
        .env("ATELIER_HOME", home)
        .env("ATELIER_LOG", "off")
        .output()
        .unwrap()
}

fn ok(home: &Path, tool: &str, args: Value) -> Value {
    let output = call(home, tool, args);
    assert!(
        output.status.success(),
        "{tool}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn cli_font_export_replays_identically_and_guards_metadata_updates() {
    let root = std::env::temp_dir().join(format!("atelier-cli-font-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("source");
    let created = ok(
        &home,
        "doc_new",
        json!({"name":"pixel font","width":2,"height":1}),
    );
    let id = created["doc_id"].as_str().unwrap();
    ok(
        &home,
        "doc_draw",
        json!({"doc_id":id,"layer":0,"frame":0,"op":"fill_cel","color":[10,20,30]}),
    );
    let font = json!({
        "family":"CLI Pixel", "baseline":1,"ascent":1,"descent":0,
        "missing_glyph":0,"space_glyph":1,
        "glyphs":[
            {"name":"missing","codepoints":[],"rect":[0,0,1,1],"advance":1},
            {"name":"space","codepoints":[32],"rect":null,"advance":1},
            {"name":"A","codepoints":[65,97,128512],"rect":[1,0,1,1],"advance":2}
        ]
    });
    let configured = ok(
        &home,
        "doc_font",
        json!({"doc_id":id,"op":"set","font":font,"expected_revision":2}),
    );
    assert_eq!(configured["revision"], 3);
    assert_eq!(
        ok(&home, "doc_font", json!({"doc_id":id,"op":"get"}))["revision"],
        3
    );
    let stale = call(
        &home,
        "doc_font",
        json!({"doc_id":id,"op":"clear","expected_revision":2}),
    );
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stdout).contains("revision_conflict"));
    let mut invalid = font.clone();
    invalid["glyphs"][2]["codepoints"] = json!([32]);
    assert!(
        !call(
            &home,
            "doc_font",
            json!({"doc_id":id,"op":"set","font":invalid,"expected_revision":3})
        )
        .status
        .success()
    );
    let current = ok(&home, "doc_font", json!({"doc_id":id,"op":"get"}));
    assert_eq!(current["revision"], 3);
    assert_eq!(
        current["font"]["glyphs"][2]["codepoints"],
        font["glyphs"][2]["codepoints"]
    );
    let original = root.join("original.ttf");
    let report = ok(
        &home,
        "doc_export",
        json!({"doc_id":id,"op":"font","out_path":original}),
    );
    assert_eq!(report["format"], "ttf");
    assert_eq!(report["glyphs"], 3);
    let bytes = std::fs::read(&original).unwrap();
    assert_eq!(&bytes[..4], &[0, 1, 0, 0]);
    for (field, value) in [
        ("scale", json!(1)),
        ("meta", json!("atelier")),
        ("format", json!("gif")),
        ("tag", json!("unused")),
        ("color_mode", json!("rgb")),
    ] {
        let mut args = json!({"doc_id":id,"op":"font","out_path":original});
        args[field] = value;
        assert!(
            !call(&home, "doc_export", args).status.success(),
            "accepted {field}"
        );
        assert_eq!(std::fs::read(&original).unwrap(), bytes);
    }
    let journal = home.join("documents").join(id).join("recipe.jsonl");
    let lines = std::fs::read_to_string(&journal).unwrap();
    assert_eq!(
        lines.lines().count(),
        3,
        "get, failed writes, and export must not enter the recipe"
    );
    let replay_home = root.join("replayed");
    let replayed = Command::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("replay")
        .arg(&journal)
        .arg("--home")
        .arg(&replay_home)
        .env("ATELIER_HOME", &home)
        .env("ATELIER_LOG", "off")
        .output()
        .unwrap();
    assert!(
        replayed.status.success(),
        "{}",
        String::from_utf8_lossy(&replayed.stderr)
    );
    let docs = ok(&replay_home, "list_docs", json!({}));
    let replay_id = docs["documents"][0]["doc_id"].as_str().unwrap();
    let replay_font = root.join("replayed.ttf");
    ok(
        &replay_home,
        "doc_export",
        json!({"doc_id":replay_id,"op":"font","out_path":replay_font}),
    );
    assert_eq!(std::fs::read(replay_font).unwrap(), bytes);
    ok(
        &home,
        "doc_font",
        json!({"doc_id":id,"op":"clear","expected_revision":3}),
    );
    assert!(ok(&home, "doc_info", json!({"doc_id":id}))["font"].is_null());
    assert!(
        !call(
            &home,
            "doc_export",
            json!({"doc_id":id,"op":"font","out_path":original})
        )
        .status
        .success()
    );
    assert_eq!(std::fs::read(&original).unwrap(), bytes);
    std::fs::remove_dir_all(root).unwrap();
}
