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
    let path = area.0.join("recipe.atelier");
    fs::write(
        &path,
        r##"atelier "Ember" format=1 renderer=1
canvas 4 4
layer "Artwork" {
  frame 0 {
    pixels {
      legend { color "x" "#ffaa0033"; }
      grid ".x..\n.xx.\n....\n....\n"
    }
  }
}
"##,
    )
    .unwrap();
    path
}

#[test]
fn edits_replace_the_recipe_and_replay_preserves_exact_state() {
    let area = Area::new();
    let path = source(&area);
    let home = area.0.join("store");
    let built: Value =
        serde_json::from_str(&success(invoke(&["replay", path.to_str().unwrap()], &home))).unwrap();
    let id = built["doc_id"].as_str().unwrap();
    // The working document owns its snapshot of the authored recipe.
    fs::write(&path, "broken original").unwrap();
    for value in 1..=12 {
        success(invoke(&["call","doc_draw",&json!({"doc_id":id,"layer":0,"frame":0,"op":"pencil","points":[[3,3]],"color":[12,34,value,255]}).to_string()],&home));
    }
    let original = home.join("documents").join(id);
    assert!(!original.join("recipe.jsonl").exists());
    assert!(
        fs::metadata(original.join("recipe/recipe.atelier"))
            .unwrap()
            .len()
            < 1000
    );
    let rebuilt: Value = serde_json::from_str(&success(invoke(&["replay", id], &home))).unwrap();
    let rebuilt = home
        .join("documents")
        .join(rebuilt["doc_id"].as_str().unwrap());
    for file in ["doc.json", "cels/L0_F0.png", "recipe/recipe.atelier"] {
        assert_eq!(
            fs::read(original.join(file)).unwrap(),
            fs::read(rebuilt.join(file)).unwrap()
        );
    }
    let bundle = area.0.join("export");
    let report: Value = serde_json::from_str(&success(invoke(
        &["migrate", id, bundle.to_str().unwrap()],
        &home,
    )))
    .unwrap();
    assert_eq!(report["pixels_equal"], true);
    assert!(
        !fs::read_to_string(bundle.join("recipe.atelier"))
            .unwrap()
            .contains(id)
    );
}

#[test]
fn invalid_recipe_publishes_no_document_and_jsonl_is_import_only() {
    let area = Area::new();
    let path = source(&area);
    let home = area.0.join("store");
    let broken = fs::read_to_string(&path)
        .unwrap()
        .replace("#ffaa0033", "bad");
    fs::write(&path, broken).unwrap();
    assert!(
        !invoke(&["replay", path.to_str().unwrap()], &home)
            .status
            .success()
    );
    let old = area.0.join("old.jsonl");
    fs::write(&old,concat!(
        "{\"tool\":\"doc_new\",\"args\":{\"name\":\"Migrated\",\"width\":4,\"height\":4,\"doc_id\":\"550e8400-e29b-41d4-a716-446655440000\"}}\n",
        "{\"tool\":\"doc_draw\",\"args\":{\"doc_id\":\"550e8400-e29b-41d4-a716-446655440000\",\"layer\":0,\"frame\":0,\"op\":\"fill_cel\",\"color\":[12,34,56]}}\n"
    )).unwrap();
    assert!(
        !invoke(&["replay", old.to_str().unwrap()], &home)
            .status
            .success()
    );
    assert!(
        fs::read_dir(home.join("documents")).unwrap().all(|e| !e
            .unwrap()
            .path()
            .join("doc.json")
            .is_file())
    );
    let bundle = area.0.join("migrated");
    success(invoke(
        &["migrate", old.to_str().unwrap(), bundle.to_str().unwrap()],
        &home,
    ));
    assert!(old.is_file());
    success(invoke(&["replay", bundle.to_str().unwrap()], &home));
    assert!(
        !invoke(
            &["migrate", old.to_str().unwrap(), bundle.to_str().unwrap()],
            &home
        )
        .status
        .success()
    );
}

#[test]
fn editing_a_legacy_working_document_replaces_its_jsonl() {
    let area = Area::new();
    let home = area.0.join("store");
    let studio = atelier_studio::Studio::with_home(home.clone());
    let made = studio.doc_new("Legacy", 4, 4).unwrap();
    let id = made["doc_id"].as_str().unwrap();
    let old = home.join("documents").join(id).join("recipe.jsonl");
    fs::write(
        &old,
        json!({"tool":"doc_new","args":{"doc_id":id,"name":"Legacy","width":4,"height":4}})
            .to_string(),
    )
    .unwrap();
    success(invoke(
        &[
            "call",
            "doc_draw",
            &json!({"doc_id":id,"layer":0,"frame":0,"op":"fill_cel","color":[21,43,65]})
                .to_string(),
        ],
        &home,
    ));
    assert!(!old.exists());
    let replay = atelier_studio::source::Source::load(&studio.recipe_path(id).unwrap()).unwrap();
    assert_eq!(
        replay.compile().unwrap().cel_full(0, 0).get_pixel(3, 3).0,
        [21, 43, 65, 255]
    );
}

#[test]
fn store_migration_preserves_ids_revisions_and_refuses_mismatched_journals() {
    let area = Area::new();
    let home = area.0.join("store");
    let studio = atelier_studio::Studio::with_home(home.clone());
    let made = studio.doc_new("Legacy", 4, 4).unwrap();
    let id = made["doc_id"].as_str().unwrap();
    let root = home.join("documents").join(id);
    let old = root.join("recipe.jsonl");
    let create =
        json!({"tool":"doc_new","args":{"name":"Legacy","width":4,"height":4,"doc_id":id}});
    // doc_new leaves no materialized cels, so this journal matches exactly.
    fs::write(&old, format!("{create}\n")).unwrap();
    fs::write(root.join("revision"), "7\n").unwrap();
    success(invoke(&["migrate", "--store"], &home));
    assert!(!old.exists());
    assert!(root.join("recipe/recipe.atelier").is_file());
    assert_eq!(fs::read_to_string(root.join("revision")).unwrap(), "7\n");
    assert!(studio.doc_info(id).is_ok());
    // A log whose artwork disagrees with native state must remain untouched.
    fs::remove_dir_all(root.join("recipe")).unwrap();
    fs::write(&old,format!("{create}\n{}\n",json!({"tool":"doc_draw","args":{"doc_id":id,"layer":0,"frame":0,"op":"fill_cel","color":[1,2,3]}}))).unwrap();
    assert!(!invoke(&["migrate", "--store"], &home).status.success());
    assert!(old.is_file());
    assert!(!root.join("recipe").exists());
    assert_eq!(fs::read_to_string(root.join("revision")).unwrap(), "7\n");
}

#[test]
fn store_migration_converts_retained_checkpoint_recipes_before_restore() {
    let area = Area::new();
    let home = area.0.join("store");
    let studio = atelier_studio::Studio::with_home(home.clone());
    let made = studio.doc_new("Legacy", 4, 4).unwrap();
    let id = made["doc_id"].as_str().unwrap();
    let root = home.join("documents").join(id);
    let journal = format!(
        "{}\n",
        json!({"tool":"doc_new","args":{"name":"Legacy","width":4,"height":4,"doc_id":id}})
    );
    fs::write(root.join("recipe.jsonl"), &journal).unwrap();
    let checkpoint = studio
        .checkpoint(
            id,
            atelier_studio::CheckpointAction::Save,
            Some("legacy"),
            None,
        )
        .unwrap();
    let cp = checkpoint["saved"].as_str().unwrap();
    success(invoke(&["migrate", "--store"], &home));
    let snapshot = root.join(".checkpoints").join(cp);
    assert!(!snapshot.join("recipe.jsonl").exists());
    assert!(snapshot.join("recipe/recipe.atelier").is_file());
    studio
        .checkpoint(
            id,
            atelier_studio::CheckpointAction::Restore,
            None,
            Some(cp),
        )
        .unwrap();
    assert!(!root.join("recipe.jsonl").exists());
    assert!(root.join("recipe/recipe.atelier").is_file());
    assert!(studio.verify_store().unwrap().ok);
}
