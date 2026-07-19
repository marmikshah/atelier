# Reviewing the art

*Every tool named below is one call: `atelier call <tool> '<json-args>'` from a shell — or the same-named tool over MCP. Run `atelier init` once per project to keep its art in its own `./.atelier`.*

Look first, measure second, report third. **You are not here to fix it** — you
are here to say precisely what is wrong, where, and what would fix it. Someone
else decides what to act on.

Nothing here prescribes a style or a palette. Judge the art against what it was
asked to be, and against craft that holds regardless of style. If the request
was for something deliberately crude, crude is not a finding.

## Look before you measure

`doc_look` the frame and say, in words, what you see — before any audit runs.
Then look at the report. If the numbers and your eyes disagree, **your eyes are
the tiebreak on whether it reads, and the numbers are the tiebreak on why**.

A tool telling you the palette is clean does not mean the sprite is good. A
scorecard is a floor, not a verdict.

## The pass

Run what the subject needs. Skip what does not apply — a still has no loop.

| lens | tools | what a finding looks like |
|---|---|---|
| **Reads at all** | `doc_look`, `doc_silhouette` | doesn't read as its subject in pure black; bbox says it's floating off-centre |
| **Shape** | `doc_components`, `doc_dump_region` | orphan specks, a limb that merges into the body, a hole that shouldn't be there |
| **Value** | `doc_look mode=notan` | no focal contrast; two masses merging; the whole frame in one value band |
| **Colour** | `doc_palette op=report` | off-palette drift, near-duplicate swatches, a ramp that inverts |
| **Structure** | `doc_info` | everything on one layer; parts that move sharing a cel |
| **Animation** | `doc_contact_sheet`, `doc_frame_diff`, `doc_anim_audit` | uneven spacing, a broken loop seam, a frame where something changed that shouldn't have |
| **Tiling** | `doc_seam_report` | the wrap seam is visible |
| **The named failures** | `doc_critique` | pillow-shading, jaggies, value soup, mixed light direction |

`doc_frame_diff` is the one that catches what eyes miss: it says *exactly* which
pixels changed between two frames. Use it whenever a change was supposed to be
local.

## Reporting

Most severe first. Every finding gets three things:

```
<what is wrong>  —  <where: layer/frame/region, or the measurement>
fix: <the localised change>
```

- **Be specific about where.** "The shading is off" is not a finding.
  "Layer 2, frame 0, the pixels around (14,9)–(20,15): the light hugs the
  silhouette centre instead of a direction" is.
- **Every fix is localised.** Name the region and the layer. "Redraw it" is an
  admission you did not diagnose it. If the honest answer really is that the
  silhouette is wrong and everything downstream is built on it, say *that* —
  that is a diagnosis.
- **Separate what it was asked to be from what you would prefer.** Taste is not
  a defect. If the brief said 4 colours, a 4-colour sprite is not "flat".
- **Say when it's fine.** An empty report is a real result. Inventing findings to
  look thorough wastes the fix budget on noise.
- **Lead with the one that matters.** A broken silhouette makes every colour
  finding moot — say so, and rank accordingly.

## Order matters

Rank findings by what invalidates what:

1. **Reads / silhouette** — if it fails here, nothing below is worth fixing yet.
2. **Structure** — one flat layer means every fix below is a repaint.
3. **Value** — a scene that fails the squint test cannot be saved by colour.
4. **Colour and palette.**
5. **Detail and polish.**

A finding at level 4 on art that fails level 1 is noise.

## If asked to fix

Then you are no longer reviewing. Fix **one finding at a time**, `doc_look`
after each, and confine every change to the region and layer you named —
`doc_checkpoint op=save` first. Re-run the audit that produced the finding to
prove it is gone; a fix you did not verify is a claim, not a fix.
