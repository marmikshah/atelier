use super::*;
use serde_json::json;
use std::collections::BTreeMap;

fn asset(cel: serde_json::Value) -> Asset {
    serde_json::from_value(json!({"format":1,"renderer":1,"name":"sprite","canvas":[4,4],"frames":[100,120],"layers":[{"name":"Body","cels":[cel]}]})).unwrap()
}
#[test]
fn cropped_grids_preserve_logical_bounds_offsets_and_hidden_rgb() {
    let a = asset(
        json!({"frames":[0,1],"size":[6,5],"at":[-2,3],"origin":[2,1],"grid":"xy\n.y\n","legend":{"x":"#01020300","y":"#ff8800"}}),
    );
    let d = compile(&a, &BTreeMap::new()).unwrap();
    let (x, y, pixels) = d.cel(0, 0).unwrap();
    assert_eq!((x, y, pixels.dimensions()), (-2, 3, (6, 5)));
    assert_eq!(pixels.get_pixel(2, 1).0, [1, 2, 3, 0]);
    assert_eq!(pixels.get_pixel(3, 2).0, [255, 136, 0, 255]);
    assert_eq!(d.cel(0, 0), d.cel(0, 1));
}
#[test]
fn procedures_use_the_existing_renderer_and_full_seed_range() {
    let mut op = json!({"op":"noise","x0":0,"y0":0,"x1":3,"y1":3,"seed":u64::MAX,
        "stops":[{"pos":0,"color":[1,2,3]},{"pos":1,"color":[211,171,91]}]});
    let mut direct = crate::document::Document::new("seed", 4, 4);
    direct.apply_op(0, 0, &op).unwrap();
    op["seed"] = u64::MAX.to_string().into();
    let a = asset(json!({"frames":[0],"draw":[op]}));
    assert_eq!(
        compile(&a, &BTreeMap::new()).unwrap().cel_full(0, 0),
        direct.cel_full(0, 0)
    );
    let op = json!({"op":"rect","x0":1,"y0":0,"x1":2,"y1":3,"color":[240,80,20]});
    let a = asset(json!({"frames":[0],"draw":[op.clone()]}));
    let mut direct = crate::document::Document::new("sprite", 4, 4);
    direct.apply_op(0, 0, &op).unwrap();
    assert_eq!(
        compile(&a, &BTreeMap::new()).unwrap().cel(0, 0),
        direct.cel(0, 0)
    );
}
#[test]
fn recipe_structure_rejects_ambiguous_pixels_bad_frames_and_excess_work() {
    for cel in [
        json!({"frames":[0,0]}),
        json!({"frames":[2]}),
        json!({"frames":[]}),
        json!({"grid":"x\nxx","legend":{"x":"#ffffff"}}),
        json!({"grid":"x","image":"x.png","legend":{"x":"#ffffff"}}),
        json!({"at":[i32::MIN,0]}),
        json!({"at":[i32::MAX,0]}),
    ] {
        assert!(asset(cel).validate().is_err());
    }
    let mut a = asset(json!({"frames":[0]}));
    let duplicate = a.layers[0].cels[0].clone();
    a.layers[0].cels.push(duplicate);
    assert!(a.validate().is_err());
    let mut huge = asset(json!({"frames":[0],"draw":vec![json!({"op":"clear_cel"});33]}));
    huge.canvas = [4096, 4096];
    assert!(huge.validate().unwrap_err().contains("budget"));
}
