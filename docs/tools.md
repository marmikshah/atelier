# atelier tool reference

**25** tools — every one advertised, no profiles to pick.

Generated from the live registry by `atelier tools --markdown`; regenerate with `make docs`. Do not edit by hand.

## `delete_doc`

Delete a document and all its files.

Parameters: `doc_id`, `expected_revision`

## `doc_add_tag`

Add a named animation frame range.

Parameters: `direction`, `doc_id`, `expected_revision`, `from`, `name`, `to`

## `doc_anim_audit`

Audit animation loops, motion spacing, arcs, or timing.

Parameters: `doc_id`, `layer`, `mode`, `region`, `tag`

## `doc_checkpoint`

Save, list, restore, or prune document checkpoints.

Parameters: `action`, `checkpoint_id`, `doc_id`, `expected_revision`, `label`

## `doc_components`

Measure up to 64 connected components and small specks.

Parameters: `color`, `connectivity`, `doc_id`, `frame`, `layer`, `min_area`

## `doc_contact_sheet`

Render all frames as a labelled inline contact sheet.

Parameters: `cols`, `doc_id`, `onion`, `scale`

## `doc_critique`

Report common contour, lighting, value, and palette problems.

Parameters: `doc_id`, `frame`, `layer`, `region`

## `doc_dither_ramp`

Dither a colour ramp horizontally, vertically, or radially.

Parameters: `axis`, `doc_id`, `expected_revision`, `frame`, `layer`, `only_existing`, `pattern`, `ramp`, `region`

## `doc_draw`

Apply one typed drawing operation across frame cels.

Parameters: `aa`, `blend`, `blend_mode`, `closed`, `color`, `colorize`, `colors`, `cx`, `cy`, `density`, `dither`, `doc_id`, `erase`, `expected_revision`, `fill`, `frame`, `frame_to`, `kind`, `layer`, `octaves`, `op`, `opacity`, `points`, `region`, `rx`, `ry`, `scale`, `seed`, `size`, `snap`, `stops`, `text`, `tip`, `tolerance`, `width`, `x`, `x0`, `x1`, `y`, `y0`, `y1`

## `doc_dump_region`

Return up to 4096 exact pixels as a symbol or hexadecimal grid.

Parameters: `doc_id`, `frame`, `layer`, `mode`, `region`

## `doc_export`

Export a spritesheet with metadata or a GIF/APNG animation.

Parameters: `doc_id`, `format`, `meta`, `op`, `out_path`, `scale`, `tag`

## `doc_frame`

Add, edit, reorder, duplicate, or delete animation frames.

Parameters: `copy_from`, `count`, `doc_id`, `duration_ms`, `expected_revision`, `frame`, `op`, `to_index`

## `doc_frame_diff`

Compare two frames or cels, with optional grid or overlay.

Parameters: `doc_id`, `frame_a`, `frame_b`, `grid`, `layer`, `out_path`, `region`, `render`, `scale`

## `doc_fx`

Apply one typed effect, transform, or colour operation across frame cels.

Parameters: `aa`, `blend_mode`, `blur`, `color`, `color_a`, `color_b`, `colors`, `dark`, `density`, `depth`, `doc_id`, `dx`, `dy`, `erase`, `expected_revision`, `form`, `frame`, `frame_to`, `from`, `h`, `horizontal`, `hue`, `keep_left`, `keep_top`, `layer`, `light`, `light_dir`, `lum`, `max_colors`, `method`, `mode`, `only_existing`, `op`, `opacity`, `pattern`, `radius`, `ramp`, `region`, `sat`, `shadow_opacity`, `snap`, `steps`, `stops`, `strength`, `to`, `tolerance`, `turns`, `vertical`, `w`, `wrap`

## `doc_info`

Get document structure: layers, frames, cels, and tags.

Parameters: `doc_id`

## `doc_layer`

Add, edit, reorder, duplicate, merge, or delete a layer.

Parameters: `blend`, `doc_id`, `expected_revision`, `index`, `name`, `op`, `opacity`, `to_index`, `visible`

## `doc_look`

Render and measure a frame using configurable analysis views.

Parameters: `bands`, `bg`, `coords`, `doc_id`, `frame`, `grid`, `max_size`, `mode`, `onion`, `out_path`, `region`, `scale`, `tile`

## `doc_new`

Create a persisted layered animation document and return its opaque `doc_id`.

Parameters: `height`, `name`, `width`

## `doc_paint_grid`

Paint a region from character rows and a colour legend.

Parameters: `doc_id`, `expected_revision`, `frame`, `layer`, `legend`, `rows`, `x`, `y`

## `doc_palette`

Generate, set, inspect, snap, or swap document palettes.

Parameters: `alpha`, `anchor_midtone`, `base`, `bg`, `colors`, `count`, `cutoff`, `doc_id`, `dupe_threshold`, `expected_revision`, `frame`, `from`, `hue_shift`, `layer`, `op`, `palette`, `region`, `sat_curve`, `scheme`, `set_doc`, `to`, `value_hi`, `value_lo`

## `doc_ref`

Set, analyze, compare, or diff a document reference image.

Parameters: `cells`, `colors`, `doc_id`, `expected_revision`, `frame`, `mode`, `op`, `path`, `target_w`, `top`

## `doc_region`

Clear or move a rectangular cel region.

Parameters: `doc_id`, `expected_revision`, `frame`, `layer`, `offset`, `op`, `rect`

## `doc_seam_report`

Measure horizontal or vertical tiling seams and mismatches.

Parameters: `axis`, `doc_id`, `frame`, `layer`, `out_path`, `threshold`

## `doc_silhouette`

Report opaque bounds, fill ratio, and a silhouette grid.

Parameters: `alpha_threshold`, `doc_id`, `frame`, `layer`

## `list_docs`

List up to 100 documents, with filters and cursor pagination.

Parameters: `contains`, `cursor`, `limit`, `prefix`

