//! Assemble TrueType tables with Fontations' validated writer.

use write_fonts::FontBuilder;
use write_fonts::tables::{
    cmap::Cmap,
    gasp::{Gasp, GaspRange, GaspRangeBehavior},
    glyf::{Bbox, GlyfLocaBuilder, Glyph},
    head::{Flags, Head},
    hhea::Hhea,
    hmtx::{Hmtx, LongMetric},
    maxp::Maxp,
    name::{Name, NameRecord},
    os2::{Os2, SelectionFlags},
    post::Post,
};
use write_fonts::types::{FWord, Fixed, GlyphId, NameId, Tag, UfWord, Version16Dot16};

use super::FontMeta;

pub(super) fn encode(
    meta: &FontMeta,
    order: &[usize],
    glyphs: &[Glyph],
) -> Result<Vec<u8>, String> {
    let unit = i32::from(meta.units_per_pixel);
    let ascent = (meta.ascent as i32 * unit) as i16;
    let descent = (meta.descent as i32 * unit) as i16;
    let line_gap = (meta.line_gap as i32 * unit) as i16;
    let count = glyphs.len() as u16;
    let mut glyf = GlyfLocaBuilder::new();
    let mut metrics = Vec::with_capacity(glyphs.len());
    let mut bbox: Option<Bbox> = None;
    let mut max_points = 0;
    let mut max_contours = 0;
    let mut min_rsb = i16::MAX;
    let mut mapping = Vec::new();
    for (gid, (&index, glyph)) in order.iter().zip(glyphs).enumerate() {
        glyf.add_glyph(glyph).map_err(|e| e.to_string())?;
        if let Some(b) = glyph.bbox() {
            bbox = Some(bbox.map_or(b, |old| old.union(b)));
        }
        if let Glyph::Simple(simple) = glyph {
            max_points = max_points.max(simple.contours.iter().map(|c| c.len()).sum::<usize>());
            max_contours = max_contours.max(simple.contours.len());
        }
        let advance = meta.glyphs[index].advance as i32 * unit;
        let glyph_box = glyph.bbox().unwrap_or_default();
        let rsb = i16::try_from(advance - i32::from(glyph_box.x_max)).map_err(|_| {
            format!(
                "font glyph '{}' right side bearing exceeds TrueType range",
                meta.glyphs[index].name
            )
        })?;
        if glyph.bbox().is_some() {
            min_rsb = min_rsb.min(rsb);
        }
        metrics.push(LongMetric::new(advance as u16, glyph_box.x_min));
        mapping.extend(meta.glyphs[index].codepoints.iter().map(|&cp| {
            (
                char::from_u32(cp).expect("validated scalar"),
                GlyphId::new(gid as u32),
            )
        }));
    }
    let bounds = bbox.unwrap_or_default();
    let (glyf, loca, loca_format) = glyf.build();
    let head = Head {
        font_revision: Fixed::ONE,
        flags: Flags::BASELINE_AT_Y_0 | Flags::LSB_AT_X_0,
        units_per_em: meta.units_per_em,
        x_min: bounds.x_min,
        y_min: bounds.y_min,
        x_max: bounds.x_max,
        y_max: bounds.y_max,
        lowest_rec_ppem: (meta.units_per_em / meta.units_per_pixel).max(1),
        index_to_loc_format: loca_format as i16,
        ..Default::default()
    };
    let hhea = Hhea {
        ascender: FWord::new(ascent),
        descender: FWord::new(-descent),
        line_gap: FWord::new(line_gap),
        advance_width_max: UfWord::new(metrics.iter().map(|m| m.advance).max().unwrap_or(0)),
        min_left_side_bearing: FWord::new(
            glyphs
                .iter()
                .filter_map(Glyph::bbox)
                .map(|b| b.x_min)
                .min()
                .unwrap_or(0),
        ),
        min_right_side_bearing: FWord::new(min_rsb),
        x_max_extent: FWord::new(bounds.x_max),
        caret_slope_rise: 1,
        number_of_h_metrics: count,
        ..Default::default()
    };
    let maxp = Maxp {
        num_glyphs: count,
        max_points: Some(max_points as u16),
        max_contours: Some(max_contours as u16),
        max_composite_points: Some(0),
        max_composite_contours: Some(0),
        max_zones: Some(1),
        max_twilight_points: Some(0),
        max_storage: Some(0),
        max_function_defs: Some(0),
        max_instruction_defs: Some(0),
        max_stack_elements: Some(0),
        max_size_of_instructions: Some(0),
        max_component_elements: Some(0),
        max_component_depth: Some(0),
    };
    let mut postscript: String = meta
        .family
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(54)
        .collect();
    if postscript.is_empty() {
        postscript.push_str("AtelierPixel");
    }
    postscript.push_str("-Regular");
    let name = Name::new(
        [
            (1, meta.family.clone()),
            (2, "Regular".into()),
            (3, format!("Atelier:{postscript}:1.0")),
            (4, format!("{} Regular", meta.family)),
            (5, "Version 1.000".into()),
            (6, postscript),
        ]
        .into_iter()
        .map(|(id, value)| NameRecord::new(3, 1, 0x0409, NameId::new(id), value.into()))
        .collect(),
    );
    let nonzero: Vec<u32> = metrics
        .iter()
        .filter(|m| m.advance > 0)
        .map(|m| u32::from(m.advance))
        .collect();
    let average = nonzero.iter().sum::<u32>() / (nonzero.len() as u32).max(1);
    let height = |ch: char| {
        mapping
            .iter()
            .find(|(cp, _)| *cp == ch)
            .and_then(|(_, gid)| glyphs[gid.to_u32() as usize].bbox())
            .map(|bbox| bbox.y_max.max(0))
    };
    let os2 = Os2 {
        x_avg_char_width: average as i16,
        ach_vend_id: Tag::new(b"ATLR"),
        fs_selection: SelectionFlags::REGULAR | SelectionFlags::USE_TYPO_METRICS,
        us_first_char_index: mapping
            .iter()
            .map(|(cp, _)| (*cp as u32).min(65535) as u16)
            .min()
            .unwrap_or(32),
        us_last_char_index: mapping
            .iter()
            .map(|(cp, _)| (*cp as u32).min(65535) as u16)
            .max()
            .unwrap_or(32),
        s_typo_ascender: ascent,
        s_typo_descender: -descent,
        s_typo_line_gap: line_gap,
        us_win_ascent: ascent.max(bounds.y_max) as u16,
        us_win_descent: descent.max(-bounds.y_min) as u16,
        ul_code_page_range_1: Some(0),
        ul_code_page_range_2: Some(0),
        sx_height: Some(height('x').unwrap_or(0)),
        s_cap_height: Some(height('H').or_else(|| height('A')).unwrap_or(ascent)),
        us_default_char: Some(0),
        us_break_char: Some(32),
        us_max_context: Some(1),
        ..Default::default()
    };
    let post = Post {
        version: Version16Dot16::VERSION_3_0,
        underline_position: FWord::new(-(meta.units_per_pixel as i16)),
        underline_thickness: FWord::new(meta.units_per_pixel as i16),
        is_fixed_pitch: u32::from(metrics.iter().all(|m| m.advance == metrics[0].advance)),
        ..Default::default()
    };
    let cmap = Cmap::from_mappings(mapping).map_err(|e| e.to_string())?;
    let hmtx = Hmtx::new(metrics, Vec::new());
    // Request neither grid fitting nor smoothing at any size. Renderers may
    // override gasp; outlines and font data themselves remain unhinted.
    let gasp = Gasp::new(
        1,
        1,
        vec![GaspRange::new(u16::MAX, GaspRangeBehavior::empty())],
    );
    let mut builder = FontBuilder::new();
    builder
        .add_table(&head)
        .map_err(|e| e.to_string())?
        .add_table(&hhea)
        .map_err(|e| e.to_string())?
        .add_table(&maxp)
        .map_err(|e| e.to_string())?
        .add_table(&os2)
        .map_err(|e| e.to_string())?
        .add_table(&hmtx)
        .map_err(|e| e.to_string())?
        .add_table(&cmap)
        .map_err(|e| e.to_string())?
        .add_table(&loca)
        .map_err(|e| e.to_string())?
        .add_table(&glyf)
        .map_err(|e| e.to_string())?
        .add_table(&name)
        .map_err(|e| e.to_string())?
        .add_table(&post)
        .map_err(|e| e.to_string())?
        .add_table(&gasp)
        .map_err(|e| e.to_string())?;
    Ok(builder.build())
}
