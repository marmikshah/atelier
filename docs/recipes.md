# Editable replay files

Atelier uses `.atelier` source files. A file describes the current canvas,
layers and frames, with an ordered construction inside each frame. CLI and MCP
mutations maintain `documents/<id>/recipe/recipe.atelier` atomically. JSONL is
accepted only by the one-time migration command; normal editing and replay
never produce or execute a JSONL log.

The source is a small KDL-style domain language. It supports quoted strings,
decimal integers and finite floating-point numbers, `#true`/`#false`, child
blocks, `//` comments and nested `/* ... */` comments. Nodes end at a newline
or semicolon. Strings use `\n`, `\t`, `\r`, `\"`, `\\` and `\u{...}` escapes.
Full KDL annotations, raw strings and extensions are outside this grammar.
Parsing and schema checking report source locations, reject unknown names and
duplicates, and bound nesting, pixel counts and rendering work.

## A complete source

```kdl
atelier "Lantern" format=1 renderer=1
canvas 16 32
frames 100 100

layer "Brass body" {
  frame 0 1 {
    pixels x=5 y=3 {
      legend {
        color "b" "#69442d"
        color "g" "#dba85b"
      }
      grid """
        .bbb
        bgggb
        bgggb
        .bbb
        """
    }
  }
}
layer "Flame" {
  frame 0 {
    pencil color="#ffb84d" {
      points { point 8 6; point 8 7; }
    }
  }
  frame 1 {
    pencil color="#ffd56b" {
      points { point 8 5; point 8 6; }
    }
  }
}
```

Layers keep their order. `frame 0 1` shares the construction between those
frames; editing one detaches it. `size W H` preserves a cel's logical bounds,
and `at X Y` places that cel on the canvas. Omitted size and position mean the
canvas dimensions and `(0, 0)`. Settings precede layers. Layer properties are
`opacity=0..255`, `visible=#false`, and `blend="multiply"` or another renderer
blend name. `tag "idle" from=0 to=1 direction="forward"` defines an animation
range. `palette "#..." ...` records document colours; a palette node inside a
frame fixes the colours used by subsequent operations.

## Pixels and drawing commands

Small sprites use cropped grids. Rows can omit trailing dots; `.` and spaces
leave existing pixels alone. To erase a pixel, name an explicit `#00000000`
colour in the legend. Colours preserve RGBA exactly, including invisible RGB.

Large repeated regions can use colour counts instead of spelling every pixel:

```kdl
row "b" 128 "g" 2 "b" 126 repeat=8
```

Rows advance downward from the pixel block's origin. Sparse regions instead
record only occupied strips, positioned relative to that origin:

```kdl
span 5 0 "bbb"
span 4 1 "bgggb" rows=2
```

A `pixels` block can contain grids, rows and spans. Blank surroundings need no
entries. All three forms use the same legend and write exact pixels directly;
counts and coordinates are checked before decoding.

Drawing and effect nodes reuse the existing operation names and strict parameter
schemas in [tools.md](tools.md). Scalar parameters are properties. Arrays use
child nodes: `points { point X Y; ... }`, or `stops { stop pos=0 color="#..."; ... }`.
Drawing colours are RGB/RGBA hex; numeric seeds support the full unsigned 64-bit
range. Operations execute in source order with their authoring palette.

An edit over a gradient keeps the gradient and adds the edit. Corrections can
replace an overwritten operation after verifying exact rendering. Unchanged
comments and formatting survive tool edits. A raster import chooses a grid for
at most 1,024 occupied bounding-box cells, occupied strips for larger regions
with more than 25% blanks, and colour rows otherwise. Later edits retain that
pixel form. Nothing automatically turns into a PNG.

## Replay and migration

```sh
atelier replay art/lantern.atelier --home target/art-review
atelier replay <doc-id>
atelier migrate old.jsonl art/lantern.atelier
atelier migrate <doc-id> art/lantern.atelier
atelier migrate --store --home .atelier
python3 tools/migrate-recipes.py old-recipes --out target/migrated-art
python3 tools/migrate-recipes.py .atelier --replace
```

Migration replays the legacy calls in an isolated store, then checks exact RGBA,
cel bounds, offsets and metadata against the new source. `--out` preserves the
originals and filenames and writes a verification report. `--replace` deletes
external JSONL inputs only after the collection verifies. Store migration keeps
existing document ids and revisions and installs each source atomically; retained
checkpoint recipes migrate too, preserving their labels and pixels. A destination already present is never overwritten.

Sources with a reference image export to a directory containing
`recipe.atelier` and the explicit reference PNG. Artwork pixels stay in source
text. Reference paths must remain inside the directory; links and traversal
are rejected. The working store also maintains raster files for the editor's
existing bounded readers and exports. These are materialized pixels, not replay
source. Checkpoints and portable archives include the matching construction.
