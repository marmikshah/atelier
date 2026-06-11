---
name: art-quality-review
description: >-
  Run an art-quality review of atelier work — either an ASSET review (critique
  existing documents against world-class pixel-art craft using the analysis
  tools, then a prioritized fix list) or a CAPABILITY review (a multi-agent audit
  of the tool surface for gaps + concrete tool proposals). Use when the user
  asks to "review the art quality", "critique this sprite/set", "audit the
  toolset", "find gaps", "what tools are missing", or asks for an "art director"
  pass over a document or the whole library. Pairs with the 2d-game-art skill
  (that one makes art; this one judges it).
---

# Art-Quality Review

You are an art director auditing pixel-art quality. Two modes — pick by what the
user is asking about. **Art** → asset review. **Tooling** → capability review.

You are good at *describing* art and bad at *seeing* it, so the whole method is
to turn judgement into **numbers and inline images**: `doc_look` for the eye,
`doc_critique` for the scorecard, the analysis readers for the gates.

---

## Mode A — Asset review (a document, or a whole set)

Judge existing art and hand back a prioritized, actionable fix list. Never
rewrite the art unless the user says so.

### Per document

1. **Look.** `doc_look` (scale 6–10; also `mode="value"` and `"notan"`) — read
   the inline PNG *and* the stats (contrast, shadow/mid/light masses).
2. **Score.** `doc_critique` — the one-call scorecard: orphan specks, un-AA'd
   jaggies, low contrast, pillow-shading, value-soup massing, off-palette drift,
   with worst-offending cells. This is the spine of the review.
3. **Gate with the readers** where critique flags something:
   - colour budget / stray tints → `doc_palette_report`; ramp discipline →
     `doc_ramp_validate`
   - readability → `doc_silhouette`, `doc_contrast_check`
   - stray/detached pixels → `doc_components`
   - glass/glow/alpha → `doc_translucency_report`
   - animation → `doc_anim_audit` (`seam` · `spacing` · `arc`),
     `doc_contact_sheet`, `doc_frame_diff`
4. **Verdict per axis** (ok / warn / blocker) with the *evidence* (the number,
   the cell coords) — not vibes.

### Across a set (cohesion is the real test)

Run the per-doc pass on each, then check the set reads as **one game**: shared
palette (`doc_palette_report` on each — same swatches?), one light direction,
consistent scale/proportion and outline convention. Flag the outliers.

### The fix list

Rank findings **blocker → major → minor**, each with the tool that fixes it:
off-palette → `doc_snap_palette`; jaggies → `doc_smooth_edges`; flat/pillow
shading → `doc_relight`; muddy values → re-ramp with `doc_make_perceptual_ramp`.
Offer to apply them — and **`doc_checkpoint action="save"` first** so every fix
is reversible.

---

## Mode B — Capability review (audit the tool surface for gaps)

A forward-looking audit: where does the *toolset* fall short of producing
world-class art, and what tools should be added? This is a large multi-agent
job — **only run it when the user explicitly opts into multi-agent
orchestration** (the `Workflow` tool / "use a workflow" / ultracode). Otherwise
describe it and ask.

Shape (scale the agent counts to the ask):

1. **Ground** — a few readers map the *existing* tool surface from source
   (`src/*.rs`) with file:line refs, so proposals don't duplicate what exists.
2. **Probe** — a few agents actually *drive* atelier on hard pieces (a metal +
   glass still, a secondary-motion animation, a depth environment) and log every
   editing friction first-hand. Empirical gaps beat theoretical ones.
3. **Analyze** — one auditor per art-quality dimension, each producing gaps +
   concrete tool proposals (real signatures, what-it-unlocks, builds-on, effort).
   Dimensions: palette/colour-harmony · light/shading · anti-aliasing ·
   dithering · outlining · form/perspective · composition/readability ·
   material/texture · animation principles · rigging · cel transform · selection
   · tiling/environments · VFX · reference/import · typography/UI · the
   agent-feedback-loop (perception).
4. **Audit** — cross-check lenses (dedup + roadmap overlap, feasibility vs the
   Rust cel architecture, art-impact ranking) + a completeness critic.
5. **Synthesize** — a prioritized tool roadmap (tiered table: tool · purpose ·
   quality leap · builds-on · effort), tier-1 deep dives, architectural unlocks,
   and a concrete "build first" sequence.

Bias the output toward two recurring, high-leverage themes: **the near-blind
agent can't see** (perception/critique tools repay disproportionately) and
**non-destructive / transform / structural editing** (the backbone that makes
ambitious work possible).

Write the report to a **local** file (e.g. `docs/ART-QUALITY-REVIEW.md`) — it is
a planning artifact, not shipped repo content.

---

## Principles (both modes)

- **Numbers + inline images, not adjectives.** Every claim cites a measurement
  or a cell.
- **Conservative verdicts.** A blind agent will obediently "fix" false defects —
  a steep deliberate ramp, a one-shot tag, intentional asymmetry. When unsure,
  say *info*, not *warn*.
- **Checkpoint before you change anything.** `doc_checkpoint` is the undo.
- **Cohesion over any single sprite.** A set that shares palette, light and
  scale beats one perfect frame.
