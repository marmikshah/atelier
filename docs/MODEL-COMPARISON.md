# Model comparison — same brief, same tools, different models

Four models drove atelier's MCP tools with a **byte-identical instruction set**
and no other help. One task, one run each, no retries, no cherry-picking.
The point is not "which model wins" — it is that atelier makes agent art
**measurable**: every run below self-audited with the same tools and every
claim in the table comes from those audits, not from vibes.

## The brief (verbatim, identical per model)

> Draw a pixel-art POTION BOTTLE. 1) `doc_create` 32×32. 2) Choose exactly 6
> colours (glass, liquid dark, liquid light, cork dark, cork light, highlight)
> and lock them with `doc_set_palette`. 3) Draw: a rounded glass flask with a
> narrower neck, a cork stopper, liquid filling about two thirds with a lighter
> meniscus line and a glass highlight, light from the top-left, and a 1px
> darker outline. 4) `doc_look` after every burst — study the image and fix
> what reads wrong; at least two look-and-fix iterations. 5) Run
> `doc_critique`, `doc_palette_report`, `doc_silhouette`; fix warnings.
> 6) `doc_export` the sheet.

Environment: the released atelier binary (core tool profile) over MCP; each
model ran as an isolated agent with only the listed tools.

## Results

| | Haiku 4.5 | Sonnet 5 | Opus 4.8 | Fable 5 |
|---|---|---|---|---|
| render | ![haiku](benchmark/potion-haiku.png) | ![sonnet](benchmark/potion-sonnet.png) | ![opus](benchmark/potion-opus.png) | ![fable](benchmark/potion-fable.png) |
| look iterations | 2 | 5 (+3 zoomed) | 5 | 5 |
| tool calls | 21 | 30 | 43 | 20 |
| tokens (output) | ~37k | ~93k | ~59k | ~54k |
| wall clock | 4.6 min | 10.7 min | 9.7 min | 6.8 min |
| critique | all ok | all ok | all ok | all ok |
| off-palette px | 0 | 0 | 0 | 0 |
| orphan px | 0 | 0 | 0 | 0 (fixed 5 mid-run) |
| contrast | ok | 0.79 | 0.75 | 0.69 |

### Visual read (one judge, stated openly)

- **Fable 5 / Sonnet 5** — both read as the brief: round flask, narrow neck,
  cork, ~2/3 liquid with a lighter meniscus, top-left highlight, clean 1px
  outline. Fable's light placement is the most consistent; Sonnet's silhouette
  is the roundest.
- **Opus 4.8** — correct structure, but the liquid reads as a rim band rather
  than a fill, so the two-thirds level is ambiguous. It also worked around a
  real engine bug it discovered (below), which cost 13 extra tool calls.
- **Haiku 4.5** — every scalar audit passed, but the *shape* doesn't read: the
  liquid is a framed panel and the silhouette is lumpy. It also stopped at the
  minimum two look iterations.

## What the experiment actually shows

1. **The floor is high for everyone.** All four models shipped 0 off-palette
   pixels, 0 orphans, and a closed single-blob silhouette — because the loop
   (lock palette → look → audit → fix) *forces* those properties. The
   discipline lives in the tool, not the model.
2. **Iteration count tracks quality more than model size.** The visible
   quality gap (Haiku vs the rest) coincides with 2 look-and-fix passes vs 5.
   Look iterations are the quality budget.
3. **Scalar audits can't see proportion.** Haiku passed every check while its
   flask reads wrong — exactly the blind spot `doc_critique_vision` (host-model
   eye) exists to cover. Numbers keep art *clean*; only looking keeps it
   *right*.
4. **Benchmarks find footguns.** The Opus run reported `doc_fx op=outline`
   "always writes black" and spent 13 tool calls repainting around it. A
   replay repro shows outline honours a well-formed `[r,g,b,a]` colour — the
   actual defect is that a malformed colour value silently falls back to
   black instead of erroring. A silent default that costs an agent 13 calls
   is a bug regardless; surfaced by the benchmark, not by a bug report.

## Caveats

Single run per model, single task, one visual judge; the drawing surface was
the *released* core profile (this branch's newer tools — pose cycles,
autotiles, form audit — were not available to the runs). Treat the numbers as
a reproducible protocol, not a leaderboard: the brief above is copy-pasteable,
and any MCP-capable model can be added to the table by running it verbatim.
