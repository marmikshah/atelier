// --- prompts ---------------------------------------------------------------

use rmcp::model::{Prompt, PromptArgument};
use serde_json::Value;

/// Looks up an argument value by name (None when absent — the builder defaults).
pub(crate) type ArgLookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// One packaged workflow: its name, what it does, the args it takes (name +
/// whether required), and a builder that fills the template from those args.
pub(crate) struct PromptSpec {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    /// (arg name, description, required) — advertised verbatim as the schema.
    pub(crate) args: &'static [(&'static str, &'static str, bool)],
    /// Tool names the rendered text names verbatim; the drift test asserts each
    /// appears in the text AND in the live tool list.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) tools: &'static [&'static str],
    /// Render the prompt text from the looked-up argument values.
    pub(crate) build: fn(&ArgLookup) -> String,
}

/// Look up an argument from the request's object, defaulting when absent so an
/// optional arg still substitutes cleanly into the template.
fn arg<'a>(args: &'a Option<serde_json::Map<String, Value>>, key: &str) -> Option<&'a str> {
    args.as_ref()?.get(key)?.as_str()
}

/// The three shipped workflows. Each `build` emits ordered, numbered steps with
/// tool names written verbatim — the get_prompt drift test checks every name
/// here against the live tool list.
pub(crate) const PROMPTS: &[PromptSpec] = &[
    PromptSpec {
        name: "pixel-sprite",
        description:
            "Draw a single pixel-art sprite the right way: lock a ramp, paint, LOOK, audit.",
        args: &[
            ("subject", "What to draw, e.g. \"a knight\".", true),
            ("size", "Canvas size in pixels (default 32).", false),
            (
                "palette_hint",
                "Colour direction, e.g. \"cool steel\".",
                false,
            ),
        ],
        tools: &[
            "doc_create",
            "doc_palette",
            "doc_draw",
            "doc_batch",
            "doc_look",
            "doc_fx",
            "doc_critique",
            "doc_silhouette",
            "doc_components",
            "doc_export",
        ],
        build: |g| {
            let subject = g("subject").unwrap_or_else(|| "a sprite".into());
            let size = g("size").unwrap_or_else(|| "32".into());
            let palette = g("palette_hint").unwrap_or_else(|| "your choice".into());
            format!(
                "Draw a pixel-art sprite of {subject} on a {size}x{size} canvas (palette: {palette}).\n\
                 1. doc_create a {size}x{size} document.\n\
                 2. doc_palette (scheme=mono) a base colour into shades, then doc_palette op=set to LOCK the ramp.\n\
                 3. Block the silhouette with doc_draw (op=rect/ellipse/polygon).\n\
                 4. Paint detail with doc_batch (many ops in one call) or doc_draw (op=pencil/line).\n\
                 5. doc_look after every burst — it returns the frame INLINE; study it before continuing.\n\
                 6. Shade with doc_fx op=shade and clean strokes with doc_fx op=pixel_perfect.\n\
                 7. Audit shape: doc_silhouette (readable bbox/fill) and doc_components (no stray specks).\n\
                 8. Audit colour: doc_palette op=report (every colour in_palette, no near-dupes).\n\
                 9. doc_critique for the failure modes you can't see; fix what it flags, doc_look to confirm.\n\
                 10. doc_export op=sheet the finished sprite to a PNG.\n\
                 Iterate 3-9 until the sprite reads cleanly at 1x."
            )
        },
    },
    PromptSpec {
        name: "walk-cycle",
        description:
            "Animate a 4-pose walk cycle: reuse frames, change only what moves, verify spacing.",
        args: &[
            ("character", "Who walks, e.g. \"the knight\".", true),
            (
                "frames",
                "Total frames (default 4: contact/down/passing/up).",
                false,
            ),
        ],
        tools: &[
            "doc_frame",
            "doc_draw",
            "doc_region",
            "doc_keyframe_move",
            "doc_look",
            "doc_contact_sheet",
            "doc_frame_diff",
            "doc_anim_audit",
            "doc_add_tag",
            "doc_export",
        ],
        build: |g| {
            let character = g("character").unwrap_or_else(|| "the character".into());
            let frames = g("frames").unwrap_or_else(|| "4".into());
            format!(
                "Animate a {frames}-frame walk cycle for {character}.\n\
                 1. Start from a finished standing pose on frame 0 (use the pixel-sprite flow first).\n\
                 2. The cycle is contact -> down -> passing -> up. Plan numbers FIRST: stride ~1/3 of \
                 the character's height in px, body bobs 1px DOWN on contact and UP on passing, arms \
                 counter-swing the legs. NEVER doc_dissolve poses — it cross-fades (ghost frames), it does \
                 not move limbs.\n\
                 3. doc_frame op=add with copy_from the previous frame so each pose starts from the last.\n\
                 4. Repaint ONLY what changes per pose (legs, arms) with doc_draw (op=pencil) / doc_region op=move; \
                 doc_keyframe_move eases a region across several frames in one call.\n\
                 5. doc_look every frame (onion=true ghosts the neighbours); doc_contact_sheet shows \
                 the whole cycle in one inline grid.\n\
                 6. Verify each adjacent pair with doc_frame_diff (only the limbs should change).\n\
                 7. doc_anim_audit mode=\"spacing\" — the per-frame motion must be even, low drift.\n\
                 8. doc_add_tag the range and doc_anim_audit mode=\"seam\" so the loop wrap is clean.\n\
                 9. doc_frame op=duration ~120ms per frame, with contact poses held ~1.5x longer — \
                 uniform 100ms reads mechanical.\n\
                 10. doc_export op=anim the tagged loop and study it.\n\
                 Iterate 4-9 until the walk reads smoothly."
            )
        },
    },
    PromptSpec {
        name: "seamless-tile",
        description: "Paint a seamless tile: wrap edges, prove the seam is zero, eyeball the grid.",
        args: &[
            ("material", "What to tile, e.g. \"grass\".", true),
            ("size", "Tile size in pixels (default 32).", false),
        ],
        tools: &[
            "doc_create",
            "doc_palette",
            "doc_draw",
            "doc_look",
            "doc_fx",
            "doc_seam_report",
            "doc_export",
        ],
        build: |g| {
            let material = g("material").unwrap_or_else(|| "the material".into());
            let size = g("size").unwrap_or_else(|| "32".into());
            format!(
                "Paint a seamless {size}x{size} {material} tile.\n\
                 1. doc_create a {size}x{size} document and doc_palette op=set to lock the colours.\n\
                 2. Fill the base with doc_draw op=fill_cel, then texture with doc_draw op=noise / op=scatter.\n\
                 3. doc_look to study the raw tile inline.\n\
                 4. doc_fx op=shift wrap=true to roll the seam into the middle, then paint over the join.\n\
                 5. Use doc_fx op=shift wrap=true again to make detail variants without breaking edges.\n\
                 6. doc_seam_report MUST return zero mismatches on both axes — fix until it does.\n\
                 7. doc_look tile=2 and eyeball the 2x2 grid for any visible repeat or seam.\n\
                 8. doc_palette op=report to confirm the texture stayed on-palette.\n\
                 9. Repeat 4-7 until the seam report is clean and the grid looks continuous.\n\
                 10. doc_export op=sheet the tile to a PNG.\n\
                 The tile is done only when doc_seam_report is zero."
            )
        },
    },
    PromptSpec {
        name: "game-asset-set",
        description:
            "Build a coherent game's asset set — hero moveset, terrain, HUD — audited as ONE work.",
        args: &[
            (
                "theme",
                "The game's look, e.g. \"forest ruins, dusk\".",
                true,
            ),
            (
                "hero",
                "The playable character, e.g. \"a cloaked ranger\".",
                true,
            ),
            (
                "size",
                "Character canvas size in pixels (default 48).",
                false,
            ),
        ],
        tools: &[
            "doc_create",
            "doc_palette",
            "doc_draw",
            "doc_figure",
            "doc_pose_cycle",
            "doc_walk",
            "doc_look",
            "doc_autotile_set",
            "doc_tilemap_assemble",
            "doc_nine_slice",
            "doc_set_audit",
            "doc_colorblind_check",
            "doc_export",
        ],
        build: |g| {
            let theme = g("theme").unwrap_or_else(|| "the game's theme".into());
            let hero = g("hero").unwrap_or_else(|| "the hero".into());
            let size = g("size").unwrap_or_else(|| "48".into());
            format!(
                "Build a coherent asset SET for a game: theme {theme}, hero {hero}. \
                 Name every document with one family prefix (e.g. game-hero-idle, game-tile-grass) so the set tools can find them.\n\
                 1. ONE palette first: doc_palette a scheme fitting {theme}; you will lock this exact ramp on every document.\n\
                 2. Hero: doc_create a {size}x{size} doc per animation; doc_palette op=set; pose {hero} once as 13 joints (doc_figure to preview), then doc_walk for the walk and doc_pose_cycle for idle, run, jump, attack, hurt — the same joints every time.\n\
                 3. doc_look each cycle inline; re-run a gait with different intensity/frames until the motion reads.\n\
                 4. Terrain: doc_create a tile-sized doc, layer 0 inner material, layer 1 outer; doc_autotile_set for the 47-tile family, then doc_tilemap_assemble a test mask and doc_look the MAP — terrain is judged assembled, never as lone tiles.\n\
                 5. HUD: doc_create a UI doc; author ONE panel (doc_draw op=panel), then doc_nine_slice it to every dialog/button size.\n\
                 6. Cohesion gate: doc_set_audit on the family prefix — fix every warning; doc_palette op=sync from the hero doc if palettes drifted.\n\
                 7. doc_colorblind_check the HUD and any state-colour art; recolour pairs that collapse.\n\
                 8. Ship: doc_export op=anim per gait tag, doc_export op=tileset for terrain, doc_export op=atlas for the whole set.\n\
                 The set is done only when doc_set_audit says cohesive."
            )
        },
    },
];

/// Build the advertised [`Prompt`] descriptors from [`PROMPTS`].
pub(crate) fn prompt_specs() -> Vec<Prompt> {
    PROMPTS
        .iter()
        .map(|p| {
            let args = p
                .args
                .iter()
                .map(|(name, desc, required)| {
                    PromptArgument::new(*name)
                        .with_description(*desc)
                        .with_required(*required)
                })
                .collect();
            Prompt::new(p.name, Some(p.description), Some(args))
        })
        .collect()
}

/// Render one prompt's filled text + description from request arguments, or None
/// if the name is unknown (the caller maps that to an error).
pub(crate) fn build_prompt(
    name: &str,
    args: &Option<serde_json::Map<String, Value>>,
) -> Option<(String, String)> {
    let spec = PROMPTS.iter().find(|p| p.name == name)?;
    let get = |k: &str| arg(args, k).map(str::to_string);
    Some(((spec.build)(&get), spec.description.to_string()))
}
