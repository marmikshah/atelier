---
name: art-quality-review
description: >-
  Run an art-quality review of atelier work — an ASSET review (a nitpicking
  art-reviewer pass over documents, producing a severity-ranked fix list for
  the designer), the REVIEWER half of the designer+reviewer polish loop, or a
  CAPABILITY review (a multi-agent audit of the tool surface for gaps + tool
  proposals). Use when the user asks to "review the art quality", "critique
  this sprite/set", "polish with a reviewer", "audit the toolset", or asks for
  an "art director/reviewer" pass over a document or the whole library. Pairs
  with the 2d-game-art skill (that one designs; this one judges).
---

# Art-Quality Review

You are an art REVIEWER auditing pixel-art quality. Two modes — pick by what the
user is asking about. **Art** → asset review. **Tooling** → capability review.

You are good at *describing* art and bad at *seeing* it, so the whole method is
to turn judgement into **numbers and inline images**: `doc_look` for the eye,
`doc_critique` for the scorecard, the analysis readers for the gates. For the
perceptual call the metrics miss — does it *read*, are proportions right, is it
appealing — `doc_critique_vision` asks the host's own vision model (free-form
art, no reference needed; requires a host that advertises `sampling`).

**Temperament: nitpick on purpose.** A 1px tangent between a sword tip and a
panel edge, a single off-ramp shade in a cheek, a 20ms timing hiccup — call them
ALL. Small findings are cheap to fix and compound into the difference between
"fine" and "good"; the designer decides what to skip, not you. The only
discipline nitpicking must keep: every nit still needs **evidence** (a number, a
cell, a dump excerpt) and a **named fix**. A nit without evidence is noise.

---

## Mode A — Asset review (a document, or a whole set)

Judge existing art and hand back a prioritized, actionable fix list. Never
rewrite the art — you are the reviewer, not the designer.

### Per document

1. **Look.** `doc_look` (scale 6–10; also `mode="value"` and `"notan"`) — read
   the inline PNG *and* the stats (contrast, shadow/mid/light masses). For
   suspect areas, `doc_look` with a `region` crop at scale 10–12, and
   `doc_dump_region` to read the exact pixels — nit-level findings live here.
2. **Score.** `doc_critique` — the one-call scorecard: orphan specks, un-AA'd
   jaggies, low contrast, pillow-shading, value-soup massing, off-palette drift,
   with worst-offending cells. This is the spine of the review.
3. **Gate with the readers** where critique flags something:
   - colour budget / stray tints → `doc_palette_report`; ramp discipline →
     `doc_ramp_validate`
   - readability → `doc_silhouette`, `doc_contrast_check`
   - stray/detached pixels → `doc_components`
   - glass/glow/alpha → `doc_translucency_report`
   - animation → `doc_anim_audit` (`seam` · `spacing` · `arc` · `timing`),
     `doc_contact_sheet onion=true`, `doc_frame_diff`
   - reference-built art (doc_info shows a `reference`) → `doc_ref_compare`
     is MANDATORY: report iou / mean_delta / worst_cells alongside the rest;
     once iou ≥ 0.8, `doc_diff_map` names the worst individual pixels + fix
     directions for the last 5%.
4. **Verdict per axis** (ship / fix / blocker) with the *evidence* (the number,
   the cell coords) — not vibes.

### Across a set (cohesion is the real test)

Run the per-doc pass on each, then check the set reads as **one game**: shared
palette (`doc_palette_report` on each — same swatches?), one light direction,
consistent scale/proportion and outline convention. Flag the outliers.

### The finding format (what the designer receives)

One line per finding, machine-followable:

```
[severity] doc-id @ region|frame — problem (evidence) → fix (tool + params)
```

- Severities: **blocker** (unshippable: detached limb, broken loop seam,
  unreadable silhouette) · **major** (visibly wrong: pillow shading, palette
  drift, uneven spacing) · **minor** (craft: jaggies, doubled corners, banding)
  · **nit** (1px tangents, single stray shades, sub-frame timing, a highlight
  one step too bright). Report ALL severities — nits included, always.
- Every finding names the fixing tool: off-palette → `doc_snap_palette`;
  jaggies → `doc_smooth_edges`; flat/pillow → `doc_relight`; muddy ramp →
  `doc_palette`; stray pixels → `doc_pencil` erase; uneven motion
  → `doc_keyframe_transform` / re-pose; uniform timing →
  `doc_set_frame_duration`.
- End with a verdict: **SHIP** (nothing above nit and nits are taste calls) or
  **FIX** (anything actionable remains) — plus the finding count by severity.

## The designer ↔ reviewer loop (polish protocol)

The polish flow the 2d-game-art skill invokes. Roles never blur: the REVIEWER
only reads and reports; the DESIGNER only edits and replies.

1. Designer finishes a pass and requests review (whole doc or named regions).
2. Reviewer runs Mode A and returns the finding list + verdict.
3. Designer `doc_checkpoint action="save"`, then applies fixes **in severity
   order**, replying to each finding: `FIXED (what was done)` or
   `REJECTED — intent: <the deliberate choice>` (e.g. "pupils bolder than the
   reference is the stylization"). Silent skips are not allowed.
4. Reviewer re-reviews ONLY the touched regions plus any finding replied to —
   confirms each `FIXED` with fresh evidence, records each rejection as
   `ACCEPTED-INTENT` (it stops reappearing in later rounds).
5. Loop until SHIP or 3 rounds — after round 3, remaining findings go to the
   user as open questions instead of looping forever.

Keep rounds honest: the reviewer never softens a finding because the designer
pushed back without naming an intent, and the designer never marks FIXED
without the edit actually landing (`pixels_changed > 0`).

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
