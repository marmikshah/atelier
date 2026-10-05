# Getting started

[README](../README.md) · [Running Atelier](running.md) · [Development](development.md)

Install Atelier using the [README instructions](../README.md#get-started).
The examples below use a POSIX shell, such as Bash or Zsh.

## Your first sprite

Create a working directory and opt into a local document store. If you have
`ATELIER_HOME` set, unset it first to use this directory-local store:

```sh
mkdir atelier-art
cd atelier-art
atelier init
atelier call doc_new '{"name":"spark","width":8,"height":8}'
```

`doc_new` prints JSON containing a `doc_id`. Copy that UUID into this variable:

```sh
DOC_ID='paste-the-returned-doc_id-here'
```

Every document call needs that ID. There is no implicitly selected document.
A new document has one layer and one frame, both at index `0`.

Paint a small spark using character rows and a colour legend:

```sh
atelier call doc_paint_grid "{
  \"doc_id\": \"$DOC_ID\", \"layer\": 0, \"frame\": 0,
  \"x\": 1, \"y\": 1,
  \"legend\": {\"y\": [255, 210, 80, 255]},
  \"rows\": [\"..y..\", \"..y..\", \"yyyyy\", \"..y..\", \"..y..\"]
}"
atelier call doc_look "{\"doc_id\":\"$DOC_ID\",\"out_path\":\"spark.png\"}"
```

Open `spark.png` in an image viewer. Each `y` paints one pixel; `.` leaves a
pixel untouched. Edits are saved automatically in `./.atelier`.

## How documents fit together

- A **document** contains the canvas, layers, and animation timeline.
- A **layer** holds a part of the artwork, such as a body or a moving limb.
- A **frame** is one step in the animation.
- A **cel** holds the pixels at one layer/frame intersection.

Layer and frame indices start at zero. Use `doc_info` to inspect structure and
`atelier library` to find stored document IDs.

A typical workflow is create → draw → inspect → adjust → export. Use
`doc_layer` to separate parts, `doc_frame` to animate, and `doc_export` for
spritesheets, GIFs, APNGs, or fonts. Use `doc_look` for a PNG preview.

## Find tools and arguments

```sh
atelier tools                         # tool names and short descriptions
atelier tools --markdown              # full reference from the current registry
atelier tools --schema doc_paint_grid # input schema for one tool
atelier call doc_info "{\"doc_id\":\"$DOC_ID\"}"
```

For larger inputs, save the JSON to a file and pass it with
`atelier call doc_paint_grid --file grid.json`, or pipe JSON to
`atelier call doc_paint_grid --stdin`. CLI reports go to stdout; logs go to stderr.

For drawing workflows, read the bundled [sprite](../crates/atelier/skills/sprite.md),
[scene](../crates/atelier/skills/scene.md), and
[review](../crates/atelier/skills/review.md) guides. See [Running Atelier](running.md)
to install these as agent skills or connect an MCP client.
