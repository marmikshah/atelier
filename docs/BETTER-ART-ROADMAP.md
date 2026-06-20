# atelier — Better Art: Tools vs AI-Assist (Round 3)

> 16-agent review (2026-06-20): 3 fresh ambitious draws on the *current* toolset
> (face bust, lit archway, creature) to find the real ceiling, 3 code-gap readers,
> parallel design of deterministic tools (4 lenses) + AI-assist endpoints (4 lenses,
> each weighed vs the no-network ethos), a judge, and an adversarial critic.

## The answer (reconciled with the critic)

**Both — but the bottleneck is SIGHT, not brushes.** The probes show the
deterministic suite *already* produces a connected, lit, on-palette silhouette
(2 of 3 probes "good"). Every remaining failure is one of two kinds, and neither
is "the agent can't express it":

1. **The agent can't SEE its own error.** It gates on aggregate scalars (IoU, one
   mean ΔE, 16px worst-cells); past IoU≈0.85 the tools "go quiet" while the last
   5% (a 2px-off edge, a misplaced highlight, a wrong-hue shadow, an egg-shaped
   head) stays invisible. *A better brush in a blind hand paints a better wrong
   picture.* **Feedback is the cap, for an LLM especially — it has no innate
   raster perception, so feedback quality IS its perception.**
2. **A few mechanical writer gaps** — no rim-light, no cast-shadow projection, no
   terminator/AO/hue-shift in relight, no concave-silhouette primitive — all pure
   deterministic wins.

The load-bearing ethos is **"every pixel is *authored* (deterministic,
reproducible, explainable)"** — NOT "every pixel is *decided without sight*." That
distinction is the whole AI answer: an endpoint that gives the agent *sight*
without *authoring* pixels is fully ethos-compatible.

## So: do we need AI-assisted endpoints?

**Exactly one, and it's worth adopting aggressively:**

- **`doc_critique_vision`** — an opt-in tool that uses **MCP host sampling**
  (`CreateMessage`) to ask the *host's* model (e.g. Claude Code) to look at the
  rendered PNG vs a one-line brief and return issues / severity / bbox / fix
  direction — **no pixels, no bundled weights, no socket opened by atelier, no API
  key**. The host runs its model; atelier stays a static no-network binary. This
  is the perceptual eye the scalar suite structurally cannot be (proportion,
  design, "still flat," "horn reads as a tail"). Falls back to scalar critique if
  the host declines.

**Everything else AI = reject or defer**, because it breaks the authored-pixel /
static-binary / no-network invariant for marginal gain:
- **reject** AI-as-canvas + `doc_neural_shade` — a model authoring final pixels;
  also diffusion raster doesn't even survive the 32–64px clamp (eyes → 2×3px, off
  palette) so it needs hand-redraw anyway.
- **reject** bundled local diffusion concept generator — hundreds of MB + runtime
  in a 9MB binary; `doc_set_reference` already covers "supply intent."
- **optional/defer** `doc_gen_reference`/`infer_pose`/`infer_depth` — only as an
  out-of-binary sidecar behind `--features`, file-in/file-out, output ALWAYS
  routed through `set_reference`/guide layers, never a final cel; deterministic
  fallback always present. Don't build until the deterministic core lands — it
  reaches most of the same ceiling.

## And better tools? Yes — they're WRITERS, second priority after sight

All pure-Rust, deterministic, reuse fields the engine already computes.

## Recommended path (ordered — sight before brushes)

1. **`doc_diff_map` — per-pixel signed ΔL/ΔC/ΔH error map (S–M, deterministic).
   THE highest-leverage move; do it first.** In `ref_compare`'s existing
   both-opaque loop emit a heat PNG + the worst *individual* pixels each tagged
   `fix:"lighten+cool"` from `sign(dL,dC,dH)`. Reuses `oklab_delta` + `encode_png`
   — near-zero new science. Converts every later writer from open-loop to
   closed-loop. **The multiplier on everything else.**
2. **`doc_critique_vision` (M, the one AI adopt).** MCP host-sampling perceptual
   eye — the only thing that sees proportion/design/"still flat." Ethos-free.
3. **`doc_rim_light` / `doc_contour_paint` (M, deterministic).** Boundary-trace +
   outward normal from the `interior_distance` gradient; paint inward weighted by
   `dot(normal, light)^falloff`; inverse mode = contact/occlusion edge. Kills the
   dominant manual cost (rim light, edge highlights, contact shadow) and is
   topological so it survives 48px where Fresnel washes out.
4. **`doc_relight` v2 (L, deterministic).** Terminator band + contact-AO
   (chamfer from internal concavity minima) + **warm-light/cool-shadow hue ramp**
   via `make_ramp_oklch` — fixes the #1 ceiling (pillow / flat-plastic /
   chromatically-dead) without breaking the signature. Pair with a
   `ramp_validate` temperature gate so feedback finally flags dead hue.
5. **`doc_cast_shadow` (L) + `doc_contour` (L), deterministic.** Projected shadow
   smear clipped to receiver alpha; a closed-outline primitive with
   sharp/round/concave vertex tags to break the balloon-animal ceiling (the
   primitive `figure`/`stroke` feed into).

**Deterministic also-adopt (lower tier):** `doc_surface_grow` (rule-scored moss/
rust/grime from interior-distance crevices + shading side) and `doc_segment`
(deterministic CC/colour-cluster region masks; make `snap_palette` selection-aware
to fix the relight→snap trap). Both pure-Rust; an optional learned refinement can
ride behind `--features infer` later.

## The one-line answer to the maintainer

> Better tools, yes — but **fix the eyes first**. The cheapest, highest-leverage
> move is a deterministic per-pixel error map (`doc_diff_map`); the one AI endpoint
> worth adding is host-sampled vision critique (`doc_critique_vision`) — it gives
> sight without authoring a pixel, so it *keeps* the ethos instead of breaking it.
> Generative / neural-pixel AI stays out of the binary.
