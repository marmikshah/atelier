use super::*;
use atelier_core::document::Document;
use image::{Rgba, RgbaImage};
use std::path::PathBuf;

const FIXTURE: &str = r##"# preserve this author's comment
format = 1
renderer = 1
name = "Fixture"
canvas = [4, 4]
frames = [90, 130]
layers = [{ id = "body", name = "Body" }]
cels = [{ layer = "body", frames = [0, 1], part = "body" }]
[inks]
brass = "#ffcc00" # a named role
[parts.body]
size = [4, 4]
grid = '''
x...
.x..
....
....
'''
legend = { x = "brass" }
"##;

struct Area(PathBuf);
impl Area {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("atelier-source-test-{}", Uuid::new_v4()));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn source(&self) -> PathBuf {
        let p = self.0.join("source.toml");
        fs::write(&p, FIXTURE).unwrap();
        p
    }
}
impl Drop for Area {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn guarded_edits_preserve_comments_and_failed_edits_publish_nothing() {
    let area = Area::new();
    let path = area.source();
    let source = Source::load(&path).unwrap();
    let edits: Edits = serde_json::from_value(json!({"inks":{"brass":"#ee3300"}})).unwrap();
    Source::apply(&path, &source.revision, edits).unwrap();
    let changed = fs::read_to_string(&path).unwrap();
    assert!(changed.contains("# preserve this author's comment"));
    assert!(changed.contains("# a named role"));
    let edits: Edits = serde_json::from_value(json!({"inks":{"brass":"#ffffff"}})).unwrap();
    assert!(
        Source::apply(&path, &source.revision, edits)
            .unwrap_err()
            .contains("conflict")
    );
    let next = Source::load(&path).unwrap();
    let edits: Edits =
        serde_json::from_value(json!({"parts":{"body":{"size":[4,4],"items":[{"part":"body"}]}}}))
            .unwrap();
    assert!(Source::apply(&path, &next.revision, edits).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);
}

#[test]
fn migration_preserves_sparse_offsets_hidden_rgb_and_all_metadata() {
    let area = Area::new();
    let studio = Studio::with_home(area.0.join("store"));
    let result = studio
        .build_source(&Source::load(&area.source()).unwrap())
        .unwrap();
    let id = result["doc_id"].as_str().unwrap();
    let mut doc = Document::load(&studio.doc_dir(id)).unwrap();
    let mut image = RgbaImage::new(3, 2);
    image.put_pixel(1, 0, Rgba([17, 33, 88, 0]));
    image.put_pixel(2, 1, Rgba([12, 20, 40, 97]));
    doc.set_cel(0, 0, -1, 3, image).unwrap();
    doc.clear_cel(0, 1).unwrap();
    doc.save(&studio.doc_dir(id)).unwrap();
    let bundle = area.0.join("bundle");
    let report = studio
        .migrate_source(id, &bundle, MigrationMode::Auto)
        .unwrap();
    assert_eq!(report["pixels_equal"], true);
    let source = Source::load(&bundle).unwrap();
    migrate::equivalent(&doc, &source.compile().unwrap()).unwrap();
    assert!(
        studio
            .migrate_source(id, &bundle, MigrationMode::Auto)
            .is_err()
    );
    let text = fs::read_to_string(bundle.join("source.toml")).unwrap();
    assert!(!text.contains(id));
}

#[test]
fn source_baselines_survive_archive_checkpoint_and_integrity_checks() {
    let area = Area::new();
    let studio = Studio::with_home(area.0.join("store"));
    let result = studio
        .build_source(&Source::load(&area.source()).unwrap())
        .unwrap();
    let id = result["doc_id"].as_str().unwrap();
    let cp = studio
        .checkpoint(id, crate::CheckpointAction::Save, None, None)
        .unwrap();
    let cp_id = cp["saved"].as_str().unwrap();
    studio
        .checkpoint(id, crate::CheckpointAction::Restore, None, Some(cp_id))
        .unwrap();
    let pack = area.0.join("fixture.atelierpack");
    studio.pack_document(id, &pack).unwrap();
    let other = Studio::with_home(area.0.join("other"));
    other.unpack_document(&pack, false).unwrap();
    assert_eq!(
        Source::load(&other.doc_dir(id).join("source"))
            .unwrap()
            .revision,
        Source::load(&area.source()).unwrap().revision
    );
    let report = serde_json::to_value(other.verify_store().unwrap()).unwrap();
    assert_eq!(report["issues"], json!([]));
}

#[test]
fn resource_changes_conflict_and_paths_cannot_escape() {
    let area = Area::new();
    let path = area.source();
    let mut source = Source::load(&path).unwrap().asset;
    source.parts.get_mut("body").unwrap().grid = None;
    source.parts.get_mut("body").unwrap().legend.clear();
    source.parts.get_mut("body").unwrap().image = Some("body.png".into());
    RgbaImage::from_pixel(4, 4, Rgba([1, 2, 3, 255]))
        .save(area.0.join("body.png"))
        .unwrap();
    fs::write(&path, format::encode(&source).unwrap()).unwrap();
    let before = Source::load(&path).unwrap().revision;
    RgbaImage::from_pixel(4, 4, Rgba([4, 5, 6, 255]))
        .save(area.0.join("body.png"))
        .unwrap();
    assert_ne!(Source::load(&path).unwrap().revision, before);
    source.parts.get_mut("body").unwrap().image = Some("../escape.png".into());
    fs::write(&path, format::encode(&source).unwrap()).unwrap();
    assert!(
        Source::load(&path)
            .err()
            .unwrap()
            .contains("inside the bundle")
    );
    source.parts.get_mut("body").unwrap().image = Some("linked.png".into());
    std::os::unix::fs::symlink(area.0.join("body.png"), area.0.join("linked.png")).unwrap();
    fs::write(&path, format::encode(&source).unwrap()).unwrap();
    assert!(Source::load(&path).err().unwrap().contains("symlinks"));
}

#[test]
fn png_migration_is_lossless_with_palette_alpha_and_hidden_rgb() {
    let area = Area::new();
    let studio = Studio::with_home(area.0.join("store"));
    let result = studio
        .build_source(&Source::load(&area.source()).unwrap())
        .unwrap();
    let id = result["doc_id"].as_str().unwrap();
    let mut document = Document::load(&studio.doc_dir(id)).unwrap();
    // Existing scale operations can create cels wider than the 4096px canvas
    // ceiling. Preserve their off-canvas pixels and logical bounds too.
    let mut image = RgbaImage::new(8192, 2);
    for (x, y, p) in image.enumerate_pixels_mut() {
        *p = Rgba([
            17,
            (x % 4) as u8,
            (y % 3) as u8,
            [0, 91, 255][(x + y) as usize % 3],
        ]);
    }
    document.set_cel(0, 0, -3, 1, image).unwrap();
    document.save(&studio.doc_dir(id)).unwrap();
    let destination = area.0.join("pixels");
    studio
        .migrate_source(id, &destination, MigrationMode::Pixels)
        .unwrap();
    let loaded = Source::load(&destination).unwrap();
    assert!(!loaded.files.is_empty());
    migrate::equivalent(&document, &loaded.compile().unwrap()).unwrap();
}

#[test]
fn auto_migration_retains_a_gradient_and_bakes_verbose_detail_in_one_asset() {
    let area = Area::new();
    let mut document = Document::new("Mixed", 128, 64);
    document.add_layer(Some("Detail".into()), 255, Default::default());
    let mut entries = vec![
        crate::JournalEntry::new(ToolName::DocNew, serde_json::Map::new()),
        crate::JournalEntry::new(
            ToolName::DocLayer,
            json!({"op":"add","name":"Detail"})
                .as_object()
                .unwrap()
                .clone(),
        ),
    ];
    let gradient = json!({"op":"gradient","x0":0,"y0":0,"x1":127,"y1":63,"stops":[{"pos":0,"color":[1,2,3]},{"pos":1,"color":[222,174,51]}]});
    document.apply_op(0, 0, &gradient).unwrap();
    let mut args = gradient.as_object().unwrap().clone();
    args.insert("layer".into(), json!(0));
    args.insert("frame".into(), json!(0));
    entries.push(crate::JournalEntry::new(ToolName::DocDraw, args));
    for y in (0..64).step_by(4) {
        for x in (0..128).step_by(4) {
            let op =
                json!({"op":"rect","x0":x,"y0":y,"x1":x+1,"y1":y+1,"color":[20,30,40],"fill":true});
            document.apply_op(1, 0, &op).unwrap();
            let mut args = op.as_object().unwrap().clone();
            args.insert("layer".into(), json!(1));
            args.insert("frame".into(), json!(0));
            entries.push(crate::JournalEntry::new(ToolName::DocDraw, args));
        }
    }
    let document_path = area.0.join("document");
    document.save(&document_path).unwrap();
    let destination = area.0.join("mixed");
    let report = migrate::publish(
        &document,
        &document_path,
        &entries,
        &destination,
        MigrationMode::Auto,
    )
    .unwrap();
    assert_eq!(report["representation"], "mixed");
    let source = Source::load(&destination).unwrap();
    assert!(source.asset.parts.values().any(|p| p.draw.is_some()));
    assert!(source.asset.parts.values().any(|p| p.image.is_some()));
    migrate::equivalent(&document, &source.compile().unwrap()).unwrap();
}

#[test]
fn full_u64_seeds_survive_source_formatting_and_comment_preserving_edits() {
    let area = Area::new();
    let path = area.source();
    let mut asset = Source::load(&path).unwrap().asset;
    let op = json!({"op":"noise","x0":0,"y0":0,"x1":3,"y1":3,"seed":u64::MAX,
        "stops":[{"pos":0,"color":[1,2,3]},{"pos":1,"color":[211,171,91]}]});
    let mut expected = Document::new("seed", 4, 4);
    expected.apply_op(0, 0, &op).unwrap();
    asset.parts.insert(
        "body".into(),
        Part {
            size: [4, 4],
            draw: Some(vec![op]),
            ..Part::default()
        },
    );
    fs::write(&path, format::encode(&asset).unwrap()).unwrap();
    let source = Source::load(&path).unwrap();
    assert_eq!(
        source.asset.parts["body"].draw.as_ref().unwrap()[0]["seed"],
        u64::MAX.to_string()
    );
    Source::apply(
        &path,
        &source.revision,
        serde_json::from_value(json!({"inks":{"brass":"#aa3300"}})).unwrap(),
    )
    .unwrap();
    assert_eq!(
        Source::load(&path)
            .unwrap()
            .compile()
            .unwrap()
            .cel_full(0, 0),
        expected.cel_full(0, 0)
    );
}
