use super::super::FrameAction;
use super::*;
use ab_glyph::{Font, FontArc, point};
use ttf_parser::{Face, GlyphId, OutlineBuilder, Tag};

fn fixture() -> Document {
    let mut doc = Document::new("pixel atlas", 8, 4);
    doc.pencil(0, 0, &[(0, 0)], [12, 34, 56, 255], 1).unwrap();
    doc.rect(0, 0, 2, 0, 4, 2, [255, 255, 255, 255], false, 1)
        .unwrap();
    doc.pencil(0, 0, &[(5, 0), (6, 1)], [80, 100, 120, 255], 1)
        .unwrap();
    let meta: FontMeta = serde_json::from_value(json!({
        "family":"Atelier Pixel", "baseline":2, "ascent":2, "descent":1, "line_gap":1,
        "missing_glyph":2, "space_glyph":1,
        "glyphs":[
            {"name":"ring", "codepoints":[79,111,128512], "rect":[2,0,3,3], "advance":4, "bearing_x":-1},
            {"name":"space", "codepoints":[32,160], "rect":null, "advance":2},
            {"name":"missing", "codepoints":[], "rect":[0,0,1,1], "advance":2},
            {"name":"diagonal", "codepoints":[88], "rect":[5,0,2,2], "advance":3}
        ]
    })).unwrap();
    doc.set_font(Some(meta)).unwrap();
    doc
}

#[derive(Default)]
struct GridOutline {
    points: Vec<(f32, f32)>,
    contours: usize,
}
impl OutlineBuilder for GridOutline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.points.push((x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.points.push((x, y));
    }
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {
        panic!("pixel font contains a curve");
    }
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
        panic!("pixel font contains a curve");
    }
    fn close(&mut self) {
        self.contours += 1;
    }
}

#[test]
fn font_tables_mappings_metrics_and_grid_are_readable_by_an_independent_parser() {
    let doc = fixture();
    let bytes = doc.font_bytes().unwrap();
    assert_eq!(
        bytes,
        doc.font_bytes().unwrap(),
        "font bytes must be deterministic"
    );
    let face = Face::parse(&bytes, 0).unwrap();
    assert_eq!(face.number_of_glyphs(), 4);
    assert_eq!(face.units_per_em(), 1024);
    assert_eq!(face.ascender(), 128);
    assert_eq!(face.descender(), -64);
    assert_eq!(face.line_gap(), 64);
    let ring = face.glyph_index('O').unwrap();
    assert_eq!(ring, GlyphId(1)); // Missing glyph at source index 2 becomes GID 0.
    assert_eq!(face.glyph_index('o'), Some(ring));
    assert_eq!(face.glyph_index('😀'), Some(ring));
    assert_eq!(face.glyph_index('Z'), None);
    assert_eq!(face.glyph_hor_advance(ring), Some(256));
    assert_eq!(face.glyph_hor_side_bearing(ring), Some(-64));
    let space = face.glyph_index(' ').unwrap();
    assert_eq!(face.glyph_index('\u{a0}'), Some(space));
    assert_eq!(face.glyph_hor_advance(space), Some(128));
    assert_eq!(face.glyph_bounding_box(space), None);
    assert!(face.glyph_bounding_box(GlyphId(0)).is_some());
    let mut outline = GridOutline::default();
    let bbox = face.outline_glyph(ring, &mut outline).unwrap();
    assert_eq!(
        (bbox.x_min, bbox.y_min, bbox.x_max, bbox.y_max),
        (-64, -64, 128, 128)
    );
    assert_eq!(outline.contours, 4);
    assert!(
        outline
            .points
            .iter()
            .all(|(x, y)| x % 64.0 == 0.0 && y % 64.0 == 0.0)
    );
    let raw = face.raw_face();
    for tag in [
        b"head", b"hhea", b"maxp", b"OS/2", b"hmtx", b"cmap", b"loca", b"glyf", b"name", b"post",
        b"gasp",
    ] {
        assert!(raw.table(Tag::from_bytes(tag)).is_some(), "missing {tag:?}");
    }
    for tag in [b"fpgm", b"prep", b"cvt ", b"fvar", b"gvar"] {
        assert!(raw.table(Tag::from_bytes(tag)).is_none());
    }
    assert_eq!(
        raw.table(Tag::from_bytes(b"gasp")).unwrap(),
        [0, 1, 0, 1, 255, 255, 0, 0]
    );
    let mut padded = bytes.clone();
    padded.resize(bytes.len().next_multiple_of(4), 0);
    let checksum = padded.as_chunks::<4>().0.iter().fold(0u32, |sum, bytes| {
        sum.wrapping_add(u32::from_be_bytes(*bytes))
    });
    assert_eq!(checksum, 0xB1B0AFBA);
}

#[test]
fn font_rasterizer_preserves_holes_and_diagonal_contacts_at_pixel_size() {
    let font = FontArc::try_from_vec(fixture().font_bytes().unwrap()).unwrap();
    for (ch, pen_x, expected) in [
        ('O', 1.0, vec![vec![1, 1, 1], vec![1, 0, 1], vec![1, 1, 1]]),
        ('X', 0.0, vec![vec![1, 0], vec![0, 1]]),
    ] {
        // ab_glyph scales by ascent-descent (three source rows in this fixture).
        let glyph = font
            .glyph_id(ch)
            .with_scale_and_position(3.0, point(pen_x, 2.0));
        let outlined = font.outline_glyph(glyph).unwrap();
        let bounds = outlined.px_bounds();
        let mut coverage = vec![vec![0.0; expected[0].len()]; expected.len()];
        outlined.draw(|x, y, c| {
            let x = (x as f32 + bounds.min.x) as usize;
            let y = (y as f32 + bounds.min.y) as usize;
            if y < coverage.len() && x < coverage[y].len() {
                coverage[y][x] = c;
            } else {
                assert!(c < 0.001, "unexpected ink outside glyph grid");
            }
        });
        for (actual, expected) in coverage.iter().flatten().zip(expected.iter().flatten()) {
            assert!(
                (actual - *expected as f32).abs() < 0.001,
                "{ch}: {coverage:?}"
            );
        }
    }
}

#[test]
fn font_metadata_round_trips_and_rejects_invalid_persisted_mappings() {
    let dir = std::env::temp_dir().join(format!("atelier_font_roundtrip_{}", std::process::id()));
    let mut doc = fixture();
    doc.save(&dir).unwrap();
    let loaded = Document::load(&dir).unwrap();
    assert_eq!(loaded.meta().font, doc.meta().font);
    assert_eq!(loaded.font_bytes().unwrap(), doc.font_bytes().unwrap());
    let mut metadata: Value =
        serde_json::from_slice(&std::fs::read(dir.join("doc.json")).unwrap()).unwrap();
    metadata["font"]["glyphs"][0]["codepoints"] = json!([0xd800]);
    std::fs::write(dir.join("doc.json"), serde_json::to_vec(&metadata).unwrap()).unwrap();
    assert!(
        Document::load_metadata(&dir)
            .err()
            .unwrap()
            .contains("Unicode scalar")
    );
    metadata.as_object_mut().unwrap().remove("font");
    std::fs::write(dir.join("doc.json"), serde_json::to_vec(&metadata).unwrap()).unwrap();
    assert!(
        Document::load(&dir).unwrap().meta().font.is_none(),
        "font metadata is optional"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn invalid_font_metadata_is_rejected_before_mutating_the_document() {
    let mut doc = fixture();
    let valid = doc.meta().font.clone().unwrap();
    for (field, value) in [
        ("family", json!("")),
        ("frame", json!(1)),
        ("baseline", json!(3)),
        ("ascent", json!(0)),
        ("descent", json!(u32::MAX)),
        ("line_gap", json!(u32::MAX)),
        ("units_per_em", json!(15)),
        ("units_per_pixel", json!(0)),
        ("missing_glyph", json!(1)),
        ("space_glyph", json!(0)),
    ] {
        let mut changed = serde_json::to_value(&valid).unwrap();
        changed[field] = value;
        let invalid: FontMeta = serde_json::from_value(changed).unwrap();
        assert!(
            doc.set_font(Some(invalid)).is_err(),
            "accepted invalid {field}"
        );
        assert_eq!(doc.meta().font.as_ref(), Some(&valid));
    }
    for (field, value) in [
        ("rect", json!([u32::MAX, 0, 2, 2])),
        ("rect", json!([0, 0, 0, 1])),
        ("rect", json!([0, 0, 1, 4])),
        ("advance", json!(u32::MAX)),
        ("bearing_x", json!(i32::MIN)),
        ("codepoints", json!([0x110000])),
        ("codepoints", json!([0xd800])),
        ("codepoints", json!([32])),
        ("codepoints", json!([79, 79])),
        ("name", json!("space")),
    ] {
        let mut changed = serde_json::to_value(&valid).unwrap();
        changed["glyphs"][0][field] = value;
        let invalid: FontMeta = serde_json::from_value(changed).unwrap();
        assert!(
            doc.set_font(Some(invalid)).is_err(),
            "accepted invalid glyph {field}"
        );
        assert_eq!(doc.meta().font.as_ref(), Some(&valid));
    }
}

#[test]
fn font_export_rejects_partial_alpha_and_empty_missing_glyph_without_overwriting() {
    let out = std::env::temp_dir().join(format!("atelier_font_reject_{}.ttf", std::process::id()));
    std::fs::write(&out, b"existing font").unwrap();
    let mut doc = fixture();
    // Layer opacity makes otherwise solid source pixels partially transparent.
    doc.meta.layers[0].opacity = 128;
    assert!(doc.export_font(&out).unwrap_err().contains("partial alpha"));
    assert_eq!(std::fs::read(&out).unwrap(), b"existing font");
    doc.meta.layers[0].opacity = 255;
    doc.clear_cel(0, 0).unwrap();
    assert!(
        doc.export_font(&out)
            .unwrap_err()
            .contains("must contain solid pixels")
    );
    assert_eq!(std::fs::read(&out).unwrap(), b"existing font");
    std::fs::remove_file(out).unwrap();
}

#[test]
fn font_metadata_and_outline_budgets_reject_excess_work() {
    let mut doc = fixture();
    let mut font = doc.meta().font.clone().unwrap();
    font.glyphs[0].codepoints = (1000..1000 + MAX_FONT_MAPPINGS as u32 + 1).collect();
    assert!(
        doc.set_font(Some(font))
            .unwrap_err()
            .contains("Unicode mappings")
    );

    let mut font = doc.meta().font.clone().unwrap();
    while font.glyphs.len() <= MAX_FONT_GLYPHS {
        font.glyphs.push(FontGlyph {
            name: format!("extra-{}", font.glyphs.len()),
            codepoints: Vec::new(),
            rect: None,
            advance: 0,
            bearing_x: 0,
        });
    }
    assert!(doc.set_font(Some(font)).unwrap_err().contains("glyphs"));

    let mut big_doc = Document::new("budget", 4096, 4096);
    let mut font = doc.meta().font.clone().unwrap();
    font.units_per_pixel = 1;
    font.baseline = 4096;
    font.ascent = 4096;
    font.descent = 0;
    font.glyphs[0].rect = Some([0, 0, 4096, 4096]);
    // The full atlas plus the other rectangles exceeds the scan budget. No
    // image allocation is needed to reject this metadata.
    assert!(
        big_doc
            .set_font(Some(font))
            .unwrap_err()
            .contains("source pixels")
    );

    let mut checker = Document::new("point budget", 258, 128);
    let points: Vec<_> = (0..128)
        .flat_map(|y| {
            (0..258)
                .filter(move |x| (x + y) % 2 == 0)
                .map(move |x| (x, y))
        })
        .collect();
    checker
        .pencil(0, 0, &points, [255, 255, 255, 255], 1)
        .unwrap();
    let mut font = doc.meta().font.clone().unwrap();
    font.units_per_pixel = 1;
    font.baseline = 128;
    font.ascent = 128;
    font.descent = 0;
    font.glyphs[0].rect = Some([0, 0, 258, 128]);
    checker.set_font(Some(font)).unwrap();
    assert!(
        checker
            .font_bytes()
            .unwrap_err()
            .contains("TrueType point limit")
    );

    let mut font = checker.meta().font.clone().unwrap();
    font.glyphs[0].rect = Some([0, 0, 128, 32]);
    while font.glyphs.len() < 132 {
        let mut glyph = font.glyphs[0].clone();
        glyph.name = format!("many-{}", font.glyphs.len());
        glyph.codepoints.clear();
        font.glyphs.push(glyph);
    }
    checker.set_font(Some(font)).unwrap();
    assert!(checker.font_bytes().unwrap_err().contains("outline points"));
}

#[test]
fn font_atlas_frame_follows_timeline_edits_and_cannot_be_deleted() {
    let mut doc = fixture();
    let original = doc.font_bytes().unwrap();
    doc.frame_ops(FrameAction::Insert, 0, None, None).unwrap();
    assert_eq!(doc.meta().font.as_ref().unwrap().frame, 1);
    assert_eq!(doc.font_bytes().unwrap(), original);
    doc.frame_ops(FrameAction::Duplicate, 0, None, None)
        .unwrap();
    assert_eq!(doc.meta().font.as_ref().unwrap().frame, 2);
    doc.frame_ops(FrameAction::Move, 2, Some(0), None).unwrap();
    assert_eq!(doc.meta().font.as_ref().unwrap().frame, 0);
    assert!(doc.frame_ops(FrameAction::Delete, 0, None, None).is_err());
    assert_eq!(doc.font_bytes().unwrap(), original);
    doc.frame_ops(FrameAction::Move, 0, Some(2), None).unwrap();
    doc.frame_ops(FrameAction::Delete, 0, None, None).unwrap();
    assert_eq!(doc.meta().font.as_ref().unwrap().frame, 1);
    assert_eq!(doc.font_bytes().unwrap(), original);
}
