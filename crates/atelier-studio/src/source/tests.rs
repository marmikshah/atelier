use super::*;
use atelier_core::document::Document;
use atelier_core::source::{PixelData, Step};
use serde_json::json;
use std::path::PathBuf;
struct Area(PathBuf);
impl Area {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("atelier-dsl-test-{}", Uuid::new_v4()));
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
fn gradient() -> Value {
    json!({"op":"gradient","x0":0,"y0":0,"x1":127,"y1":63,"stops":[{"pos":0,"color":[10,20,30]},{"pos":1,"color":[211,171,91]}]})
}
#[test]
fn a_pixel_edit_keeps_the_gradient_and_later_corrections_stay_local() {
    let area = Area::new();
    let s = area.studio();
    let made = s.doc_new("Gradient", 128, 64).unwrap();
    let id = made["doc_id"].as_str().unwrap();
    s.save_recipe(id, None).unwrap();
    apply(&s, id, gradient());
    let path = s.recipe_path(id).unwrap();
    let before = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        before.replace(
            "    gradient",
            "    // Hand adjusted colour stops\n    gradient",
        ),
    )
    .unwrap();
    for c in 1..=12 {
        apply(
            &s,
            id,
            json!({"op":"pencil","points":[[5,5]],"color":[12,34,c]}),
        );
        let source = Source::load(&path).unwrap();
        assert_eq!(source.asset.layers[0].cels[0].steps.len(), 2);
        assert!(source.text.contains("// Hand adjusted colour stops"));
        assert!(source.text.contains("gradient"));
        assert!(source.text.len() < 800);
        edit::equivalent(&s.open(id).unwrap().1, &source.compile().unwrap()).unwrap();
    }
    assert!(!s.doc_dir(id).join("recipe.jsonl").exists());
    let out = area.0.join("export");
    s.export_recipe(id, &out).unwrap();
    let rebuilt = s.build_source(&Source::load(&out).unwrap()).unwrap();
    edit::equivalent(
        &s.open(id).unwrap().1,
        &s.open(rebuilt["doc_id"].as_str().unwrap()).unwrap().1,
    )
    .unwrap();
}
#[test]
fn full_overwrites_replace_only_the_overwritten_construction() {
    let a = Area::new();
    let s = a.studio();
    let m = s.doc_new("Gradient", 128, 64).unwrap();
    let id = m["doc_id"].as_str().unwrap();
    s.save_recipe(id, None).unwrap();
    for value in 10..22 {
        let mut op = gradient();
        op["stops"][0]["color"][0] = json!(value);
        apply(&s, id, op);
        let source = Source::load(&s.recipe_path(id).unwrap()).unwrap();
        assert_eq!(source.asset.layers[0].cels[0].steps.len(), 1);
        assert!(source.text.len() < 500);
    }
}
#[test]
fn shared_frames_detach_and_preserve_offsets_and_hidden_rgb() {
    let a = Area::new();
    let s = a.studio();
    let m = s.doc_new("Pixels", 8, 8).unwrap();
    let id = m["doc_id"].as_str().unwrap();
    let (dir, mut doc) = s.open(id).unwrap();
    doc.add_frame(120, None).unwrap();
    let mut p = image::RgbaImage::new(9, 3);
    p.put_pixel(5, 1, image::Rgba([1, 2, 3, 0]));
    doc.set_cel(0, 0, -2, 1, p.clone()).unwrap();
    doc.set_cel(0, 1, -2, 1, p).unwrap();
    doc.save(&dir).unwrap();
    s.save_recipe(id, None).unwrap();
    let source = Source::load(&s.recipe_path(id).unwrap()).unwrap();
    assert_eq!(source.asset.layers[0].cels.len(), 1);
    assert_eq!(source.asset.layers[0].cels[0].frames, vec![0, 1]);
    let mut p = doc.cel(0, 1).unwrap().2.clone();
    p.put_pixel(4, 0, image::Rgba([7, 8, 9, 255]));
    doc.set_cel(0, 1, -2, 1, p).unwrap();
    doc.save(&dir).unwrap();
    s.save_recipe(id, None).unwrap();
    let source = Source::load(&s.recipe_path(id).unwrap()).unwrap();
    assert_eq!(source.asset.layers[0].cels.len(), 2);
    edit::equivalent(&doc, &source.compile().unwrap()).unwrap();
}
#[test]
fn pixel_forms_are_not_changed_when_a_cel_is_edited() {
    let area = Area::new();
    let s = area.studio();
    let p = area.0.join("art.atelier");
    for body in ["grid \"a\"", "row \"a\" 1", "span 0 0 \"a\""] {
        fs::write(&p,format!("atelier \"Sprite\" format=1 renderer=1\ncanvas 4 4\nlayer \"Body\" {{\n frame 0 {{\n pixels {{\n legend {{ color \"a\" \"#ff0000\"; }}\n {body}\n }}\n }}\n}}\n")).unwrap();
        let made = s.build_source(&Source::load(&p).unwrap()).unwrap();
        let id = made["doc_id"].as_str().unwrap();
        let (dir, mut doc) = s.open(id).unwrap();
        let mut img = doc.cel(0, 0).unwrap().2.clone();
        img.put_pixel(0, 0, image::Rgba([0, 255, 0, 255]));
        doc.set_cel(0, 0, 0, 0, img).unwrap();
        doc.save(&dir).unwrap();
        s.save_recipe(id, None).unwrap();
        let source = Source::load(&s.recipe_path(id).unwrap()).unwrap();
        let Step::Pixels(p) = &source.asset.layers[0].cels[0].steps[0] else {
            panic!("pixels")
        };
        assert!(matches!(
            (&p.data[0], body.split(' ').next().unwrap()),
            (PixelData::Grid(_), "grid")
                | (PixelData::Row { .. }, "row")
                | (PixelData::Span { .. }, "span")
        ));
        edit::equivalent(&doc, &source.compile().unwrap()).unwrap();
    }
}
#[test]
fn full_width_seeds_round_trip_and_settings_do_not_recolour_draws() {
    let a = Area::new();
    let s = a.studio();
    let m = s.doc_new("Seed", 64, 64).unwrap();
    let id = m["doc_id"].as_str().unwrap();
    s.save_recipe(id, None).unwrap();
    let op = json!({"op":"noise","x0":0,"y0":0,"x1":63,"y1":63,"seed":u64::MAX,"stops":[{"pos":0,"color":[1,2,3]},{"pos":1,"color":[211,171,91]}]});
    apply(&s, id, op);
    let old = s.open(id).unwrap().1.cel_full(0, 0);
    let (dir, mut doc) = s.open(id).unwrap();
    doc.set_palette(vec![[1, 2, 3, 255]]).unwrap();
    doc.save(&dir).unwrap();
    s.save_recipe(id, None).unwrap();
    let source = Source::load(&s.recipe_path(id).unwrap()).unwrap();
    assert_eq!(source.compile().unwrap().cel_full(0, 0), old);
    assert!(source.text.contains(&u64::MAX.to_string()));
    assert!(source.text.contains("noise"));
}
#[test]
fn wide_cels_and_reference_resources_survive_checkpoints_and_archives() {
    let a = Area::new();
    let s = a.studio();
    let m = s.doc_new("Archive", 64, 64).unwrap();
    let id = m["doc_id"].as_str().unwrap();
    let (dir, mut doc) = s.open(id).unwrap();
    let img = image::RgbaImage::from_pixel(8192, 2, image::Rgba([9, 7, 5, 0]));
    doc.set_cel(0, 0, -1, 2, img.clone()).unwrap();
    doc.save(&dir).unwrap();
    let reference = a.0.join("ref.png");
    image::RgbaImage::new(8, 8).save(&reference).unwrap();
    s.set_reference(id, Some(reference.to_str().unwrap()))
        .unwrap();
    s.save_recipe(id, None).unwrap();
    let original = Source::load(&s.recipe_path(id).unwrap()).unwrap();
    assert_eq!(original.files.len(), 1);
    assert!(original.text.len() < 600);
    let cp = s
        .checkpoint(id, crate::CheckpointAction::Save, Some("original"), None)
        .unwrap();
    s.set_reference(id, None).unwrap();
    apply(&s, id, json!({"op":"fill_cel","color":[1,2,3]}));
    s.checkpoint(
        id,
        crate::CheckpointAction::Restore,
        None,
        cp["saved"].as_str(),
    )
    .unwrap();
    let restored = Source::load(&s.recipe_path(id).unwrap()).unwrap();
    assert_eq!(original.text, restored.text);
    assert_eq!(original.files, restored.files);
    let archive = a.0.join("art.atelierpack");
    s.pack_document(id, &archive).unwrap();
    let other = Studio::with_home(a.0.join("other"));
    other.unpack_document(&archive, false).unwrap();
    let source = Source::load(&other.recipe_path(id).unwrap()).unwrap();
    edit::equivalent(&s.open(id).unwrap().1, &source.compile().unwrap()).unwrap();
    assert!(other.verify_store().unwrap().ok);
}
#[test]
fn strict_schema_rejects_malformed_and_ambiguous_input() {
    let good = "atelier \"Art\" format=1 renderer=1\ncanvas 4 4\nlayer \"Body\" {\n frame 0 {\n fill_cel color=\"#ff8800\"\n }\n}\n";
    for bad in [
        good.replace("format=1", "format=2"),
        good.replace("canvas 4 4", "canvas 4 4\ncanvas 8 8"),
        good.replace("color=\"#ff8800\"", "color=\"#ff8800\" color=\"#ffffff\""),
        good.replace("fill_cel", "misspelled"),
        good.replace("frame 0", "frame 0 0"),
        good.replace("frame 0", "frame 2"),
        good.replace("color=\"#ff8800\"", "color=\"wrong\""),
        good.trim_end_matches(['\n', '}']).to_owned(),
    ] {
        assert!(parse::decode(&bad).is_err(), "{bad}");
    }
    assert!(parse::decode(good).is_ok());
    let error = syntax::parse("atelier \"unclosed").unwrap_err();
    assert!(error.contains("line 1, column"));
    let deep = format!("{}{}", "node {\n".repeat(34), "}\n".repeat(34));
    assert!(syntax::parse(&deep).is_err());
}
#[test]
fn names_comments_and_multiline_grids_round_trip() {
    let mut doc = Document::new("Art \"✦\"\\\n\u{1}", 4, 4);
    doc.apply_op(
        0,
        0,
        &json!({"op":"pencil","color":[1,2,3],"points":[[0,0],[3,3]]}),
    )
    .unwrap();
    let a = Area::new();
    let source = edit::current(&doc, &a.0, None, None).unwrap();
    edit::equivalent(&doc, &source.compile().unwrap()).unwrap();
    assert!(source.text.lines().all(|line| line == line.trim_end()));
    let text = "/* outer /* inner */ */\nnode \"a\\u{1f3ae}\" enabled=#true; // comment\n";
    assert_eq!(syntax::parse(text).unwrap()[0].args[0], json!("a🎮"));
    assert!(syntax::parse("node \"bad\\u{d800}\"").is_err());
}

#[test]
fn source_comments_and_pixel_corrections_do_not_accumulate_unused_colours() {
    let area = Area::new();
    let s = area.studio();
    let file = area.0.join("art.atelier");
    fs::write(&file,"// Canvas intent\natelier \"Art\" format=1 renderer=1\ncanvas 4 4\nlayer \"Body\" {\n frame 0 {\n pixels {\n legend {color \"a\" \"#ff0000\";}\n grid \"a\"\n // Keep this pixel note\n }\n // Keep this frame note\n }\n // Keep this layer note\n}\n// Final author note\n").unwrap();
    let made = s.build_source(&Source::load(&file).unwrap()).unwrap();
    let id = made["doc_id"].as_str().unwrap();
    for value in 1..=50 {
        apply(
            &s,
            id,
            json!({"op":"pencil","points":[[0,0]],"color":[12,34,value]}),
        );
        let source = Source::load(&s.recipe_path(id).unwrap()).unwrap();
        for note in [
            "Canvas intent",
            "Keep this pixel note",
            "Keep this frame note",
            "Keep this layer note",
            "Final author note",
        ] {
            assert!(source.text.contains(note), "lost {note}");
        }
        let Step::Pixels(p) = &source.asset.layers[0].cels[0].steps[0] else {
            panic!("pixels")
        };
        assert_eq!(p.legend.len(), 1);
        assert!(source.text.len() < 500);
    }
}
#[test]
fn repeated_grid_patches_keep_the_gradient_and_erase_explicitly() {
    let a = Area::new();
    let s = a.studio();
    let made = s.doc_new("Gradient", 128, 64).unwrap();
    let id = made["doc_id"].as_str().unwrap();
    s.save_recipe(id, None).unwrap();
    apply(&s, id, gradient());
    for value in 1..=30 {
        let (dir, mut doc) = s.open(id).unwrap();
        let mut img = doc.cel_full(0, 0);
        img.put_pixel(5, 5, image::Rgba([12, 34, value, 255]));
        img.put_pixel(7, 5, image::Rgba([0; 4]));
        doc.set_cel(0, 0, 0, 0, img).unwrap();
        doc.save(&dir).unwrap();
        s.save_recipe(id, None).unwrap();
        let source = Source::load(&s.recipe_path(id).unwrap()).unwrap();
        assert_eq!(source.asset.layers[0].cels[0].steps.len(), 2);
        assert!(source.text.contains("gradient"));
        assert!(source.text.contains("#00000000"));
        assert!(source.text.len() < 800);
        edit::equivalent(&doc, &source.compile().unwrap()).unwrap();
    }
}

#[test]
fn metadata_edits_preserve_colour_spelling_and_windows_line_endings() {
    let area = Area::new();
    let s = area.studio();
    let p = area.0.join("art.atelier");
    let authored="atelier \"Art\" format=1 renderer=1\ncanvas 4 4\npalette \"#FF8800\"\nlayer \"Body\" {\n frame 0 {\n // Preserve this command\n fill_cel color=\"#FF8800\"\n }\n}\n".replace('\n',"\r\n");
    fs::write(&p, &authored).unwrap();
    let made = s.build_source(&Source::load(&p).unwrap()).unwrap();
    let id = made["doc_id"].as_str().unwrap();
    let (dir, mut doc) = s.open(id).unwrap();
    doc.set_frame_duration(0, 120).unwrap();
    doc.save(&dir).unwrap();
    s.save_recipe(id, None).unwrap();
    let source = Source::load(&s.recipe_path(id).unwrap()).unwrap();
    assert!(source.text.contains("palette \"#FF8800\"\r\n"));
    assert!(source.text.contains("fill_cel color=\"#FF8800\"\r\n"));
    assert!(source.text.contains("// Preserve this command"));
    edit::equivalent(&doc, &source.compile().unwrap()).unwrap();
    let multiline="atelier \"Art\" format=1 renderer=1\ncanvas 4 4\nlayer \"Body\" {\n frame 0 {\n pixels {\n legend {color \"x\" \"#ffffff\";}\n grid \"\"\"\n   x\n   xx\n   \"\"\"\n }\n }\n}\n".replace('\n',"\r\n");
    assert!(parse::decode(&multiline).is_ok());
}
