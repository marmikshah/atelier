# Recipe formats

Atelier recipes are ordered tool calls. `atelier replay` detects all three
supported representations by content, not filename:

- authored JSON: `{name, description, steps}`;
- legacy JSONL: one `{tool, args, note?}` object per line;
- compact JSONL v2: a versioned header followed by compact call lines.

Existing JSON and legacy JSONL remain readable. Unknown compact versions fail
instead of being guessed.

## Compact JSONL v2

The first non-empty line is the header:

```json
{"v":2,"name":"hero","description":"walk cycle","defaults":{"doc":"hero","layer":0,"frame":0}}
```

`defaults` is optional. Its `doc`, `layer`, and `frame` values form a sticky
context. An action can update any value before it runs:

```json
{"call":"doc_layer","at":"d","args":{"op":"add","name":"FX"}}
{"layer":1,"batch":[["line",1,2,8,9,"#ffe080ff",1]]}
{"frame":1,"batch":[["clear_cel"],["fill_cel","#202840"]]}
```

Generic calls use:

- `call`: the full Atelier tool name;
- `args`: the fields that were not moved into context; omitted means `{}`;
- `at`: which context fields to restore into `args` (`d` → `doc_id`, `l` →
  `layer`, `f` → `frame`);
- `doc`, `layer`, `frame`: context updates applied before this call;
- `note`: optional human narration, preserved by compact/expand.

For hand-authored files, a context-only line is also valid:

```json
{"use":{"doc":"hero","layer":0,"frame":3}}
```

A `batch` line is shorthand for `doc_batch` and always restores all three
context fields. Its optional `frames` has the same meaning as the tool field.

### Batch tuples

Only an exact known shape becomes a tuple. An operation with extra, missing,
or future fields stays a normal JSON object inside `batch`, so compaction is
lossless and forward-compatible.

| Operation | Tuple values after the operation name |
|---|---|
| `line` | `x0, y0, x1, y1, color[, size]` |
| `rect` | `x0, y0, x1, y1, color[, fill]` |
| `ellipse` | `cx, cy, rx, ry, color, fill` |
| `polyline` | `points, color, size` |
| `pencil` | `points, color` |
| `clear_cel` | none |
| `fill_cel` | `color` |
| `glow` | `radius, intensity, color, mode` |

Tuple colors use `#rrggbb` or `#rrggbbaa`, retaining whether the source had
three or four channels. Point lists flatten from `[[x,y], ...]` to
`[x,y,...]`; `fill` uses `0`/`1`. Expansion restores the original JSON types.

## Commands

Replay needs no conversion:

```sh
atelier replay recipe.jsonl --home /tmp/rebuild
```

Convert to compact v2, expand to readable authored JSON, or measure the
possible reduction:

```sh
atelier recipe compact recipe.json -o recipe.jsonl
atelier recipe expand recipe.jsonl -o recipe.json
atelier recipe stats recipe.jsonl
atelier recipe stats recipe.jsonl --json
```

Use `-` as input for stdin. Without `-o`/`--output`, converted content goes to
stdout. Writing over an existing file uses Atelier's normal same-directory
atomic replacement and keeps the previous content as `.atelier-backup`.

`compact → expand` preserves the recipe name, description, ordered tool names,
argument values, and notes exactly. Encoding is deterministic, so compacting
the same logical recipe produces the same bytes.

## Recording and crash safety

New document journals and `--record` sessions use compact JSONL v2. A document
with an existing legacy journal continues appending legacy `{tool,args}` lines;
Atelier never mixes formats inside one file.

The header and each action occupy a complete line. Appends never rewrite prior
steps, and replay tolerates only a syntactically partial final line—the normal
result of a process dying mid-write. Corruption in the middle, or a complete
but invalid final action, is an error so a truncated rebuild cannot look
successful.
