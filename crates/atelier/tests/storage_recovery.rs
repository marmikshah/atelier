//! Damaged documents are refused by tools and replaced only by `library reset`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

static STORE_ID: AtomicUsize = AtomicUsize::new(0);

struct Store(PathBuf);

impl Store {
    fn new() -> Self {
        loop {
            let id = STORE_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("atelier-recovery-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create recovery test store: {error}"),
            }
        }
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

    /// Run `library reset <id>` with any extra flags; returns success and stdout.
    fn reset(&self, id: &str, extra: &[&str]) -> (bool, String) {
        let output = Command::new(env!("CARGO_BIN_EXE_atelier"))
            .args(["library", "reset", id])
            .args(extra)
            .arg("--home")
            .arg(&self.0)
            .env("ATELIER_LOG", "off")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
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
fn unsupported_and_corrupt_metadata_is_refused_until_an_explicit_reset() {
    for corruption in [
        "missing schema",
        "unsupported marker",
        "future schema",
        "invalid JSON",
    ] {
        let store = Store::new();
        let id = store.create();
        let path = store.document(&id).join("doc.json");
        let mut meta = read_json(&path);
        match corruption {
            "missing schema" => {
                meta.as_object_mut().unwrap().remove("schema");
            }
            "unsupported marker" => {
                let schema = meta.as_object_mut().unwrap().remove("schema").unwrap();
                meta["format_version"] = schema;
            }
            "future schema" => meta["schema"] = json!(99),
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
        assert!(!ok, "{corruption}: {result}");
        let error = result["error"].as_str().unwrap();
        assert!(error.contains("left unchanged"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), before, "tools never replace data");

        // Without a terminal or --yes the reset refuses, like `library rm`.
        assert!(!store.reset(&id, &[]).0);
        assert_eq!(fs::read(&path).unwrap(), before);

        // Intact work in another format is never pointed at reset, and reset
        // keeps it unless explicitly told to discard it.
        let confirm: &[&str] = if corruption == "future schema" {
            assert!(error.contains("does not read"), "{error}");
            assert!(!error.contains("library reset"), "{error}");
            assert!(!store.reset(&id, &["--yes"]).0);
            assert_eq!(fs::read(&path).unwrap(), before);
            &["--yes", "--discard-unsupported"]
        } else {
            assert!(error.contains("library reset"), "{error}");
            &["--yes"]
        };
        let (ok, report) = store.reset(&id, confirm);
        assert!(ok, "{report}");
        let report: Value = serde_json::from_str(&report).unwrap();
        assert_eq!(report["doc_id"], id);
        let (ok, result) = store.call("doc_info", json!({"doc_id":id}));
        assert!(ok, "{result}");
        assert_eq!(result["revision"], 0);
        assert_eq!(read_json(&path)["schema"], 1);
        assert!(read_json(&path).get("format_version").is_none());
    }
}

#[test]
fn reset_of_broken_cels_revisions_and_journals_keeps_the_named_canvas_and_uuid() {
    for component in ["cels/L0_F0.png", "revision", "doc.recipe"] {
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
        } else if component == "doc.recipe" {
            "doc_draw"
        } else {
            "doc_info"
        };
        let args = match tool {
            "doc_draw" => json!({"doc_id":id,"layer":0,"frame":0,"op":"clear_cel"}),
            "doc_dump_region" => json!({"doc_id":id,"layer":0,"frame":0,"region":[0,0,0,0]}),
            _ => json!({"doc_id":id}),
        };
        let (ok, refused) = store.call(tool, args);
        assert!(!ok, "{component}: {refused}");
        assert_eq!(
            fs::read(store.document(&id).join(component)).unwrap(),
            b"broken\n",
            "{component} stays as found"
        );
        let (ok, report) = store.reset(&id, &["--yes"]);
        assert!(ok, "{component}: {report}");
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
fn failed_reset_writes_leave_the_saved_generation_untouched() {
    let store = Store::new();
    let id = store.create();
    let path = store.document(&id).join("doc.json");
    fs::write(&path, "broken").unwrap();
    fs::remove_dir(store.0.join("documents/.transactions")).unwrap();
    fs::write(store.0.join("documents/.transactions"), "blocks staging").unwrap();
    assert!(!store.reset(&id, &["--yes"]).0);
    assert_eq!(fs::read(&path).unwrap(), b"broken");
    fs::remove_file(store.0.join("documents/.transactions")).unwrap();
    assert!(store.reset(&id, &["--yes"]).0);
    assert!(store.call("doc_info", json!({"doc_id":id})).0);
}

#[test]
fn reset_leaves_healthy_and_missing_documents_alone() {
    let store = Store::new();
    let id = store.create();
    let path = store.document(&id).join("doc.json");
    let before = fs::read(&path).unwrap();
    let (ok, output) = store.reset(&id, &["--yes"]);
    assert!(ok, "{output}");
    assert!(output.contains("nothing to reset"), "{output}");
    assert_eq!(fs::read(&path).unwrap(), before);
    let (ok, output) = store.reset("00000000-0000-4000-8000-000000000000", &["--yes"]);
    assert!(ok, "{output}");
    assert!(output.contains("nothing to reset"), "{output}");
}
