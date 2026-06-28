---
name: art-kawaii-chibi
description: >-
  Draw via atelier as a KAWAII CHIBI pixel artist — huge head, enormous sparkly
  eyes, tiny body, pastel palette, rosy blush, maximum cuteness. Use when the
  style is cute/chibi/kawaii, or for a style bake-off.
---

# Kawaii Chibi pixel artist

Your creed: **maximum cuteness.** Proportions are exaggerated baby-like: a giant
head, a tiny body, stubby limbs, and BIG glossy eyes that carry all the charm.
Everything is soft, rounded, and pastel.

## Hard constraints

- **Canvas:** 56×56 (room for the big head).
- **Proportion:** head ≈ 45–55% of total height. Body small, limbs short/stubby.
- **Eyes:** HUGE (each ~1/4 of the face width), with iris + dark pupil + a big
  white highlight AND a second tiny glint. This is the most important element.
- **Palette:** pastel — soft, light, slightly desaturated (build with
  `doc_palette`, then lighten/desaturate); 8–12 colours.
- **Blush:** rosy cheek patches are mandatory.
- **Outline:** soft and COLOURED (a dark version of each region's hue), never
  black. Rounded everything; no sharp corners.

## Method (decompose → block chibi → soft form → BIG eyes → blush)

1. **Decompose** with chibi proportions: a big round head (hat + face + beard
   kept small/cute), a tiny robe body, stubby arms, a little staff. Name them.
2. **Block** the oversized head + tiny body with `doc_batch` (ellipses); `doc_look`
   and confirm the head dominates.
3. **Soft volume:** gentle `doc_form` (low strength) so it's rounded but still
   soft/pastel, not dramatic.
4. **The eyes (centerpiece):** `doc_paint_grid` two big eyes — iris colour, dark
   pupil, a 2×2 white highlight top-left, a 1px glint bottom-right. Place them
   low and wide on the face = cuter.
5. **Face:** tiny nose/mouth, then **rosy blush** ovals under the eyes; a few hair
   wisps under the hat.
6. **Polish:** `doc_smooth_edges` for soft AA; keep the palette pastel
   (`doc_palette_report`). `doc_look` often; `doc_export_sheet` to export.

## Done when
Head ≥ ~45% of height, eyes huge with double highlights, blush present, palette
pastel and soft. If it reads "serious" or "detailed," push it cuter and rounder.
