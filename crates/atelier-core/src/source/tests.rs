use super::*;
use crate::{document::Document, raster::Blend};
use image::{Rgba, RgbaImage};
use serde_json::json;
use std::collections::BTreeMap;

fn asset() -> Asset {
    serde_json::from_value(
        json!({"format":1,"renderer":1,"name":"source test","canvas":[8,8],
        "frames":[80,120],"inks":{"brass":"#ffcc00","flame":"#ffcc00"},
        "layers":[{"id":"art","name":"Art"}],
        "parts":{"tip":{"size":[4,4],"origin":[1,1],"grid":"x.\n.x\n","legend":{"x":"flame"}},
                 "poses":{"size":[8,8],"items":[{"part":"tip","at":[1,0],"flip_x":true}]}},
        "cels":[{"layer":"art","frames":[0,1],"part":"poses"}]}),
    )
    .unwrap()
}

#[test]
fn logical_bounds_control_transforms_and_replacing_dots_erases() {
    let mut a = asset();
    let before = compile(&a, &BTreeMap::new()).unwrap();
    assert_eq!(before.get_pixel(0, 0, 3, 1).unwrap(), [255, 204, 0, 255]);
    assert_eq!(before.get_pixel(0, 0, 2, 2).unwrap(), [255, 204, 0, 255]);
    a.parts.get_mut("tip").unwrap().grid = Some("..\n.x\n".into());
    let after = compile(&a, &BTreeMap::new()).unwrap();
    assert_eq!(after.get_pixel(0, 0, 3, 1).unwrap(), [0, 0, 0, 0]);
    assert_eq!(after.get_pixel(0, 1, 2, 2).unwrap(), [255, 204, 0, 255]);
    assert_eq!(after.meta().frames[1].duration_ms, 120);
}

#[test]
fn equal_named_inks_stay_independent_and_bindings_are_local() {
    let mut a = asset();
    a.parts.get_mut("poses").unwrap().items.as_mut().unwrap()[0]
        .bindings
        .insert("flame".into(), "brass".into());
    a.inks.insert("flame".into(), "#ff0000".into());
    let doc = compile(&a, &BTreeMap::new()).unwrap();
    assert_eq!(doc.get_pixel(0, 0, 3, 1).unwrap(), [255, 204, 0, 255]);
    a.parts.get_mut("poses").unwrap().items.as_mut().unwrap()[0]
        .bindings
        .clear();
    assert_eq!(
        compile(&a, &BTreeMap::new())
            .unwrap()
            .get_pixel(0, 0, 3, 1)
            .unwrap(),
        [255, 0, 0, 255]
    );
}

#[test]
fn rgba_including_hidden_rgb_survives_images_and_replace() {
    let mut a = asset();
    a.parts.insert(
        "tip".into(),
        Part {
            size: [4, 4],
            origin: [1, 1],
            image: Some("tip.png".into()),
            ..Part::default()
        },
    );
    let img = RgbaImage::from_pixel(1, 1, Rgba([12, 34, 56, 0]));
    let doc = compile(&a, &BTreeMap::from([("tip.png".into(), img)])).unwrap();
    assert_eq!(doc.get_pixel(0, 0, 3, 1).unwrap(), [12, 34, 56, 0]);
}

#[test]
fn cycles_expansion_and_invalid_targets_fail_before_rendering() {
    let mut a = asset();
    a.parts.get_mut("poses").unwrap().items = Some(vec![Instance::new("poses")]);
    assert!(a.validate().unwrap_err().contains("cyclic"));
    a = asset();
    a.cels.push(a.cels[0].clone());
    assert!(a.validate().unwrap_err().contains("duplicate"));
    for at in [[i32::MAX, 0], [0, i32::MIN]] {
        let mut invalid = asset();
        invalid.cels[0].at = at;
        assert!(invalid.validate().unwrap_err().contains("coordinate range"));
    }
    a = asset();
    a.parts.get_mut("tip").unwrap().image = Some("also.png".into());
    assert!(a.validate().unwrap_err().contains("exactly one"));
    a = asset();
    for n in 0..20 {
        let previous = if n == 0 {
            "tip".into()
        } else {
            format!("node{}", n - 1)
        };
        a.parts.insert(
            format!("node{n}"),
            Part {
                size: [4, 4],
                items: Some(vec![Instance::new(&previous), Instance::new(&previous)]),
                ..Part::default()
            },
        );
    }
    assert!(a.validate().unwrap_err().contains("budget"));
}

#[test]
fn source_over_is_explicit_and_extreme_offsets_are_clipped() {
    let mut a = asset();
    let i = &mut a.parts.get_mut("poses").unwrap().items.as_mut().unwrap()[0];
    i.at = [i32::MAX, i32::MIN];
    i.scale = 16;
    i.turns = 3;
    assert!(
        compile(&a, &BTreeMap::new())
            .unwrap()
            .cel_full(0, 0)
            .pixels()
            .all(|p| p.0 == [0; 4])
    );
    a = asset();
    let i = &mut a.parts.get_mut("poses").unwrap().items.as_mut().unwrap()[0];
    i.opacity = 128;
    assert!(a.validate().unwrap_err().contains("mode"));
    let i = &mut a.parts.get_mut("poses").unwrap().items.as_mut().unwrap()[0];
    i.mode = Placement::Over;
    i.blend = Blend::Normal;
    assert_eq!(
        compile(&a, &BTreeMap::new())
            .unwrap()
            .get_pixel(0, 0, 3, 1)
            .unwrap()[3],
        128
    );
}

#[test]
fn procedures_use_the_existing_renderer_and_palette() {
    let mut a = asset();
    let op = json!({"op":"rect","x0":0,"y0":0,"x1":2,"y1":2,"color":[8,9,10,128],"fill":true});
    a.parts.insert(
        "poses".into(),
        Part {
            size: [8, 8],
            draw: Some(vec![op.clone()]),
            ..Part::default()
        },
    );
    let mut expected = Document::new("expected", 8, 8);
    expected.apply_op(0, 0, &op).unwrap();
    assert_eq!(
        compile(&a, &BTreeMap::new()).unwrap().cel_full(0, 0),
        expected.cel_full(0, 0)
    );
}
