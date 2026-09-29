use super::*;
use atelier_core::document::Document;
use serde_json::json;
use std::path::PathBuf;

struct Area(PathBuf);
impl Area {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("atelier-flat-test-{}", Uuid::new_v4()));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn studio(&self) -> Studio {
        Studio::with_home(self.0.join("store"))
    }
}
impl Drop for Area {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn apply(studio: &Studio, id: &str, op: Value) {
    let (dir, mut doc) = studio.open(id).unwrap();
    doc.apply_op(0, 0, &op).unwrap();
    doc.save(&dir).unwrap();
    let mut args = op;
    args["layer"] = json!(0);
    args["frame"] = json!(0);
    studio
        .save_recipe(id, Some((ToolName::DocDraw, &args)))
        .unwrap();
}
#[test]
fn overwrites_do_not_accumulate_and_gradients_remain_procedural() {
    let area = Area::new();
    let studio = area.studio();
    let created = studio.doc_new("Gradient", 128, 64).unwrap();
    let id = created["doc_id"].as_str().unwrap();
    studio.save_recipe(id, None).unwrap();
    for value in 10..22 {
        apply(
            &studio,
            id,
            json!({"op":"gradient","x0":0,"y0":0,"x1":127,"y1":63,"stops":[{"pos":0,"color":[value,2,3]},{"pos":1,"color":[222,174,51]}]}),
        );
        let source = Source::load(&studio.recipe_path(id).unwrap()).unwrap();
        assert!(source.encoded_bytes() < 700);
        assert_eq!(source.asset.layers[0].cels[0].draw.len(), 1);
        optimize::equivalent(&studio.open(id).unwrap().1, &source.compile().unwrap()).unwrap();
    }
    assert!(!studio.doc_dir(id).join("recipe.jsonl").exists());
    let bundle = area.0.join("export");
    studio.export_recipe(id, &bundle).unwrap();
    let imported = Source::load(&bundle).unwrap();
    let rebuilt = studio.build_source(&imported).unwrap();
    optimize::equivalent(
        &studio.open(id).unwrap().1,
        &studio.open(rebuilt["doc_id"].as_str().unwrap()).unwrap().1,
    )
    .unwrap();
}
#[test]
fn shared_frame_storage_detaches_on_edit_and_preserves_bounds_and_hidden_rgb() {
    let area = Area::new();
    let studio = area.studio();
    let made = studio.doc_new("Pixels", 8, 8).unwrap();
    let id = made["doc_id"].as_str().unwrap();
    let (dir, mut doc) = studio.open(id).unwrap();
    doc.add_frame(120, None).unwrap();
    let mut pixels = image::RgbaImage::new(9, 3);
    pixels.put_pixel(5, 1, image::Rgba([1, 2, 3, 0]));
    doc.set_cel(0, 0, -2, 1, pixels.clone()).unwrap();
    doc.set_cel(0, 1, -2, 1, pixels).unwrap();
    doc.save(&dir).unwrap();
    studio.save_recipe(id, None).unwrap();
    let a = Source::load(&studio.recipe_path(id).unwrap()).unwrap();
    assert_eq!(a.asset.layers[0].cels.len(), 1);
    assert_eq!(a.asset.layers[0].cels[0].frames, vec![0, 1]);
    optimize::equivalent(&doc, &a.compile().unwrap()).unwrap();
    let mut pixels = doc.cel(0, 1).unwrap().2.clone();
    pixels.put_pixel(4, 0, image::Rgba([7, 8, 9, 255]));
    doc.set_cel(0, 1, -2, 1, pixels).unwrap();
    doc.save(&dir).unwrap();
    studio.save_recipe(id, None).unwrap();
    let b = Source::load(&studio.recipe_path(id).unwrap()).unwrap();
    assert_eq!(b.asset.layers[0].cels.len(), 2);
    optimize::equivalent(&doc, &b.compile().unwrap()).unwrap();
}
#[test]
fn snapshots_cover_resources_and_refuse_traversal_and_links() {
    let area = Area::new();
    let studio = area.studio();
    let made = studio.doc_new("Wide", 8, 8).unwrap();
    let id = made["doc_id"].as_str().unwrap();
    let (dir, mut doc) = studio.open(id).unwrap();
    let pixels = image::RgbaImage::from_pixel(8192, 2, image::Rgba([9, 7, 5, 0]));
    doc.set_cel(0, 0, 0, 0, pixels).unwrap();
    doc.save(&dir).unwrap();
    studio.save_recipe(id, None).unwrap();
    let path = studio.recipe_path(id).unwrap();
    let source = Source::load(&path).unwrap();
    optimize::equivalent(&doc, &source.compile().unwrap()).unwrap();
    let image = source.asset.cels().next().unwrap().image.as_ref().unwrap();
    let resource = path.parent().unwrap().join(image);
    fs::write(&resource, b"broken").unwrap();
    assert!(Source::load(&path).is_err());
    optimize::equivalent(&doc, &source.compile().unwrap()).unwrap();
    let malicious = source.text.replace(image, "../outside.png");
    fs::write(&path, malicious).unwrap();
    assert!(Source::load(&path).is_err());
    fs::write(&path, &source.text).unwrap();
    fs::remove_file(&resource).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&path, &resource).unwrap();
        assert!(Source::load(&path).is_err());
    }
}
#[test]
fn full_width_seeds_round_trip_through_toml() {
    let area = Area::new();
    let mut d = Document::new("Seed", 64, 64);
    let op = json!({"op":"noise","x0":0,"y0":0,"x1":63,"y1":63,"seed":u64::MAX,"stops":[{"pos":0,"color":[1,2,3]},{"pos":1,"color":[211,171,91]}]});
    d.apply_op(0, 0, &op).unwrap();
    let mut args = op;
    args["layer"] = json!(0);
    args["frame"] = json!(0);
    let source = optimize::current(&d, &area.0, None, Some((ToolName::DocDraw, &args))).unwrap();
    let bundle = area.0.join("seed");
    source.write_to(&bundle).unwrap();
    let loaded = Source::load(&bundle).unwrap();
    optimize::equivalent(&d, &loaded.compile().unwrap()).unwrap();
    assert_eq!(
        loaded.asset.layers[0].cels[0].draw[0]["seed"],
        u64::MAX.to_string()
    );
}

#[test]
fn archives_and_checkpoints_preserve_png_resources_and_reference() {
    let area = Area::new();
    let studio = area.studio();
    let made = studio.doc_new("Archive", 64, 64).unwrap();
    let id = made["doc_id"].as_str().unwrap();
    let (dir, mut doc) = studio.open(id).unwrap();
    let pixels = image::RgbaImage::from_fn(64, 64, |x, y| {
        image::Rgba([x as u8, y as u8, (x ^ y) as u8, 255])
    });
    doc.set_cel(0, 0, -1, 2, pixels.clone()).unwrap();
    doc.save(&dir).unwrap();
    let reference = area.0.join("ref.png");
    pixels.save(&reference).unwrap();
    studio
        .set_reference(id, Some(reference.to_str().unwrap()))
        .unwrap();
    studio.save_recipe(id, None).unwrap();
    let saved = studio
        .checkpoint(id, crate::CheckpointAction::Save, Some("original"), None)
        .unwrap();
    let cp = saved["saved"].as_str().unwrap();
    let original = Source::load(&studio.recipe_path(id).unwrap()).unwrap();
    assert_eq!(original.files.len(), 2);
    studio.set_reference(id, None).unwrap();
    apply(&studio, id, json!({"op":"clear_cel"}));
    studio
        .checkpoint(id, crate::CheckpointAction::Restore, None, Some(cp))
        .unwrap();
    let restored = Source::load(&studio.recipe_path(id).unwrap()).unwrap();
    assert_eq!(original.text, restored.text);
    assert_eq!(original.files, restored.files);
    let archive = area.0.join("art.atelierpack");
    studio.pack_document(id, &archive).unwrap();
    let other = Studio::with_home(area.0.join("other"));
    other.unpack_document(&archive, false).unwrap();
    let loaded = Source::load(&other.recipe_path(id).unwrap()).unwrap();
    assert_eq!(loaded.files, original.files);
    optimize::equivalent(&studio.open(id).unwrap().1, &loaded.compile().unwrap()).unwrap();
    assert!(other.verify_store().unwrap().ok);
}

#[test]
fn a_later_palette_change_does_not_recolour_saved_operations() {
    let area = Area::new();
    let studio = area.studio();
    let path = area.0.join("recipe.toml");
    fs::write(&path,r##"format=1
renderer=1
name="Palette"
canvas=[64,64]
palette=["#c82814"]
[[layers]]
name="Gradient"
[[layers.cels]]
draw=[{op="gradient",x0=0,y0=0,x1=63,y1=63,stops=[{pos=0,color=[10,20,30]},{pos=1,color=[211,171,91]}]}]
"##).unwrap();
    let made = studio.build_source(&Source::load(&path).unwrap()).unwrap();
    let id = made["doc_id"].as_str().unwrap();
    let (dir, mut doc) = studio.open(id).unwrap();
    doc.set_palette(vec![[20, 40, 200, 255]]).unwrap();
    doc.save(&dir).unwrap();
    studio.save_recipe(id, None).unwrap();
    let source = Source::load(&studio.recipe_path(id).unwrap()).unwrap();
    assert_eq!(source.asset.layers[0].cels[0].draw[0]["op"], "gradient");
    assert_eq!(
        source.compile().unwrap().cel_full(0, 0).get_pixel(0, 0).0,
        [200, 40, 20, 255]
    );
    optimize::equivalent(&doc, &source.compile().unwrap()).unwrap();
}
