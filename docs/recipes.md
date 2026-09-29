# Replay recipes

A recipe describes the current artwork, grouped by layer and frame. Normal CLI
and MCP edits maintain `documents/<id>/recipe/recipe.toml` automatically. This
replaces the append-only JSONL log; there is no separate authoring workflow.

## Why this structure

Metadata calls replace document, layer or frame settings. Pixel and region edits
replace the affected cel's pixels. Drawing and effect calls retain their existing
operation parameters when that representation is smaller and pixel exact;
otherwise the result becomes pixel data. A full overwrite can remove the earlier
construction. Read-only calls and export paths do not enter the recipe.

Small cels use multiline grids. Large or high-colour pixel data uses cropped,
lossless PNG resources, keeping raster bytes out of text and model context.
Identical cels within one layer share a frame list. Editing one frame separates
it automatically. Layers stay named, ordered, and independent.

This is exact reduction of stored artwork, not artistic inference: it does not
guess a generator for painted texture or link unrelated shapes. Retained draw
operations use Atelier's existing renderer. PNG encoding does not quantise
colours; invisible RGB, logical cel dimensions and off-canvas pixels survive.

## A complete recipe

```toml
format = 1
renderer = 1
name = "Lantern flame"
canvas = [7, 7]
frames = [100, 120]

[[layers]]
name = "Flame"

[[layers.cels]]
frames = [0, 1]
origin = [2, 1]
legend = { f = "#f27d3a", h = "#ffecbd" }
grid = '''
.f.
fhf
fhf
.f.
'''
```

`frames` at the document level gives durations in milliseconds; inside a cel it
lists zero-based frame indices. The example stores one image used in two frames.
Replay creates independent editable cels from that shared description.

Document settings include `palette` (RGB/RGBA hex colours), `tags` (the existing
name/from/to/direction fields), and an optional relative PNG `reference`.
Layer settings are `name`, `opacity` (0–255), `visible`, and `blend`, which default
to fully visible, opaque, normal blending.

Cel fields:

| Field | Meaning |
| --- | --- |
| `frames` | Frame indices; defaults to `[0]` |
| `size` | Logical pixel dimensions; defaults to the document canvas |
| `at` | Signed placement of that logical cel on the canvas; defaults to `[0, 0]` |
| `origin` | Position of cropped grid/PNG pixels inside the logical cel |
| `grid`, `legend` | Equal-width rows and symbol-to-hex colour mappings; `.` clears |
| `image` | Relative lossless PNG path, used instead of a grid |
| `draw` | Ordered existing drawing/effect operations applied after starting pixels |
| `palette` | Optional drawing palette, preserving indexed operation colours |

For example, a gradient remains a concise operation:

```toml
[[layers.cels]]
frames = [2]
draw = [
  { op = "gradient", x0 = 0, y0 = 0, x1 = 63, y1 = 63, stops = [{ pos = 0, color = [21, 18, 39] }, { pos = 1, color = [230, 148, 63] }] },
]
```

This fragment requires frame 2 and a suitable canvas in its containing recipe.
Operations use the same parameters as `doc_draw` and `doc_fx`, without document,
layer, frame or revision arguments. Seeds above TOML's signed integer range are
quoted decimal strings and are restored to the renderer's full unsigned range.

## Replay and migrate

```sh
atelier replay art/lantern                    # bundle containing recipe.toml
atelier replay art/lantern/recipe.toml         # or an explicit file
atelier replay <doc-id>                       # current stored recipe
atelier migrate <doc-id> art/lantern          # export a working document
atelier migrate old.jsonl art/lantern         # legacy import
python3 tools/migrate-recipes.py old-recipes --out target/migrated-art
```

The batch script accepts files or directories, groups only identical legacy
command streams, and writes `migration.json` mapping every original path to its
new recipe. It stops on failure and keeps the partial report. Originals are never
overwritten; retry into a new directory. Changing a game's asset references is
an explicit next step using that mapping.

Migration compares complete metadata, logical cel bounds, placement and exact
RGBA pixels. Normal edits use the same check before publication. Checkpoints
and portable archives include the complete recipe bundle.

The parser refuses unknown fields, duplicate frame assignments, traversal and
symlink resources. Text is limited to 16 MiB, decoded/logical pixels to the
existing 64-megapixel document budget, and total rendering work is bounded.
The versioned `format` and `renderer` fields make interpretation explicit.
