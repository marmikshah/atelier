# Artwork sources

Keep the current artwork as a named, editable source. Keep exploratory changes
in a working document's local journal. Source format 1 / renderer 1 compiles to
the same layered RGBA document used by the existing editing and export tools.

## Commands

```sh
atelier source migrate recipe.jsonl art/lantern
atelier source check art/lantern
atelier replay art/lantern --home target/art-review
atelier source inspect art/lantern
atelier source inspect art/lantern --part flame-tip
```

A bundle is a directory with `source.toml` and any referenced PNG resources.
An explicit TOML filename also works. The original recipe is untouched, the
destination must be new, and migration publishes it only after checking all
metadata, cel positions, logical dimensions and exact RGBA pixels. Atlas layout,
layer order, animation timing, tags, palette order and hidden RGB are retained.
No quantisation, resampling or automatic generator inference occurs.

`--mode auto` uses inline grids for cropped regions up to 1,024 pixels with a
small colour vocabulary. Larger regions become lossless PNGs. It also attempts
to recover a construction graph from single-frame drawing, effects, palette
sets, and supported layer operations, discarding overwritten construction.
At each final cel it chooses verified construction when smaller, allowing
procedures and pixels in the same bundle.
`--mode pixels` explicitly bakes the current pixels. `--mode procedural` requires
a recoverable, pixel-identical construction graph and fails otherwise. These
are representation choices, not a promise of a globally minimal encoding.

Recovering artistic relationships remains an authoring decision: migration
does not assume two similar flames should share edits, invent palette roles
from RGBA equality, or infer a seed from a texture. Use named parts and explicit
references when that relationship is intended. The migrator preserves independent
cels; authoring shared animation assignments can save more space afterwards.

## Migrate a collection

```sh
python3 tools/migrate-recipes.py path/to/recipes --out target/migrated-art
```

Use `--atelier target/release/atelier` for an uninstalled build. Files and
directories can be mixed; directories are searched recursively for JSONL.
The script creates stable names from the document name and a command digest,
groups exact duplicates that differ only in working UUID stamps, and writes
`migration.json`, mapping every original file to its verified bundle. Historical
variants remain separate. It stops on failure and leaves completed bundles and
the partial report for inspection; retry into a new directory. No original is
deleted and no game asset path is rewritten. Review the map before changing a
game's references. The report and rendered output are generated review artifacts;
the TOML and authored PNGs are source inputs worth tracking.

## Format

```toml
format = 1
renderer = 1
name = "Ember"
canvas = [8, 8]
frames = [100, 120]
palette = ["#ffcc00", "#e06020"] # document swatches, in order
layers = [{ id = "light", name = "Light" }]
cels = [{ layer = "light", frames = [0, 1], part = "pose" }]

[inks]
flame = "#ffcc00" # live named role; distinct from equal-colour roles

[parts.tip]
size = [3, 2]
legend = { x = "flame" }
grid = '''
.x.
xxx
'''

[parts.pose]
size = [8, 8]
items = [{ part = "tip", at = [2, 1], scale = 2 }]
```

`frames` defaults to `[100]`. Palette colours and inks accept `#RRGGBB` and
`#RRGGBBAA`. Swatches are document metadata; grid legends explicitly bind to a
named ink or literal hex colour. Equal values do not merge ink identities.

Layer IDs and part names use ASCII letters, digits, `_`, `-`, `.`, `/` (1–128
bytes). Layers are ordered, with `opacity = 255`, `visible = true`, and
`blend = "normal"` by default. `tags` use the existing `name`, `from`, `to`,
`direction` fields, and an optional `reference` names a local PNG.

Each part has `size = [width, height]` and exactly one definition:

| Definition | Fields | Intended use |
| --- | --- | --- |
| Grid | `grid`, `legend`, optional `origin` | Small pixels you can read spatially |
| Image | `image = "pixels/body.png"`, optional `origin` | Large authored pixel regions |
| Group | `items = [{ part = "name", ... }]` | Ordered instances of named parts |
| Drawing | `draw = [{ op = "rect", ... }]`, optional `base`, `palette` | Existing core drawing/effect operations |

Logical `size` is independent of cropped resource dimensions. `origin` places
the grid/image inside those bounds and defaults to `[0, 0]`. All rows in a grid
have equal width; `.` means transparent. PNGs contain resolved RGBA pixels, not
live ink bindings. Replacing a grid rebuilds from its new contents: a dot clears
the old coverage. This differs from `doc_paint_grid`, whose dots skip painting.

Drawing parts use existing operation schemas and rasterisation. `base` is an
optional same-sized part to start from. A drawing part's `palette` defaults to
the document palette; `[]` means unlocked. Procedures retain literal operation
colours; instance ink bindings affect grid references. Seeds belong to the
renderer's unsigned 64-bit range; values above TOML's signed integer range are
written as quoted decimal strings (migration does this automatically). Seeds
also belong to the
declared renderer version; a future algorithm change needs a new renderer
contract, not a silent reinterpretation.

Group instances support `at`, integer `scale` (1–16), `flip_x`, `flip_y`, and
`turns` (0–3 clockwise quarter turns). Apply flips around logical bounds, then
turn, scale, and translate; output is clipped to the containing group.
`mode = "replace"` copies every RGBA tuple, including transparent pixels, by
default. `mode = "over"` uses source-over compositing and accepts layer-style
`opacity`/`blend`. `bindings = { flame = "another-ink" }` overrides ink roles
within that instance. A part edit reaches all references to that part; duplicate
the part and change one reference to detach an instance.

Each `cels` record names a layer, a part, one or more zero-based frame indices,
and optionally `at` for the cel offset. A layer/frame may be assigned once.
Repeated frames reference the same authored definition but resolve to ordinary
independent working cels. Six atlas regions are still one frame if the consumer
uses a single-frame atlas: changing storage does not change runtime layout.

Resources are regular PNG files with relative ASCII paths inside the bundle
(at most 200 bytes). Traversal and symlinks are refused. Unknown fields and
unsupported versions fail. Compilation checks dimensions, cycles, dependency
depth, instance expansion, operation work and decoded pixel budgets first.
The canvas and drawing-part axes are limited to 4,096 pixels; pixel/group parts
use the document cel's 64-megapixel limit, retaining scaled off-canvas artwork.

## Focused edits and rebuilds

`inspect` returns names, dependencies, sizes and a revision, omitting bulk pixel
data. `--part` returns one definition and its named inks. To apply related edits,
write a JSON batch and use the revision returned by inspection:

```json
{"inks":{"flame":"#ffe090"},"parts":{"tip":{"size":[3,2],"grid":"...\n.x.\n","legend":{"x":"flame"}}}}
```

```sh
atelier source apply art/lantern --file edits.json --expected-revision HASH
atelier replay art/lantern --home target/art-review
```

A part entry is a complete replacement; ink entries must name existing roles.
The batch validates and compiles before one atomic manifest replacement, keeping
unrelated comments. The revision hashes the exact manifest and every PNG, so
direct text/image edits invalidate stale requests. Cooperating CLI writers use
an advisory `.atelier-source.lock`; ignore that file in Git. External editors
do not take this lock: avoid saving through two editors at the same instant.
Image edits use an external editor and are checked on the next load/build.

Source compilation is below MCP and publishes one completed working document.
It saves a self-contained starting source snapshot beside the local journal.
CLI/MCP pixel edits then affect that working document and its replayable local
history. They do not edit an external source definition: use `source apply` to
change that definition, or `source migrate <working-id> <new-dir>` to bake the
working result into a new candidate. Checkpoints, verification, and portable
archives preserve the source snapshot too. Keep the whole snapshot with its
journal; a copied journal alone cannot rebuild its starting artwork.

The source commands are currently CLI/application APIs. Existing MCP inspection
and export tools operate on the built document. A visual source editor, richer
MCP source tools, automatic semantic decomposition and CV upscaling are separate
follow-up work; this format does not require any of them to rebuild artwork.
