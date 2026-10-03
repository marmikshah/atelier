//! Recovery goes through the shipped CLI and the shared MCP dispatch path.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

struct Store(PathBuf);

impl Store {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self(std::env::temp_dir().join(format!("atelier-recovery-{}-{nonce}", std::process::id())))
    }

    fn call(&self, tool: &str, args: Value) -> (bool, Value) {
        let output = Command::new(env!("CARGO_BIN_EXE_atelier"))
            .args(["call", tool, &args.to_string(), "--home"])
            .arg(&self.0)
            .env("ATELIER_LOG", "off")
            .output()
            .unwrap();
        let report = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
        (output.status.success(), report)
    }

    fn create(&self) -> String {
        let (ok, result) = self.call(
            "doc_new",
            json!({"name":"named canvas", "width":8, "height":6}),
        );
        assert!(ok, "{result}");
        result["doc_id"].as_str().unwrap().into()
    }

    fn document(&self, id: &str) -> PathBuf {
        self.0.join("documents").join(id)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn unsupported_and_corrupt_metadata_is_replaced_in_the_current_format() {
    for corruption in ["missing version", "future version", "invalid JSON"] {
        let store = Store::new();
        let id = store.create();
        let path = store.document(&id).join("doc.json");
        let mut meta = read_json(&path);
        match corruption {
            "missing version" => {
                meta.as_object_mut().unwrap().remove("format_version");
            }
            "future version" => meta["format_version"] = json!(99),
            _ => {}
        }
        fs::write(
            &path,
            if corruption == "invalid JSON" {
                "broken".into()
            } else {
                meta.to_string()
            },
        )
        .unwrap();
        let before = fs::read(&path).unwrap();
        let verify = Command::new(env!("CARGO_BIN_EXE_atelier"))
            .args(["library", "verify", "--json", "--home"])
            .arg(&store.0)
            .output()
            .unwrap();
        assert!(!verify.status.success());
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "verification stays read-only"
        );

        let (ok, result) = store.call("doc_info", json!({"doc_id":id}));
        assert!(ok, "{result}");
        assert_eq!(result["doc_id"], id);
        assert_eq!(result["recovery"]["doc_id"], id);
        assert_eq!(result["revision"], 0);
        assert_eq!(read_json(&path)["format_version"], 1);
        assert_eq!(
            store.call("doc_info", json!({"doc_id":id})).1["recovery"],
            Value::Null
        );
    }
}

#[test]
fn broken_cels_revisions_and_journals_keep_the_named_canvas_and_uuid() {
    for component in ["cels/L0_F0.png", "revision", "recipe.jsonl"] {
        let store = Store::new();
        let id = store.create();
        assert!(
            store
                .call(
                    "doc_draw",
                    json!({
                        "doc_id":id, "layer":0, "frame":0, "op":"fill_cel", "color":[9,8,7],
                    })
                )
                .0
        );
        fs::write(store.document(&id).join(component), "broken\n").unwrap();
        let tool = if component.starts_with("cels/") {
            "doc_dump_region"
        } else if component == "recipe.jsonl" {
            "doc_draw"
        } else {
            "doc_info"
        };
        let args = match tool {
            "doc_draw" => json!({"doc_id":id,"layer":0,"frame":0,"op":"clear_cel"}),
            "doc_dump_region" => json!({"doc_id":id,"layer":0,"frame":0,"region":[0,0,0,0]}),
            _ => json!({"doc_id":id}),
        };
        let (ok, recovered) = store.call(tool, args);
        assert!(ok, "{component}: {recovered}");
        assert_eq!(recovered["recovery"]["doc_id"], id);
        let (ok, result) = store.call("doc_info", json!({"doc_id":id}));
        assert!(ok, "{result}");
        assert_eq!(result["name"], "named canvas");
        assert_eq!(result["w"], 8);
        assert_eq!(result["h"], 6);
        assert_eq!(result["cels"], json!([]));
        assert!(
            store
                .call(
                    "doc_draw",
                    json!({"doc_id":id,"layer":0,"frame":0,"op":"fill_cel","color":"[1,2,3]"})
                )
                .1
                .get("error")
                .is_some()
        );
    }
}

#[test]
fn failed_recovery_writes_leave_the_saved_generation_untouched() {
    let store = Store::new();
    let id = store.create();
    let path = store.document(&id).join("doc.json");
    fs::write(&path, "broken").unwrap();
    fs::remove_dir(store.0.join("documents/.transactions")).unwrap();
    fs::write(store.0.join("documents/.transactions"), "blocks staging").unwrap();
    let (ok, result) = store.call("doc_info", json!({"doc_id":id}));
    assert!(!ok, "{result}");
    assert!(result["error"].as_str().unwrap().contains("recover"));
    assert_eq!(fs::read(&path).unwrap(), b"broken");
    fs::remove_file(store.0.join("documents/.transactions")).unwrap();
    assert!(store.call("doc_info", json!({"doc_id":id})).0);
}
