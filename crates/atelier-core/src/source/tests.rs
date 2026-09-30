use super::*;
use serde_json::json;
use std::collections::BTreeMap;
fn asset(cel: Cel) -> Asset {
    Asset {
        format: 1,
        renderer: 1,
        name: "sprite".into(),
        canvas: [4, 4],
        frames: vec![100, 120],
        palette: vec![],
        tags: vec![],
        reference: None,
        layers: vec![Layer {
            name: "Body".into(),
            opacity: 255,
            visible: true,
            blend: crate::raster::Blend::Normal,
            cels: vec![cel],
        }],
    }
}
fn pixel() -> Pixels {
    Pixels {
        origin: [2, 1],
        legend: BTreeMap::from([
            ("x".into(), "#01020300".into()),
            ("y".into(), "#ff8800".into()),
        ]),
        data: vec![PixelData::Grid("xy\n.y\n".into())],
    }
}
#[test]
fn pixel_forms_preserve_bounds_offsets_and_hidden_rgb() {
    for data in [
        pixel().data,
        vec![
            PixelData::Row {
                runs: vec![("x".into(), 1), ("y".into(), 1)],
                repeat: 1,
            },
            PixelData::Row {
                runs: vec![(".".into(), 1), ("y".into(), 1)],
                repeat: 1,
            },
        ],
        vec![
            PixelData::Span {
                x: 0,
                y: 0,
                text: "xy".into(),
                rows: 1,
            },
            PixelData::Span {
                x: 1,
                y: 1,
                text: "y".into(),
                rows: 1,
            },
        ],
    ] {
        let mut p = pixel();
        p.data = data;
        let a = asset(Cel {
            frames: vec![0, 1],
            size: Some([6, 5]),
            at: [-2, 3],
            steps: vec![Step::Pixels(p)],
        });
        let d = compile(&a).unwrap();
        let (x, y, p) = d.cel(0, 0).unwrap();
        assert_eq!((x, y, p.dimensions()), (-2, 3, (6, 5)));
        assert_eq!(p.get_pixel(2, 1).0, [1, 2, 3, 0]);
        assert_eq!(p.get_pixel(3, 2).0, [255, 136, 0, 255]);
        assert_eq!(d.cel(0, 0), d.cel(0, 1));
    }
}
#[test]
fn patches_skip_holes_and_explicitly_erase_over_procedural_art() {
    let op = json!({"op":"fill_cel","color":[20,30,40]});
    let p = Pixels {
        origin: [0, 0],
        legend: BTreeMap::from([
            ("c".into(), "#00000000".into()),
            ("h".into(), "#01020300".into()),
        ]),
        data: vec![PixelData::Grid("c.h\n".into())],
    };
    let a = asset(Cel {
        frames: vec![0],
        steps: vec![
            Step::Draw {
                op,
                palette: vec![],
            },
            Step::Pixels(p),
        ],
        ..Cel::default()
    });
    let d = compile(&a).unwrap();
    let p = d.cel(0, 0).unwrap().2;
    assert_eq!(p.get_pixel(0, 0).0, [0; 4]);
    assert_eq!(p.get_pixel(1, 0).0, [20, 30, 40, 255]);
    assert_eq!(p.get_pixel(2, 0).0, [1, 2, 3, 0]);
}
#[test]
fn procedures_reuse_the_renderer_with_full_seed_range() {
    let op = json!({"op":"noise","x0":0,"y0":0,"x1":3,"y1":3,"seed":u64::MAX,"stops":[{"pos":0,"color":[1,2,3]},{"pos":1,"color":[211,171,91]}]});
    let mut direct = crate::document::Document::new("sprite", 4, 4);
    direct.apply_op(0, 0, &op).unwrap();
    let a = asset(Cel {
        frames: vec![0],
        steps: vec![Step::Draw {
            op,
            palette: vec![],
        }],
        ..Cel::default()
    });
    assert_eq!(compile(&a).unwrap().cel_full(0, 0), direct.cel_full(0, 0));
}
#[test]
fn counts_coordinates_symbols_frames_and_work_are_checked_before_allocation() {
    let invalid = [
        PixelData::Span {
            x: u32::MAX,
            y: 0,
            text: "x".into(),
            rows: 1,
        },
        PixelData::Span {
            x: 0,
            y: 0,
            text: "x".into(),
            rows: u32::MAX,
        },
        PixelData::Span {
            x: 0,
            y: 0,
            text: ".".into(),
            rows: 1,
        },
        PixelData::Row {
            runs: vec![("x".into(), u32::MAX)],
            repeat: 1,
        },
        PixelData::Row {
            runs: vec![("x".into(), 0)],
            repeat: 1,
        },
        PixelData::Grid("z".into()),
    ];
    for data in invalid {
        let mut p = pixel();
        p.data = vec![data];
        assert!(
            asset(Cel {
                frames: vec![0],
                steps: vec![Step::Pixels(p)],
                ..Cel::default()
            })
            .validate()
            .is_err()
        );
    }
    for frames in [vec![], vec![0, 0], vec![2]] {
        assert!(
            asset(Cel {
                frames,
                ..Cel::default()
            })
            .validate()
            .is_err()
        );
    }
    let mut a = asset(Cel {
        frames: vec![0],
        steps: vec![
            Step::Draw {
                op: json!({"op":"fill_cel","color":[1,2,3]}),
                palette: vec![]
            };
            33
        ],
        ..Cel::default()
    });
    a.canvas = [4096, 4096];
    assert!(a.validate().unwrap_err().contains("budget"));
}
