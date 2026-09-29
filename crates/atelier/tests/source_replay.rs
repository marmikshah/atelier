//! User-facing regression: a source build remains editable and replayable.
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Area(PathBuf);
impl Area {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("atelier-source-cli-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Area {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn invoke(args: &[&str], home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_atelier"))
        .args(args)
        .arg("--home")
        .arg(home)
        .env("TOKIO_WORKER_THREADS", "2")
        .output()
        .unwrap()
}
fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn source(area: &Area) -> PathBuf {
    let path = area.0.join("art.toml");
    fs::write(
        &path,
        r##"format=1
renderer=1
name="Ember"
canvas=[4,4]
layers=[{id="art",name="Artwork"}]
cels=[{layer="art",frames=[0],part="tip"}]
[parts.tip]
size=[4,4]
grid=".x..\n.xx.\n....\n....\n"
legend={x="#ffaa0033"}
"##,
    )
    .unwrap();
    path
}

#[test]
fn edit_replay_and_migrate_a_source_backed_document() {
    let area = Area::new();
    let path = source(&area);
    let home = area.0.join("store");
    let built: Value =
        serde_json::from_str(&success(invoke(&["replay", path.to_str().unwrap()], &home))).unwrap();
    let id = built["doc_id"].as_str().unwrap();
    // Editing the authoring file must not change an earlier working snapshot.
    fs::write(
        area.0.join("edits.json"),
        json!({"parts":{"tip":{"size":[4,4],"grid":"....\n....\n....\n....\n"}}}).to_string(),
    )
    .unwrap();
    let edited: Value = serde_json::from_str(&success(
        Command::new(env!("CARGO_BIN_EXE_atelier"))
            .current_dir(&area.0)
            .args([
                "source",
                "apply",
                "art.toml",
                "--file",
                "edits.json",
                "--expected-revision",
                built["source_revision"].as_str().unwrap(),
            ])
            .env("TOKIO_WORKER_THREADS", "2")
            .output()
            .unwrap(),
    ))
    .unwrap();
    assert!(edited.get("warning").is_none(), "{edited}");
    success(invoke(&["call","doc_draw",&json!({"doc_id":id,"layer":0,"frame":0,"op":"pencil","points":[[3,3]],"color":[12,34,56,255]}).to_string()],&home));
    success(invoke(&["replay", id], &home));
    let directories: Vec<_> = fs::read_dir(home.join("documents"))
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            p.join("doc.json").is_file().then_some(p)
        })
        .collect();
    assert_eq!(directories.len(), 2);
    let original = home.join("documents").join(id);
    let rebuilt = directories.iter().find(|p| **p != original).unwrap();
    assert_eq!(
        fs::read(original.join("doc.json")).unwrap(),
        fs::read(rebuilt.join("doc.json")).unwrap()
    );
    assert_eq!(
        fs::read(original.join("cels/L0_F0.png")).unwrap(),
        fs::read(rebuilt.join("cels/L0_F0.png")).unwrap()
    );
    let bundle = area.0.join("migrated");
    let report: Value = serde_json::from_str(&success(invoke(
        &["source", "migrate", id, bundle.to_str().unwrap()],
        &home,
    )))
    .unwrap();
    assert_eq!(report["pixels_equal"], true);
    assert!(bundle.join("source.toml").is_file());
    assert!(
        !fs::read_to_string(bundle.join("source.toml"))
            .unwrap()
            .contains(id)
    );
}

#[test]
fn source_backed_replay_failure_publishes_no_document() {
    let area = Area::new();
    let path = source(&area);
    let home = area.0.join("store");
    let built: Value =
        serde_json::from_str(&success(invoke(&["replay", path.to_str().unwrap()], &home))).unwrap();
    let id = built["doc_id"].as_str().unwrap();
    let journal = home.join("documents").join(id).join("recipe.jsonl");
    let mut text = fs::read_to_string(&journal).unwrap();
    text.push_str(
        &json!({"tool":"doc_draw","args":{"doc_id":id,"layer":0,"frame":0,"op":"does-not-exist"}})
            .to_string(),
    );
    text.push('\n');
    fs::write(&journal, text).unwrap();
    let target = area.0.join("empty");
    assert!(
        !invoke(&["replay", journal.to_str().unwrap()], &target)
            .status
            .success()
    );
    assert!(
        fs::read_dir(target.join("documents")).unwrap().all(|e| !e
            .unwrap()
            .path()
            .join("doc.json")
            .is_file())
    );
}
