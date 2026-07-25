use super::batch::{OPS, OpSide, batch_op_keys};
use super::*;
use image::Rgba;

#[test]
fn the_op_table_is_the_single_source_of_truth() {
    // One table (`OPS`) drives dispatch, validation and the doc_draw/doc_fx
    // split — the three lists it replaced were hand-synced and drifted.
    let mut seen = std::collections::HashSet::new();
    for s in OPS {
        assert!(seen.insert(s.name), "duplicate op name {}", s.name);
        // Validation reads the same key lists the table declares.
        assert_eq!(
            batch_op_keys(s.name),
            Some((s.required, s.optional)),
            "{}: validator disagrees with the table",
            s.name
        );
        // Dispatch finds every entry. A missing-required-arg op may Err or
        // panic; either proves the executor exists. The only forbidden outcome
        // is "unknown batch op".
        let mut d = Document::new("t", 8, 8);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            d.apply_op_raw(0, 0, &json!({"op": s.name}))
        }));
        if let Ok(Err(e)) = r {
            assert!(!e.contains("unknown batch op"), "{}: {e}", s.name);
        }
    }
    // The vocabularies are filtered views: disjoint, and covering every op.
    for op in draw_ops() {
        assert!(OPS.iter().any(|s| s.name == *op && s.side == OpSide::Draw));
        assert!(!fx_ops().contains(op), "{op} is in both vocabularies");
    }
    for op in fx_ops() {
        assert!(OPS.iter().any(|s| s.name == *op && s.side == OpSide::Fx));
    }
    // glow stays batch-only (its on-palette snap has no single-op form).
    assert!(!draw_ops().contains(&"glow") && !fx_ops().contains(&"glow"));
    // And the reverse: an unknown op IS reported (the guard isn't vacuous).
    assert!(batch_op_keys("nope").is_none());
    let mut d = Document::new("t", 8, 8);
    let e = d.apply_op_raw(0, 0, &json!({"op": "nope"})).unwrap_err();
    assert!(e.contains("unknown batch op"));
}

#[test]
fn palette_set_and_index() {
    let mut d = Document::new("t", 4, 4);
    d.set_palette(vec![[1, 1, 1, 255], [2, 2, 2, 255]]);
    assert_eq!(d.meta.palette.len(), 2);
    assert_eq!(d.meta.palette[1], [2, 2, 2, 255]);
}

#[test]
fn frame_ops_delete_reindexes_and_protects_last() {
    let mut d = Document::new("t", 2, 2);
    d.pencil(0, 0, &[(0, 0)], [1, 1, 1, 255], 1).unwrap();
    d.add_frame(100, None);
    d.pencil(0, 1, &[(0, 0)], [2, 2, 2, 255], 1).unwrap();
    d.add_frame(100, None);
    d.pencil(0, 2, &[(0, 0)], [3, 3, 3, 255], 1).unwrap();
    d.add_tag("mid", 1, 1, "forward").unwrap();
    d.add_tag("all", 0, 2, "forward").unwrap();
    d.frame_ops("delete", 1, None, None).unwrap();
    assert_eq!(d.meta.frames.len(), 2);
    // Frame 2's cel slid down to index 1.
    assert_eq!(d.get_pixel(0, 1, 0, 0).unwrap(), [3, 3, 3, 255]);
    // The tag covering only the deleted frame is gone; the spanning one shrank.
    assert_eq!(d.meta.tags.len(), 1);
    assert_eq!((d.meta.tags[0].from, d.meta.tags[0].to), (0, 1));
    d.frame_ops("delete", 1, None, None).unwrap();
    assert!(d.frame_ops("delete", 0, None, None).is_err()); // last frame protected
}

#[test]
fn frame_ops_move_remaps_tags_without_ballooning() {
    // A tagged frame moved far away LEAVES its tag (no ballooning over
    // untagged frames).
    let mut d = Document::new("t", 2, 2);
    for _ in 0..3 {
        d.add_frame(100, None);
    }
    d.add_tag("walk", 0, 1, "forward").unwrap();
    d.frame_ops("move", 1, Some(3), None).unwrap();
    assert_eq!((d.meta.tags[0].from, d.meta.tags[0].to), (0, 0));

    // A reorder entirely INSIDE a tag keeps the tag's full coverage.
    let mut e = Document::new("t", 2, 2);
    e.add_frame(100, None);
    e.add_frame(100, None);
    e.add_tag("all", 0, 2, "forward").unwrap();
    e.frame_ops("move", 1, Some(2), None).unwrap();
    assert_eq!((e.meta.tags[0].from, e.meta.tags[0].to), (0, 2));

    // A single-frame tag follows its frame.
    let mut s = Document::new("t", 2, 2);
    for _ in 0..3 {
        s.add_frame(100, None);
    }
    s.add_tag("pose", 1, 1, "forward").unwrap();
    s.frame_ops("move", 1, Some(3), None).unwrap();
    assert_eq!((s.meta.tags[0].from, s.meta.tags[0].to), (3, 3));
}

#[test]
fn frame_ops_move_and_duplicate() {
    let mut d = Document::new("t", 2, 2);
    d.pencil(0, 0, &[(0, 0)], [1, 1, 1, 255], 1).unwrap();
    d.add_frame(100, None);
    d.pencil(0, 1, &[(0, 0)], [2, 2, 2, 255], 1).unwrap();
    d.frame_ops("move", 0, Some(1), None).unwrap();
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [2, 2, 2, 255]);
    assert_eq!(d.get_pixel(0, 1, 0, 0).unwrap(), [1, 1, 1, 255]);
    d.frame_ops("duplicate", 0, None, None).unwrap();
    assert_eq!(d.meta.frames.len(), 3);
    assert_eq!(d.get_pixel(0, 1, 0, 0).unwrap(), [2, 2, 2, 255]); // the copy
    assert_eq!(d.get_pixel(0, 2, 0, 0).unwrap(), [1, 1, 1, 255]); // shifted
}

#[test]
fn move_layer_reorders_and_cels_follow() {
    let mut d = Document::new("t", 4, 4);
    d.add_layer(None, 255, "normal".into()).unwrap(); // layer 1
    d.pencil(0, 0, &[(0, 0)], [255, 0, 0, 255], 1).unwrap();
    d.pencil(1, 0, &[(1, 1)], [0, 0, 255, 255], 1).unwrap();
    d.move_layer(0, 1).unwrap();
    // old layer 1 (blue) is now index 0; old layer 0 (red) is index 1
    assert_eq!(d.get_pixel(0, 0, 1, 1).unwrap(), [0, 0, 255, 255]);
    assert_eq!(d.get_pixel(1, 0, 0, 0).unwrap(), [255, 0, 0, 255]);
}

#[test]
fn insert_and_delete_layer_shift_cels() {
    let mut d = Document::new("t", 4, 4);
    d.pencil(0, 0, &[(0, 0)], [9, 9, 9, 255], 1).unwrap();
    d.insert_layer(0, Some("bg".into()), 255, "normal".into())
        .unwrap();
    // the drawn cel moved from layer 0 to layer 1
    assert_eq!(d.meta.layers.len(), 2);
    assert_eq!(d.get_pixel(1, 0, 0, 0).unwrap(), [9, 9, 9, 255]);
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 0]);
    d.delete_layer(0).unwrap();
    assert_eq!(d.meta.layers.len(), 1);
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [9, 9, 9, 255]);
}

#[test]
fn delete_last_layer_is_refused() {
    let mut d = Document::new("t", 4, 4);
    assert!(d.delete_layer(0).is_err());
}

#[test]
fn duplicate_layer_copies_cels_above() {
    let mut d = Document::new("t", 4, 4);
    d.pencil(0, 0, &[(2, 2)], [7, 7, 7, 255], 1).unwrap();
    let ni = d.duplicate_layer(0).unwrap();
    assert_eq!(ni, 1);
    assert_eq!(d.get_pixel(0, 0, 2, 2).unwrap(), [7, 7, 7, 255]);
    assert_eq!(d.get_pixel(1, 0, 2, 2).unwrap(), [7, 7, 7, 255]);
}

#[test]
fn merge_down_bakes_upper_onto_lower() {
    let mut d = Document::new("t", 4, 4);
    d.add_layer(None, 255, "normal".into()).unwrap();
    d.pencil(0, 0, &[(0, 0)], [255, 0, 0, 255], 1).unwrap();
    d.pencil(1, 0, &[(0, 0)], [0, 0, 255, 255], 1).unwrap();
    d.merge_down(1).unwrap();
    assert_eq!(d.meta.layers.len(), 1);
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 255, 255]);
}

#[test]
fn cel_wide_ops_are_reachable_from_their_single_op_tool() {
    // The vocabulary drives doc_draw/doc_fx dispatch, so an op the registry can
    // execute but the vocabulary doesn't name is reachable ONLY through
    // doc_batch — `clear_cel` was exactly that bug once.
    for op in ["fill_cel", "clear_cel"] {
        assert!(batch_op_keys(op).is_some(), "{op} lost its registry entry");
        assert!(
            draw_ops().contains(&op),
            "{op} is dispatchable but missing from the draw vocabulary — doc_draw rejects it"
        );
    }
}

#[test]
fn clear_cel_rejects_a_target_that_does_not_exist() {
    let mut d = Document::new("t", 4, 4);
    assert!(
        d.clear_cel(99, 0).is_err(),
        "clearing a nonexistent layer reported success"
    );
    assert!(
        d.clear_cel(0, 99).is_err(),
        "clearing a nonexistent frame reported success"
    );
    assert!(d.clear_cel(0, 0).is_ok(), "clearing a real cel must work");
}

#[test]
fn snap_to_palette_picks_perceptual_nearest() {
    let mut d = Document::new("t", 4, 4);
    d.pencil(0, 0, &[(0, 0)], [200, 10, 10, 255], 1).unwrap();
    let changed = d.snap_to_palette(
        &[[255, 0, 0, 255], [0, 0, 255, 255]],
        None,
        None,
        AlphaSnap::Preserve,
    );
    assert_eq!(changed, 1);
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [255, 0, 0, 255]);
}

#[test]
fn replace_color_recolours_aa_edges() {
    // A solid pixel and a same-RGB anti-aliased (low-alpha) edge: both should
    // recolour at tol 0 now that the match ignores alpha (RGB max-channel).
    let mut d = Document::new("t", 3, 1);
    d.pencil(0, 0, &[(0, 0)], [200, 0, 0, 255], 1).unwrap();
    d.pencil(0, 0, &[(1, 0)], [200, 0, 0, 80], 1).unwrap();
    d.replace_color(0, 0, [200, 0, 0, 255], [0, 0, 255, 255], 0)
        .unwrap();
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 255, 255]);
    assert_eq!(d.get_pixel(0, 0, 1, 0).unwrap(), [0, 0, 255, 255]); // AA edge too
}

/// Transparent is [0,0,0,0], so an RGB-only match made a black outline on an
/// empty canvas indistinguishable from the background: a fill OUTSIDE the shape
/// ate the outline, and a fill inside escaped to the whole canvas. The most
/// ordinary pixel-art setup there is.
#[test]
fn bucket_fill_does_not_cross_the_transparent_boundary() {
    let mut d = Document::new("t", 12, 12);
    d.rect(0, 0, 3, 3, 8, 8, [0, 0, 0, 255], false, 1).unwrap();
    // Click outside the ring, tol 0.
    d.bucket_fill(0, 0, 0, 0, [255, 0, 0, 255], 0).unwrap();
    assert_eq!(
        d.get_pixel(0, 0, 3, 3).unwrap(),
        [0, 0, 0, 255],
        "the fill ate the black outline"
    );
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [255, 0, 0, 255]);
    // The interior is fenced off by the ring, so it stays empty.
    assert_eq!(d.get_pixel(0, 0, 5, 5).unwrap(), [0, 0, 0, 0]);

    // The inverse: filling a black shape must not escape into the background.
    let mut d = Document::new("t", 12, 12);
    d.rect(0, 0, 3, 3, 8, 8, [0, 0, 0, 255], true, 1).unwrap();
    d.bucket_fill(0, 0, 5, 5, [0, 255, 0, 255], 0).unwrap();
    assert_eq!(d.get_pixel(0, 0, 5, 5).unwrap(), [0, 255, 0, 255]);
    assert_eq!(
        d.get_pixel(0, 0, 0, 0).unwrap(),
        [0, 0, 0, 0],
        "the fill escaped into the transparent background"
    );
}

/// Same root cause as the fill: `from` = an opaque black repainted every
/// transparent pixel, turning the empty canvas fully opaque.
#[test]
fn replace_color_leaves_the_transparent_background_alone() {
    let mut d = Document::new("t", 8, 8);
    d.rect(0, 0, 2, 2, 5, 5, [0, 0, 0, 255], true, 1).unwrap();
    d.replace_color(0, 0, [0, 0, 0, 255], [255, 0, 0, 255], 0)
        .unwrap();
    assert_eq!(d.get_pixel(0, 0, 3, 3).unwrap(), [255, 0, 0, 255]);
    assert_eq!(
        d.get_pixel(0, 0, 0, 0).unwrap(),
        [0, 0, 0, 0],
        "replace_color painted the transparent background"
    );
}

/// A stroke wholly off-canvas inverted the clamped bbox, which panicked in
/// release as well as debug (`clamp` with min > max). Nothing is visible, so it
/// must clip silently like every other primitive.
#[test]
fn stroke_entirely_off_canvas_clips_instead_of_panicking() {
    let mut d = Document::new("t", 16, 16);
    for pts in [
        vec![(-100.0, -100.0, 2.0), (-90.0, -90.0, 2.0)],
        vec![(200.0, 200.0, 2.0), (300.0, 300.0, 2.0)],
        vec![(-50.0, 8.0, 2.0), (-40.0, 8.0, 2.0)],
    ] {
        d.stroke_f(0, 0, &pts, [255, 0, 0, 255], true, false)
            .unwrap();
    }
    // Canvas untouched, no panic.
    assert_eq!(d.get_pixel(0, 0, 8, 8).unwrap(), [0, 0, 0, 0]);
}

#[test]
fn snap_opaque_collapses_bloom_to_crisp_palette() {
    // A continuous-tone bloom: one bright core + one faint halo pixel, both
    // off-palette tints. Opaque snap should make the core a solid palette
    // colour and clear the faint halo — crisp, not soft.
    let mut d = Document::new("t", 4, 4);
    let pal = [[255, 0, 0, 255], [0, 0, 255, 255]];
    d.pencil(0, 0, &[(0, 0)], [200, 12, 12, 200], 1).unwrap(); // bright core
    d.pencil(0, 0, &[(1, 0)], [180, 30, 30, 30], 1).unwrap(); // faint halo
    d.snap_to_palette(&pal, None, None, AlphaSnap::Opaque(128));
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [255, 0, 0, 255]); // core → solid
    assert_eq!(d.get_pixel(0, 0, 1, 0).unwrap(), [0, 0, 0, 0]); // halo → cleared
}

#[test]
fn fx_resnaps_to_locked_palette_by_default() {
    // A blur across two flat palette blocks averages the boundary into
    // off-palette mud. With a palette locked, the continuous-tone FX path
    // must re-snap by default so every pixel stays on-palette.
    let mut d = Document::new("t", 4, 4);
    let pal = vec![[220, 30, 30, 255], [30, 30, 220, 255]];
    d.set_palette(pal.clone());
    d.apply_op(
        0,
        0,
        &json!({"op": "fill_cel", "color": [220, 30, 30, 255]}),
    )
    .unwrap();
    d.apply_op(
        0,
        0,
        &json!({"op": "rect", "x0": 2, "y0": 0, "x1": 3, "y1": 3, "color": [30, 30, 220, 255], "fill": true}),
    )
    .unwrap();
    d.apply_op(0, 0, &json!({"op": "blur", "radius": 1}))
        .unwrap();
    for y in 0..4 {
        for x in 0..4 {
            let p = d.get_pixel(0, 0, x, y).unwrap();
            assert!(pal.contains(&p), "pixel {p:?} at {x},{y} off-palette");
        }
    }
}

#[test]
fn fx_snap_false_keeps_continuous_tone() {
    // The same blur with `snap:false` opts out — the blended boundary is
    // allowed to stay off-palette.
    let mut d = Document::new("t", 4, 4);
    let pal = vec![[220, 30, 30, 255], [30, 30, 220, 255]];
    d.set_palette(pal.clone());
    d.apply_op(
        0,
        0,
        &json!({"op": "fill_cel", "color": [220, 30, 30, 255]}),
    )
    .unwrap();
    d.apply_op(
        0,
        0,
        &json!({"op": "rect", "x0": 2, "y0": 0, "x1": 3, "y1": 3, "color": [30, 30, 220, 255], "fill": true}),
    )
    .unwrap();
    d.apply_op(0, 0, &json!({"op": "blur", "radius": 1, "snap": false}))
        .unwrap();
    let has_off = (0..4)
        .flat_map(|y| (0..4).map(move |x| (x, y)))
        .any(|(x, y)| !pal.contains(&d.get_pixel(0, 0, x, y).unwrap()));
    assert!(
        has_off,
        "snap:false should leave off-palette blended pixels"
    );
}

#[test]
fn malformed_colour_errors_instead_of_black_fallback() {
    // A hex-string colour used to silently become BLACK — the footgun the
    // model benchmark surfaced (one run burned 13 calls repainting around
    // it). It must reject loudly at validation time now.
    for bad in [
        json!("#ff00ff"),
        json!({"r": 255, "g": 0, "b": 255}),
        json!([255, 0]),
        json!([255, 0, 300]),
    ] {
        let e = validate_batch_op(0, &json!({"op": "outline", "color": bad}))
            .expect_err("malformed colour must be rejected");
        assert!(e.contains("colour array"), "unhelpful error: {e}");
    }
    // Well-formed colours still pass, 3 or 4 channels.
    validate_batch_op(0, &json!({"op": "outline", "color": [255, 0, 255]})).unwrap();
    validate_batch_op(0, &json!({"op": "outline", "color": [255, 0, 255, 128]})).unwrap();
}

#[test]
fn gradient_map_remaps_luminance_keeps_alpha() {
    let mut d = Document::new("t", 4, 1);
    d.pencil(0, 0, &[(0, 0)], [20, 20, 20, 255], 1).unwrap(); // dark
    d.pencil(0, 0, &[(1, 0)], [240, 240, 240, 200], 1).unwrap(); // light, soft alpha
    d.apply_op(
        0,
        0,
        &json!({"op": "gradient_map", "stops": [
            {"pos": 0.0, "color": [10, 0, 40, 255]},
            {"pos": 1.0, "color": [255, 200, 80, 255]}
        ]}),
    )
    .unwrap();
    let dark = d.get_pixel(0, 0, 0, 0).unwrap();
    let light = d.get_pixel(0, 0, 1, 0).unwrap();
    // Dark maps near the first stop, light near the last; alpha preserved.
    assert!(
        dark[0] < 40 && dark[2] > 20,
        "dark → deep stop, got {dark:?}"
    );
    assert!(
        light[0] > 200 && light[1] > 150,
        "light → warm stop, got {light:?}"
    );
    assert_eq!(light[3], 200);
    // Transparent pixels untouched.
    assert_eq!(d.get_pixel(0, 0, 3, 0).unwrap(), [0, 0, 0, 0]);
}

#[test]
fn load_rejects_traversal_cel_paths() {
    let dir = std::env::temp_dir().join("atelier-cel-traversal-test");
    let _ = std::fs::remove_dir_all(&dir);
    let mut d = Document::new("t", 8, 8);
    d.fill_cel(0, 0, [1, 2, 3, 255]).unwrap();
    d.save(&dir).unwrap();
    assert!(Document::load(&dir).is_ok()); // the well-formed doc loads

    // Rewrite doc.json to point a cel at an escaping path.
    let jp = dir.join("doc.json");
    let s = std::fs::read_to_string(&jp).unwrap();
    let poisoned = s.replace("cels/L0_F0.png", "../../../../etc/passwd");
    assert_ne!(poisoned, s, "expected a cel path to rewrite");
    std::fs::write(&jp, poisoned).unwrap();
    match Document::load(&dir) {
        Err(e) => assert!(e.contains("suspicious cel path"), "unexpected error: {e}"),
        Ok(_) => panic!("traversal path must be refused"),
    }
}

#[test]
fn persisted_document_shape_is_strict() {
    let dir = std::env::temp_dir().join("atelier-strict-doc-shape-test");
    let _ = std::fs::remove_dir_all(&dir);
    let mut document = Document::new("strict", 8, 8);
    document.save(&dir).unwrap();
    let path = dir.join("doc.json");
    let current: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        current.get("reference").is_some_and(Value::is_null),
        "the current optional field is explicit, not inferred when absent"
    );

    let mut missing = current.clone();
    missing.as_object_mut().unwrap().remove("palette");
    std::fs::write(&path, serde_json::to_vec(&missing).unwrap()).unwrap();
    assert!(Document::load(&dir).is_err(), "missing fields must fail");

    let mut unknown = current.clone();
    unknown["linked"] = json!(true);
    std::fs::write(&path, serde_json::to_vec(&unknown).unwrap()).unwrap();
    assert!(Document::load(&dir).is_err(), "unknown fields must fail");

    let mut bad_blend = current;
    bad_blend["layers"][0]["blend"] = json!("source-over");
    std::fs::write(&path, serde_json::to_vec(&bad_blend).unwrap()).unwrap();
    let error = Document::load(&dir).err().expect("invalid blend must fail");
    assert!(error.contains("unknown blend"), "got: {error}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn layer_mutations_reject_unknown_blends() {
    let mut document = Document::new("strict", 8, 8);
    assert!(document.add_layer(None, 255, "source-over".into()).is_err());
    assert!(
        document
            .insert_layer(0, None, 255, "source-over".into())
            .is_err()
    );
    assert!(
        document
            .set_layer(0, None, None, Some("source-over".into()))
            .is_err()
    );
}

#[test]
fn sheet_image_errors_on_dimension_overflow() {
    // Built directly, past the studio's 4096 cap; a wild scale overflows the
    // frame-width u32 and must error instead of wrapping to a garbage buffer.
    let mut d = Document::new("t", 100_000, 4);
    d.fill_cel(0, 0, [1, 1, 1, 255]).unwrap();
    assert!(d.sheet_image(50_000).is_err()); // 100000 * 50000 overflows u32
}

#[test]
fn export_sheet_std_writes_engine_parsable_json() {
    let mut d = Document::new("runner", 8, 8);
    d.fill_cel(0, 0, [255, 0, 0, 255]).unwrap();
    d.add_frame(80, Some(0));
    d.add_tag("run", 0, 1, "forward").unwrap();
    let dir = std::env::temp_dir().join("atelier-std-json-test");
    let _ = std::fs::create_dir_all(&dir);
    let out = dir.join("runner.png");
    let r = d.export_sheet_std(&out, 2).unwrap();
    assert_eq!(r["meta_format"], "standard");
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(out.with_extension("json")).unwrap())
            .unwrap();
    // The hash layout engines parse: frames keyed by name, rect under
    // "frame", per-frame "duration", tags under meta.frameTags.
    let f0 = &meta["frames"]["runner 0"];
    assert_eq!(f0["frame"]["w"], 16); // 8px * scale 2
    assert_eq!(f0["frame"]["x"], 0);
    assert_eq!(meta["frames"]["runner 1"]["frame"]["x"], 16);
    assert_eq!(meta["frames"]["runner 1"]["duration"], 80);
    assert_eq!(meta["meta"]["image"], "runner.png");
    assert_eq!(meta["meta"]["frameTags"][0]["name"], "run");
    assert_eq!(meta["meta"]["size"]["w"], 32);
}

#[test]
fn stroke_f_keeps_subpixel_position() {
    // A 1px AA stroke centred on row 3 lights that row nearly solid; nudging
    // it half a pixel down splits its coverage between rows 3 and 4, so the
    // row-3 alpha must drop. If the point were rounded to an integer first
    // (the old double-quantize) both cases would be identical.
    let on_row = |y: f32| {
        let mut d = Document::new("t", 12, 8);
        d.stroke_f(
            0,
            0,
            &[(2.0, y, 1.0), (8.0, y, 1.0)],
            [255, 255, 255, 255],
            true,
            false,
        )
        .unwrap();
        d.get_pixel(0, 0, 5, 3).unwrap()[3]
    };
    let centred = on_row(3.0);
    let nudged = on_row(3.5);
    assert!(
        centred > nudged + 40,
        "sub-pixel nudge should lower row-3 coverage (centred={centred}, nudged={nudged})"
    );
}

#[test]
fn snap_flatten_melts_partial_alpha_onto_backdrop() {
    // A faint bluish bloom over a dark backdrop should flatten to an opaque
    // on-palette colour rather than staying semi-transparent off-palette.
    let mut d = Document::new("t", 4, 4);
    let pal = [[20, 20, 40, 255], [120, 140, 255, 255]];
    d.pencil(0, 0, &[(0, 0)], [168, 207, 255, 60], 1).unwrap();
    d.snap_to_palette(&pal, None, None, AlphaSnap::Flatten([20, 20, 40, 255]));
    let p = d.get_pixel(0, 0, 0, 0).unwrap();
    assert_eq!(p[3], 255); // fully opaque after flatten
    assert!(pal.contains(&p)); // and on-palette
}

#[test]
fn stroke_is_gap_free_union() {
    // A thin diagonal capsule: the union rasterizer must leave NO empty row
    // across the span (the gap-free property stacked beziers lack).
    let mut d = Document::new("t", 48, 48);
    d.stroke(
        0,
        0,
        &[(2, 2, 2), (45, 45, 2)],
        [255, 255, 255, 255],
        false,
        false,
    )
    .unwrap();
    for y in 4..44 {
        let any = (0..48).any(|x| d.get_pixel(0, 0, x, y).unwrap()[3] > 0);
        assert!(any, "row {y} had a gap");
    }
}

#[test]
fn stroke_tapers_toward_zero_width_tip() {
    let mut d = Document::new("t", 48, 16);
    d.stroke(
        0,
        0,
        &[(8, 8, 0), (40, 8, 10)],
        [255, 255, 255, 255],
        false,
        false,
    )
    .unwrap();
    let col = |x: i32| {
        (0..16)
            .filter(|&y| d.get_pixel(0, 0, x, y).unwrap()[3] > 0)
            .count()
    };
    assert!(col(10) <= 2, "tip should be ~1px, got {}", col(10));
    assert!(col(38) >= 7, "wide end should be ~10px, got {}", col(38));
}

#[test]
fn stroke_aa_emits_fractional_coverage() {
    // Bresenham yields zero partial-alpha pixels; the analytic coverage core
    // must produce a smooth AA edge.
    let mut d = Document::new("t", 32, 32);
    d.stroke(
        0,
        0,
        &[(4, 4, 3), (28, 28, 3)],
        [255, 255, 255, 255],
        true,
        false,
    )
    .unwrap();
    let frac = (0..32)
        .flat_map(|y| (0..32).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let a = d.get_pixel(0, 0, x, y).unwrap()[3];
            a > 0 && a < 255
        })
        .count();
    assert!(
        frac >= 20,
        "AA capsule should have ≥20 fractional pixels, got {frac}"
    );
}

#[test]
fn save_load_round_trip() {
    let dir = std::env::temp_dir().join(format!("atelier_doc_rt_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let mut d = Document::new("rt", 4, 4);
    let mut img = RgbaImage::from_pixel(2, 2, Rgba([0, 0, 0, 0]));
    img.put_pixel(0, 0, Rgba([10, 20, 30, 255]));
    d.set_cel(0, 0, 1, 1, img).unwrap();
    d.save(&dir).unwrap();

    let loaded = Document::load(&dir).unwrap();
    assert_eq!(loaded.meta.name, "rt");
    assert_eq!((loaded.meta.w, loaded.meta.h), (4, 4));
    // the cel is recorded in meta at the offset it was placed
    assert_eq!(loaded.meta.cels.len(), 1);
    let c = &loaded.meta.cels[0];
    assert_eq!((c.layer, c.frame, c.x, c.y), (0, 0, 1, 1));
    // the pixel painted into the cel survives the round-trip
    let cel_img = image::open(dir.join(&c.file)).unwrap().to_rgba8();
    assert_eq!(cel_img.get_pixel(0, 0).0, [10, 20, 30, 255]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn structure_reports_layers_frames_and_cels() {
    let mut d = Document::new("s", 4, 4);
    d.add_layer(None, 255, "normal".into()).unwrap();
    let img = RgbaImage::from_pixel(1, 1, Rgba([1, 1, 1, 255]));
    d.set_cel(1, 0, 0, 0, img).unwrap();
    let v = d.structure();
    assert_eq!(v["name"], "s");
    assert_eq!(v["layers"].as_array().unwrap().len(), 2);
    assert_eq!(v["frames"].as_array().unwrap().len(), 1);
    let cels = v["cels"].as_array().unwrap();
    assert_eq!(cels.len(), 1);
    assert_eq!(cels[0]["layer"], 1);
    assert_eq!(cels[0]["frame"], 0);
}

#[test]
fn filled_ellipse_top_row_is_wide_not_a_nub() {
    // Regression: the old rasteriser left a single-pixel spike at each
    // cardinal tip. The half-pixel-inflated test rounds them.
    let mut d = Document::new("t", 48, 24);
    d.ellipse(0, 0, 24, 12, 12, 8, [0, 200, 0, 255], true)
        .unwrap();
    let top = 12 - 8; // y of the extreme top row
    let width = (0..48)
        .filter(|x| d.get_pixel(0, 0, *x, top).unwrap()[3] > 0)
        .count();
    assert!(width >= 5, "top row width {} — looks like a nub", width);
}

#[test]
fn ellipse_outline_is_closed_and_thin() {
    // Every outline pixel is opaque; the centre stays empty (true ring).
    let mut d = Document::new("t", 40, 40);
    d.ellipse(0, 0, 20, 20, 15, 15, [200, 0, 0, 255], false)
        .unwrap();
    assert_eq!(d.get_pixel(0, 0, 20, 20).unwrap(), [0, 0, 0, 0]); // hollow centre
    assert!(d.get_pixel(0, 0, 20, 5).unwrap()[3] > 0); // top of ring drawn
    assert!(d.get_pixel(0, 0, 5, 20).unwrap()[3] > 0); // left of ring drawn
}

#[test]
fn filled_polygon_covers_interior_only() {
    let mut d = Document::new("t", 24, 24);
    let tri = [(2, 2), (20, 2), (11, 18)];
    d.polygon(0, 0, &tri, [60, 60, 200, 255], true).unwrap();
    assert!(d.get_pixel(0, 0, 11, 8).unwrap()[3] > 0); // inside
    assert_eq!(d.get_pixel(0, 0, 2, 17).unwrap(), [0, 0, 0, 0]); // outside (bottom-left)
    assert!(d.get_pixel(0, 0, 11, 18).unwrap()[3] > 0); // apex vertex stroked
}

#[test]
fn polyline_draws_segments_and_can_close() {
    let mut d = Document::new("t", 16, 16);
    d.polyline(0, 0, &[(1, 1), (10, 1), (10, 10)], [9, 9, 9, 255], 1, false)
        .unwrap();
    assert!(d.get_pixel(0, 0, 5, 1).unwrap()[3] > 0); // along first segment
    assert!(d.get_pixel(0, 0, 10, 5).unwrap()[3] > 0); // along second segment
    assert_eq!(d.get_pixel(0, 0, 5, 10).unwrap(), [0, 0, 0, 0]); // open: no closing edge
}

#[test]
fn form_sphere_gives_volume_toward_light() {
    let mut d = Document::new("t", 32, 32);
    // A flat-filled disc — no volume yet.
    d.ellipse(0, 0, 16, 16, 12, 12, [120, 120, 120, 255], true)
        .unwrap();
    let ramp: Vec<[u8; 4]> = vec![
        [20, 20, 20, 255],
        [70, 70, 70, 255],
        [120, 120, 120, 255],
        [180, 180, 180, 255],
        [240, 240, 240, 255],
    ];
    d.form(0, 0, "top-left", "sphere", None, Some(ramp), 1.0)
        .unwrap();
    let lum = |p: [u8; 4]| p[0] as i32 + p[1] as i32 + p[2] as i32;
    let tl = d.get_pixel(0, 0, 9, 9).unwrap(); // toward the light
    let br = d.get_pixel(0, 0, 23, 23).unwrap(); // away from it
    assert!(tl[3] > 0 && br[3] > 0); // both still opaque
    assert!(
        lum(tl) > lum(br),
        "lit corner {:?} should outshine shadow corner {:?}",
        tl,
        br
    );
}

#[test]
fn form_auto_brightens_interior_over_edge() {
    let mut d = Document::new("t", 32, 32);
    d.ellipse(0, 0, 16, 16, 12, 12, [120, 120, 120, 255], true)
        .unwrap();
    d.form(0, 0, "top", "auto", None, None, 1.0).unwrap();
    let lum = |p: [u8; 4]| p[0] as i32 + p[1] as i32 + p[2] as i32;
    let core = d.get_pixel(0, 0, 16, 16).unwrap(); // deep interior
    let edge = d.get_pixel(0, 0, 16, 27).unwrap(); // bottom rim
    assert!(core[3] > 0 && edge[3] > 0);
    assert!(
        lum(core) > lum(edge),
        "interior {:?} should be brighter than the rim {:?}",
        core,
        edge
    );
}

#[test]
fn text_renders_known_glyph_exactly() {
    // 'I' is a top bar, a centre stem, and a bottom bar in its 3×5 cell.
    let mut d = Document::new("t", 8, 8);
    d.text(0, 0, 1, 1, "I", [255, 255, 255, 255], 1).unwrap();
    let on = |x, y| d.get_pixel(0, 0, x, y).unwrap()[3] > 0;
    // top bar (y=1): all three columns lit
    assert!(on(1, 1) && on(2, 1) && on(3, 1));
    // centre stem (x=2) for the three middle rows
    assert!(on(2, 2) && on(2, 3) && on(2, 4));
    // the stem's flanks stay empty
    assert!(!on(1, 2) && !on(3, 2));
    // bottom bar (y=5): all three columns lit
    assert!(on(1, 5) && on(2, 5) && on(3, 5));
}

#[test]
fn text_size_two_doubles_every_pixel() {
    // Each lit cell becomes a 2×2 block, so the top bar spans y=0..1.
    let mut d = Document::new("t", 16, 16);
    d.text(0, 0, 0, 0, "I", [10, 20, 30, 255], 2).unwrap();
    let on = |x, y| d.get_pixel(0, 0, x, y).unwrap()[3] > 0;
    // top bar doubled vertically and across all six scaled columns
    for x in 0..6 {
        assert!(on(x, 0) && on(x, 1), "top bar block missing at x={x}");
    }
    // centre stem (cell col 1 -> scaled cols 2,3) doubled at the second row band
    assert!(on(2, 2) && on(3, 2) && on(2, 3) && on(3, 3));
}

#[test]
fn text_returns_layout_width() {
    // size 1: glyph is 3px, +1px spacing between glyphs, no trailing space.
    let mut d = Document::new("t", 32, 8);
    assert_eq!(d.text(0, 0, 0, 0, "", [0; 4], 1).unwrap(), 0);
    assert_eq!(d.text(0, 0, 0, 0, "I", [255; 4], 1).unwrap(), 3);
    assert_eq!(d.text(0, 0, 0, 0, "II", [255; 4], 1).unwrap(), 7); // 3 + 1 + 3
    // size 2 scales the whole width.
    assert_eq!(d.text(0, 0, 0, 0, "II", [255; 4], 2).unwrap(), 14);
}

#[test]
fn text_size_is_clamped_against_runaway_scaling() {
    // A huge size completes (no hang) and behaves as the 64 clamp ceiling.
    let mut d = Document::new("t", 256, 512);
    let clamped = d.text(0, 0, 0, 0, "I", [255; 4], 64).unwrap();
    let huge = d.text(0, 0, 0, 0, "I", [255; 4], 9999).unwrap();
    assert_eq!(huge, clamped);
    assert_eq!(huge, 3 * 64); // 192px, the clamped glyph width
}

#[test]
fn text_unknown_char_is_hollow_box() {
    // '@' is not in the font, so it renders as the 3×5 hollow box.
    let mut d = Document::new("t", 8, 8);
    d.text(0, 0, 0, 0, "@", [255, 255, 255, 255], 1).unwrap();
    let on = |x, y| d.get_pixel(0, 0, x, y).unwrap()[3] > 0;
    assert!(on(0, 0) && on(2, 0) && on(0, 4) && on(2, 4)); // corners
    assert!(!on(1, 2)); // hollow centre
}

#[test]
fn batch_text_op_validates() {
    // The "text" op must be a known batch op with its required keys checked.
    let ok = json!({"op": "text", "x": 0, "y": 0, "text": "HI", "color": [255, 255, 255]});
    assert!(validate_batch_op(0, &ok).is_ok());
    // Missing the required `text` key is rejected.
    let missing = json!({"op": "text", "x": 0, "y": 0, "color": [255, 255, 255]});
    assert!(validate_batch_op(0, &missing).is_err());
}

/// Two opaque full-cel layers, top with `mode`, flattened at frame 0.
fn blend_two(mode: &str, bottom: [u8; 4], top: [u8; 4]) -> [u8; 4] {
    let mut d = Document::new("t", 1, 1);
    d.fill_cel(0, 0, bottom).unwrap();
    let l = d.add_layer(None, 255, mode.into()).unwrap();
    d.fill_cel(l, 0, top).unwrap();
    d.flatten(0).get_pixel(0, 0).0
}

#[test]
fn normal_blend_matches_plain_source_over() {
    // Opaque top fully covers the backdrop, unchanged from old compositor.
    assert_eq!(
        blend_two("normal", [255, 0, 0, 255], [0, 255, 0, 255]),
        [0, 255, 0, 255]
    );
}

#[test]
fn multiply_darkens_screen_lightens() {
    // red x green channelwise -> black; red screen green -> yellow.
    assert_eq!(
        blend_two("multiply", [255, 0, 0, 255], [0, 255, 0, 255]),
        [0, 0, 0, 255]
    );
    assert_eq!(
        blend_two("screen", [255, 0, 0, 255], [0, 255, 0, 255]),
        [255, 255, 0, 255]
    );
    assert_eq!(
        blend_two("add", [200, 100, 0, 255], [100, 200, 50, 255]),
        [255, 255, 50, 255]
    );
}

#[test]
fn multiply_over_empty_backdrop_keeps_source() {
    // No backdrop (αb=0): a multiply layer must not collapse to black.
    let mut d = Document::new("t", 1, 1);
    let l = d.add_layer(None, 255, "multiply".into()).unwrap();
    d.fill_cel(l, 0, [40, 90, 160, 255]).unwrap();
    assert_eq!(d.flatten(0).get_pixel(0, 0).0, [40, 90, 160, 255]);
}

#[test]
fn layer_opacity_blends_toward_backdrop() {
    // A 50%-opacity red layer over an opaque black backdrop flattens to ~half red.
    let mut d = Document::new("t", 1, 1);
    d.fill_cel(0, 0, [0, 0, 0, 255]).unwrap();
    let l = d.add_layer(None, 128, "normal".into()).unwrap();
    d.fill_cel(l, 0, [255, 0, 0, 255]).unwrap();
    let p = d.flatten(0).get_pixel(0, 0).0;
    assert!(
        (p[0] as i32 - 128).abs() <= 2,
        "expected ~128, got {}",
        p[0]
    );
    assert_eq!(p[3], 255);
}

#[test]
fn render_preview_tile_and_region_size() {
    let d = Document::new("t", 4, 4);
    let tiled = d.render_preview(0, 1, None, false, 3, None).unwrap();
    assert_eq!((tiled.width(), tiled.height()), (12, 12)); // 3×3 grid
    let crop = d
        .render_preview(0, 1, Some((0, 0, 1, 1)), false, 1, None)
        .unwrap();
    assert_eq!((crop.width(), crop.height()), (2, 2));
}

#[test]
fn get_pixel_reads_back_a_drawn_pixel() {
    let mut d = Document::new("t", 8, 8);
    d.pencil(0, 0, &[(3, 4)], [10, 20, 30, 255], 1).unwrap();
    assert_eq!(d.get_pixel(0, 0, 3, 4).unwrap(), [10, 20, 30, 255]);
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 0]); // untouched
    assert_eq!(d.get_pixel(0, 0, 99, 99).unwrap(), [0, 0, 0, 0]); // OOB
}

#[test]
fn shift_wrap_rolls_pixels_around() {
    let mut d = Document::new("t", 4, 4);
    d.pencil(0, 0, &[(3, 0)], [9, 9, 9, 255], 1).unwrap();
    d.shift(0, 0, 1, 0, true).unwrap(); // (3+1)%4 = 0
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [9, 9, 9, 255]);
}

#[test]
fn move_region_clears_source_and_fills_dest() {
    let mut d = Document::new("t", 8, 8);
    d.pencil(0, 0, &[(1, 1)], [5, 5, 5, 255], 1).unwrap();
    d.move_region(0, 0, 1, 1, 1, 1, 3, 0).unwrap();
    assert_eq!(d.get_pixel(0, 0, 1, 1).unwrap(), [0, 0, 0, 0]); // source cleared
    assert_eq!(d.get_pixel(0, 0, 4, 1).unwrap(), [5, 5, 5, 255]); // moved here
}

#[test]
fn move_region_does_not_punch_a_hole_in_dest_art() {
    // Move a 2x2 block that contains a transparent corner over existing art;
    // the transparent corner must NOT erase the art already there.
    let mut d = Document::new("t", 8, 8);
    d.pencil(0, 0, &[(0, 0)], [9, 9, 9, 255], 1).unwrap(); // only opaque pixel of the 2x2 source
    d.pencil(0, 0, &[(6, 2)], [7, 7, 7, 255], 1).unwrap(); // dest art, under the block's transparent corner
    // Move the 2x2 rect (0,0)-(1,1) by (+5,+1): source(0,0)->(5,1), source(1,1)->(6,2).
    d.move_region(0, 0, 0, 0, 1, 1, 5, 1).unwrap();
    assert_eq!(d.get_pixel(0, 0, 5, 1).unwrap(), [9, 9, 9, 255]); // opaque pixel landed
    assert_eq!(d.get_pixel(0, 0, 6, 2).unwrap(), [7, 7, 7, 255]); // dest art survived the transparent corner
}

#[test]
fn blur_spreads_a_dot() {
    let mut d = Document::new("t", 5, 5);
    d.pencil(0, 0, &[(2, 2)], [255, 255, 255, 255], 1).unwrap();
    d.blur(0, 0, 1, None).unwrap();
    assert!(d.get_pixel(0, 0, 2, 1).unwrap()[3] > 0); // bled into neighbour
    assert!(d.get_pixel(0, 0, 2, 2).unwrap()[3] < 255); // centre softened
}

#[test]
fn drop_shadow_adds_offset_silhouette() {
    let mut d = Document::new("t", 8, 8);
    d.rect(0, 0, 1, 1, 3, 3, [255, 255, 255, 255], true, 1)
        .unwrap();
    d.drop_shadow(0, 0, 2, 2, [0, 0, 0, 255], 200, 0).unwrap();
    assert_eq!(d.get_pixel(0, 0, 2, 2).unwrap(), [255, 255, 255, 255]); // art still on top
    assert!(d.get_pixel(0, 0, 5, 5).unwrap()[3] > 0); // shadow offset by (2,2)
}

#[test]
fn gradient_linear_lerps_between_stops() {
    let mut d = Document::new("t", 4, 1);
    d.gradient(
        0,
        0,
        "linear",
        0,
        0,
        3,
        0,
        vec![(0.0, [0, 0, 0, 255]), (1.0, [255, 255, 255, 255])],
        "none",
        0,
        None,
        false,
    )
    .unwrap();
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 255]);
    assert_eq!(d.get_pixel(0, 0, 3, 0).unwrap(), [255, 255, 255, 255]);
    assert_eq!(d.get_pixel(0, 0, 1, 0).unwrap(), [85, 85, 85, 255]); // t=1/3
}

#[test]
fn gradient_dither_uses_only_stop_colors() {
    let mut d = Document::new("t", 8, 8);
    let (a, b) = ([10, 20, 30, 255], [200, 210, 220, 255]);
    d.gradient(
        0,
        0,
        "linear",
        0,
        0,
        7,
        0,
        vec![(0.0, a), (1.0, b)],
        "bayer",
        0,
        None,
        false,
    )
    .unwrap();
    for x in 0..8 {
        for y in 0..8 {
            let p = d.get_pixel(0, 0, x, y).unwrap();
            assert!(p == a || p == b, "dither pixel {:?} not a stop colour", p);
        }
    }
}

#[test]
fn gradient_region_clips() {
    let mut d = Document::new("t", 8, 8);
    d.gradient(
        0,
        0,
        "linear",
        0,
        0,
        7,
        0,
        vec![(0.0, [9, 9, 9, 255]), (1.0, [9, 9, 9, 255])],
        "none",
        0,
        Some((2, 2, 5, 5)),
        false,
    )
    .unwrap();
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 0]); // outside region untouched
    assert_eq!(d.get_pixel(0, 0, 3, 3).unwrap(), [9, 9, 9, 255]); // inside painted
}

#[test]
fn gradient_region_fully_off_canvas_is_a_no_op() {
    let mut d = Document::new("t", 8, 8);
    d.gradient(
        0,
        0,
        "linear",
        0,
        0,
        7,
        0,
        vec![(0.0, [9, 9, 9, 255]), (1.0, [9, 9, 9, 255])],
        "none",
        0,
        Some((20, 20, 30, 30)),
        false,
    )
    .unwrap(); // succeeds without painting anything
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 0]);
}

#[test]
fn scatter_is_deterministic_and_density_bounded() {
    let count = |seed: u64, dens: f32| {
        let mut d = Document::new("t", 16, 16);
        d.scatter(0, 0, 0, 0, 15, 15, &[[255, 0, 0, 255]], dens, seed, 1)
            .unwrap();
        (0..16)
            .flat_map(|y| (0..16).map(move |x| (x, y)))
            .filter(|(x, y)| d.get_pixel(0, 0, *x, *y).unwrap()[3] > 0)
            .count()
    };
    assert_eq!(count(0, 0.0), 0); // density 0 paints nothing
    assert_eq!(count(7, 0.3), count(7, 0.3)); // same seed reproduces
    assert!(count(7, 0.3) < count(7, 0.8)); // higher density paints more
}

#[test]
fn symmetry_mirrors_across_vertical_axis() {
    let mut d = Document::new("t", 4, 4);
    d.pencil(0, 0, &[(0, 1)], [9, 9, 9, 255], 1).unwrap();
    d.symmetry(0, 0, Some(1), None, true, false).unwrap(); // reflect left over column 1
    assert_eq!(d.get_pixel(0, 0, 2, 1).unwrap(), [9, 9, 9, 255]); // 2*1-0 = 2
}

#[test]
fn quantize_snaps_to_palette() {
    let mut d = Document::new("t", 4, 4);
    d.pencil(0, 0, &[(0, 0)], [250, 10, 10, 255], 1).unwrap();
    d.pencil(0, 0, &[(1, 0)], [10, 10, 250, 255], 1).unwrap();
    d.quantize(0, 0, vec![[255, 0, 0, 255], [0, 0, 255, 255]], 2)
        .unwrap();
    assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [255, 0, 0, 255]);
    assert_eq!(d.get_pixel(0, 0, 1, 0).unwrap(), [0, 0, 255, 255]);
}

#[test]
fn quantize_derives_palette_by_median_cut() {
    let mut d = Document::new("t", 4, 4);
    d.fill_cel(0, 0, [20, 30, 40, 255]).unwrap();
    let pal = d.quantize(0, 0, vec![], 4).unwrap();
    assert!(!pal.is_empty() && pal.len() <= 4);
}

#[test]
fn adjust_hue_rotates_red_toward_green() {
    let mut d = Document::new("t", 2, 2);
    d.fill_cel(0, 0, [200, 0, 0, 255]).unwrap(); // hue 0
    d.adjust(0, 0, None, 120.0, 0.0, 0.0).unwrap(); // +120° → green
    let p = d.get_pixel(0, 0, 0, 0).unwrap();
    assert!(
        p[1] > p[0] && p[1] > p[2],
        "expected green-dominant, got {:?}",
        p
    );
}

fn doc_with_frames(n: usize) -> Document {
    let mut d = Document::new("t", 4, 4);
    while d.meta.frames.len() < n {
        d.add_frame(100, None);
    }
    d
}

#[test]
fn no_tag_plays_whole_timeline_forward() {
    let d = doc_with_frames(4);
    assert_eq!(d.play_sequence(None).unwrap(), vec![0, 1, 2, 3]);
}

#[test]
fn forward_tag_is_inclusive_range() {
    let mut d = doc_with_frames(5);
    d.add_tag("walk", 1, 3, "forward").unwrap();
    assert_eq!(d.play_sequence(Some("walk")).unwrap(), vec![1, 2, 3]);
}

#[test]
fn reverse_tag_plays_high_to_low() {
    let mut d = doc_with_frames(5);
    d.add_tag("rev", 1, 3, "reverse").unwrap();
    assert_eq!(d.play_sequence(Some("rev")).unwrap(), vec![3, 2, 1]);
}

#[test]
fn pingpong_does_not_duplicate_endpoints() {
    let mut d = doc_with_frames(4);
    d.add_tag("blink", 0, 2, "pingpong").unwrap();
    // open -> half -> closed -> half (-> loops to open), no double closed/open
    assert_eq!(d.play_sequence(Some("blink")).unwrap(), vec![0, 1, 2, 1]);
}

#[test]
fn pingpong_two_frame_range_has_no_inner_turnaround() {
    let mut d = doc_with_frames(3);
    d.add_tag("pp", 0, 1, "pingpong").unwrap();
    assert_eq!(d.play_sequence(Some("pp")).unwrap(), vec![0, 1]);
}

#[test]
fn unknown_tag_errors() {
    let d = doc_with_frames(2);
    assert!(d.play_sequence(Some("nope")).is_err());
}

#[test]
fn tag_range_clamps_to_existing_frames() {
    // A tag added when there were more frames must not index out of bounds.
    let mut d = doc_with_frames(5);
    d.add_tag("big", 0, 4, "forward").unwrap();
    d.meta.frames.truncate(3);
    assert_eq!(d.play_sequence(Some("big")).unwrap(), vec![0, 1, 2]);
}

#[test]
fn export_sheet_writes_png_and_json_sidecar() {
    let dir = std::env::temp_dir().join(format!("atelier_sheet_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut d = doc_with_frames(3);
    d.fill_cel(0, 0, [255, 0, 0, 255]).unwrap();
    let out = dir.join("sheet.png");
    d.export_sheet(&out, 2).unwrap();
    // 3 frames × (4·2) wide, (4·2) tall.
    let png = image::open(&out).unwrap().to_rgba8();
    assert_eq!(png.dimensions(), (24, 8));
    let json: Value =
        serde_json::from_str(&std::fs::read_to_string(out.with_extension("json")).unwrap())
            .unwrap();
    assert_eq!(json["frames"].as_array().unwrap().len(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_gif_writes_nonempty_file() {
    let dir = std::env::temp_dir().join(format!("atelier_gif_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut d = doc_with_frames(2);
    d.fill_cel(0, 0, [0, 255, 0, 255]).unwrap();
    d.fill_cel(0, 1, [0, 0, 255, 255]).unwrap();
    let out = dir.join("anim.gif");
    let n = d.export_gif(&out, 1, None).unwrap();
    assert_eq!(n, 2);
    let bytes = std::fs::metadata(&out).unwrap().len();
    assert!(bytes > 0, "gif should be nonempty");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `acTL` frame count from APNG bytes: scan for the chunk type marker, read
/// the following 4-byte big-endian `num_frames` field (start of its data).
fn apng_frame_count(bytes: &[u8]) -> Option<u32> {
    let pos = bytes.windows(4).position(|w| w == b"acTL")?;
    let n = &bytes[pos + 4..pos + 8];
    Some(u32::from_be_bytes([n[0], n[1], n[2], n[3]]))
}

/// First `fcTL`'s `(delay_num, delay_den)` from APNG bytes: the two big-endian
/// u16 fields at offsets 20 and 22 into the chunk data (after the marker).
fn apng_first_frame_delay(bytes: &[u8]) -> Option<(u16, u16)> {
    let pos = bytes.windows(4).position(|w| w == b"fcTL")? + 4;
    let num = u16::from_be_bytes([bytes[pos + 20], bytes[pos + 21]]);
    let den = u16::from_be_bytes([bytes[pos + 22], bytes[pos + 23]]);
    Some((num, den))
}

#[test]
fn export_apng_is_animated_png_with_matching_frame_count() {
    let dir = std::env::temp_dir().join(format!("atelier_apng_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut d = doc_with_frames(2);
    d.fill_cel(0, 0, [0, 255, 0, 128]).unwrap(); // partial alpha — APNG keeps it
    d.fill_cel(0, 1, [0, 0, 255, 255]).unwrap();
    let out = dir.join("anim.png");
    let n = d.export_apng(&out, 2, None).unwrap();
    assert_eq!(n, 2);
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
    assert_eq!(apng_frame_count(&bytes), Some(2), "acTL frame count");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_apng_honours_tag_direction() {
    let dir = std::env::temp_dir().join(format!("atelier_apng_tag_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut d = doc_with_frames(3);
    // pingpong over 0..2 plays 0,1,2,1 — 4 frames from a 3-frame range.
    d.add_tag("pp", 0, 2, "pingpong").unwrap();
    let out = dir.join("pp.png");
    let n = d.export_apng(&out, 1, Some("pp")).unwrap();
    assert_eq!(n, 4);
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(
        apng_frame_count(&bytes),
        Some(4),
        "acTL matches pingpong len"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_apng_long_delay_does_not_wrap() {
    let dir = std::env::temp_dir().join(format!("atelier_apng_long_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut d = doc_with_frames(1);
    d.set_frame_duration(0, 70_000).unwrap(); // > u16::MAX ms — used to wrap
    let out = dir.join("long.png");
    d.export_apng(&out, 1, None).unwrap();
    let bytes = std::fs::read(&out).unwrap();
    // 70_000ms is encoded as 7000/100s, not a truncated u16.
    assert_eq!(apng_first_frame_delay(&bytes), Some((7000, 100)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn per_op_opacity_blends_in_batch() {
    let mut d = Document::new("t", 2, 2);
    d.fill_cel(0, 0, [0, 0, 0, 255]).unwrap();
    d.apply_op(0, 0, &json!({"op":"rect","x0":0,"y0":0,"x1":1,"y1":1,"color":[200,0,0],"fill":true,"opacity":128})).unwrap();
    let p = d.get_pixel(0, 0, 0, 0).unwrap();
    assert!(
        (p[0] as i32 - 100).abs() <= 2,
        "expected ~100, got {}",
        p[0]
    ); // 50% red over black
    assert_eq!(p[3], 255);
}

#[test]
fn erase_turns_any_op_into_an_eraser() {
    // Drawing [0,0,0,0] is a no-op under source-over, so before `erase` there
    // was no way to punch a shaped hole — every showcase FX agent wanted one.
    let mut d = Document::new("t", 4, 4);
    d.fill_cel(0, 0, [200, 0, 0, 255]).unwrap();
    d.apply_op(
        0,
        0,
        &json!({"op":"ellipse","cx":1,"cy":1,"rx":1,"ry":1,"color":[1,2,3],"fill":true,"erase":true}),
    )
    .unwrap();
    assert_eq!(d.get_pixel(0, 0, 1, 1).unwrap(), [0, 0, 0, 0], "hole");
    assert_eq!(d.get_pixel(0, 0, 3, 3).unwrap(), [200, 0, 0, 255], "kept");
    // The stencil colour never lands.
    for y in 0..4 {
        for x in 0..4 {
            assert_ne!(d.get_pixel(0, 0, x, y).unwrap(), [1, 2, 3, 255]);
        }
    }
}

#[test]
fn silhouette_center_of_known_shape() {
    let mut d = Document::new("t", 6, 6);
    // Filled 3×3 block from (1,1) to (3,3): bbox-centre is (2,2).
    d.rect(0, 0, 1, 1, 3, 3, [255, 255, 255, 255], true, 1)
        .unwrap();
    let c = d.silhouette_center(None, 0, None).unwrap().unwrap();
    assert_eq!(c, [2.0, 2.0]);
}

#[test]
fn seam_axis_solid_is_seamless_edge_mismatch_is_not() {
    let mut d = Document::new("t", 4, 4);
    d.fill_cel(0, 0, [120, 120, 120, 255]).unwrap();
    // A solid cel tiles seamlessly: far edge == near edge on both axes.
    let (mh, _, _) = d.seam_axis(None, 0, true, 8).unwrap();
    let (mv, _, _) = d.seam_axis(None, 0, false, 8).unwrap();
    assert_eq!(mh, 0);
    assert_eq!(mv, 0);
    // Recolour the far (right) column so it no longer matches x=0.
    d.line(0, 0, 3, 0, 3, 3, [10, 10, 10, 255], 1).unwrap();
    let (mh2, max_delta, _) = d.seam_axis(None, 0, true, 8).unwrap();
    assert!(mh2 > 0, "edge mismatch should be detected");
    assert!(max_delta > 8, "delta should exceed threshold");
}

// -- hardening regression tests (the review pass) -----------------------------

#[test]
fn huge_coordinates_cannot_wedge_the_drawing_primitives() {
    let mut d = Document::new("t", 8, 8);
    // Each of these used to loop over the raw input span (billions of steps).
    d.rect(0, 0, 0, 0, i32::MAX, i32::MAX, [9, 9, 9, 255], true, 1)
        .unwrap();
    assert_eq!(d.get_pixel(0, 0, 7, 7).unwrap(), [9, 9, 9, 255]);
    d.ellipse(0, 0, 4, 4, i32::MAX, i32::MAX, [1, 1, 1, 255], true)
        .unwrap();
    assert_eq!(d.get_pixel(0, 0, 4, 4).unwrap(), [1, 1, 1, 255]);
    d.line(0, 0, -2_000_000_000, 0, 2_000_000_000, 7, [5, 5, 5, 255], 1)
        .unwrap();
    // A horizontal-ish line across the whole canvas still lands.
    assert_eq!(d.get_pixel(0, 0, 4, 3).unwrap()[3], 255);
    // Fully off-canvas line: clipped away, no hang, no pixels.
    let mut d2 = Document::new("t", 8, 8);
    d2.line(
        0,
        0,
        i32::MAX - 10,
        i32::MAX - 10,
        i32::MAX,
        i32::MAX,
        [1, 1, 1, 255],
        1,
    )
    .unwrap();
    assert_eq!(d2.opaque_count(None, 0).unwrap(), 0);
    // brush/size clamps: an absurd brush size covers the canvas, it doesn't loop.
    d2.pencil(0, 0, &[(4, 4)], [7, 7, 7, 255], i32::MAX)
        .unwrap();
    assert_eq!(d2.opaque_count(None, 0).unwrap(), 64);
}

#[test]
fn fx_parameters_are_clamped_to_sane_bounds() {
    let mut d = Document::new("t", 8, 8);
    d.rect(0, 0, 0, 0, 7, 7, [200, 30, 30, 255], true, 1)
        .unwrap();
    // These used to run O(param) loops straight from caller input.
    d.bevel(0, 0, [255, 255, 255, 128], [0, 0, 0, 128], i32::MAX)
        .unwrap();
    d.noise(
        0,
        0,
        "cloud",
        0,
        0,
        7,
        7,
        4.0,
        u32::MAX,
        1,
        vec![(0.0, [0, 0, 0, 255]), (1.0, [255, 255, 255, 255])],
        false,
    )
    .unwrap();
    d.blur(0, 0, i32::MAX, None).unwrap();
    d.scatter(0, 0, 0, 0, 7, 7, &[[1, 2, 3, 255]], 1.0, 42, i32::MAX)
        .unwrap();
}

#[test]
fn dither_ramp_rejects_an_unknown_pattern() {
    let mut d = Document::new("t", 4, 4);
    let e = d
        .dither_ramp(
            0,
            0,
            None,
            &[[0, 0, 0, 255], [255, 255, 255, 255]],
            "v",
            "bogus",
            false,
        )
        .unwrap_err();
    assert!(e.contains("unknown pattern"), "got: {e}");
}

#[test]
fn batch_wrapper_scalars_reject_out_of_range_values() {
    // 300 as u8 == 44: truncation used to silently apply a wrong opacity.
    let e = validate_batch_op(
        0,
        &json!({"op": "rect", "x0": 0, "y0": 0, "x1": 1, "y1": 1,
        "color": [1, 2, 3], "opacity": 300}),
    )
    .unwrap_err();
    assert!(e.contains("opacity"), "got: {e}");
    let e = validate_batch_op(0, &json!({"op": "glow", "intensity": 256})).unwrap_err();
    assert!(e.contains("intensity"), "got: {e}");
    let e = validate_batch_op(
        0,
        &json!({"op": "drop_shadow", "color": [1, 2, 3], "shadow_opacity": 999}),
    )
    .unwrap_err();
    assert!(e.contains("shadow_opacity"), "got: {e}");
    for bad in [
        json!({"op": "clear_cel", "blend_mode": "source-over"}),
        json!({"op": "clear_cel", "blend_mode": 1}),
        json!({"op": "clear_cel", "erase": "true"}),
    ] {
        assert!(validate_batch_op(0, &bad).is_err(), "accepted {bad}");
    }
    // And the good path still passes.
    assert!(validate_batch_op(0, &json!({"op": "glow", "intensity": 255})).is_ok());
}

#[test]
fn save_writes_only_dirtied_cels_and_sweeps_stale_files() {
    let dir = std::env::temp_dir().join(format!("atelier-dirty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let modified = |p: &std::path::Path| std::fs::metadata(p).unwrap().modified().unwrap();
    let mut d = Document::new("t", 4, 4);
    d.add_frame(100, None);
    d.rect(0, 0, 0, 0, 1, 1, [1, 1, 1, 255], true, 1).unwrap();
    d.rect(0, 1, 0, 0, 1, 1, [2, 2, 2, 255], true, 1).unwrap();
    d.save(&dir).unwrap();
    let f0 = dir.join("cels/L0_F0.png");
    let f1 = dir.join("cels/L0_F1.png");
    let m0 = modified(&f0);
    let m1 = modified(&f1);
    std::thread::sleep(std::time::Duration::from_millis(20));
    // Edit ONLY frame 0 and re-save: frame 1's file must not be rewritten.
    d.rect(0, 0, 2, 2, 3, 3, [3, 3, 3, 255], true, 1).unwrap();
    d.save(&dir).unwrap();
    assert!(modified(&f0) > m0, "dirtied cel should be rewritten");
    assert_eq!(modified(&f1), m1, "untouched cel must not be re-encoded");
    // Deleting frame 1 must sweep its file (and keep frame 0's).
    d.frame_ops("delete", 1, None, None).unwrap();
    d.save(&dir).unwrap();
    assert!(!f1.exists(), "stale cel file should be swept");
    assert!(f0.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_load_round_trip_still_recovers_every_cel() {
    // The dirty-set must never lose an edit: draw on both frames, reload.
    let dir = std::env::temp_dir().join(format!("atelier-dirty-rt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut d = Document::new("t", 4, 4);
    d.add_frame(100, Some(0));
    d.rect(0, 0, 0, 0, 1, 1, [10, 0, 0, 255], true, 1).unwrap();
    d.rect(0, 1, 2, 2, 3, 3, [0, 0, 10, 255], true, 1).unwrap();
    d.save(&dir).unwrap();
    let back = Document::load(&dir).unwrap();
    assert_eq!(back.get_pixel(0, 0, 0, 0).unwrap(), [10, 0, 0, 255]);
    assert_eq!(back.get_pixel(0, 1, 3, 3).unwrap(), [0, 0, 10, 255]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn structural_ops_keep_cel_and_tag_invariants() {
    // Deterministic mini-fuzzer: random layer/frame lifecycle ops (seeded by
    // raster::hash2, no new deps), asserting the meta↔cel lock-step after
    // EVERY step — the invariant the remap choke points exist to keep.
    for seed in 0..40u64 {
        let mut d = Document::new("t", 4, 4);
        for step in 0..120i32 {
            let h = crate::raster::hash2(step, seed as i32, seed);
            let layers = d.meta().layers.len();
            let frames = d.meta().frames.len();
            match h % 8 {
                0 => {
                    let _ = d.insert_layer(
                        (h >> 8) as usize % (layers + 1),
                        None,
                        255,
                        "normal".into(),
                    );
                }
                1 => {
                    let _ = d.delete_layer((h >> 8) as usize % (layers + 1));
                }
                2 => {
                    let _ = d.move_layer((h >> 8) as usize % layers, (h >> 16) as usize % layers);
                }
                3 => {
                    let _ = d.duplicate_layer((h >> 8) as usize % layers);
                }
                4 => {
                    let _ = d.frame_ops("insert", (h >> 8) as usize % (frames + 1), None, None);
                }
                5 => {
                    let _ = d.frame_ops("delete", (h >> 8) as usize % (frames + 1), None, None);
                }
                6 => {
                    let _ = d.frame_ops(
                        "move",
                        (h >> 8) as usize % frames,
                        Some((h >> 16) as usize % frames),
                        None,
                    );
                }
                _ => {
                    let _ = d.add_tag("t", 0, frames - 1, "forward");
                    d.rect(
                        0,
                        0,
                        (h >> 8) as i32 % 4,
                        (h >> 16) as i32 % 4,
                        (h >> 8) as i32 % 4,
                        (h >> 16) as i32 % 4,
                        [1, 2, 3, 255],
                        true,
                        1,
                    )
                    .unwrap();
                }
            }
            // The lock-step invariant, after every single op.
            for (l, f) in d.cel_keys() {
                assert!(
                    l < d.meta().layers.len() && f < d.meta().frames.len(),
                    "seed {seed} step {step}: cel ({l},{f}) escaped the structure"
                );
            }
            for t in &d.meta().tags {
                assert!(
                    t.from <= t.to && t.to < d.meta().frames.len(),
                    "seed {seed} step {step}: tag {:?} out of bounds",
                    t.name
                );
            }
        }
        // And the whole history flattens + round-trips without a panic.
        for f in 0..d.meta().frames.len() {
            let _ = d.flatten(f);
        }
        let dir = std::env::temp_dir().join(format!("atelier-fuzz-{seed}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        d.save(&dir).unwrap();
        let back = Document::load(&dir).unwrap();
        assert_eq!(
            back.meta().layers.len(),
            d.meta().layers.len(),
            "seed {seed}: layer count lost in round-trip"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
