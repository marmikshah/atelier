<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo-wordmark-dark.png">
    <img src="docs/logo-wordmark.png" width="384" alt="atelier">
  </picture>
</p>

<p align="center"><strong>The pixel-art studio agents can see — headless, over MCP.</strong></p>

<p align="center">
  <img src="docs/platformer-scene.gif" width="640" alt="dusk side-scroller scene: cloaked lantern-bearer, owl on a ledge, crystal cave, fireflies">
</p>

<p align="center">
  <img src="docs/showcase/alien-haiku-4-5.gif" width="96" alt="idle space alien, drawn by Haiku 4.5">
  <img src="docs/showcase/potion-sonnet-5.gif" width="96" alt="bubbling potion, drawn by Sonnet 5">
  <img src="docs/showcase/cat-opus-4-8.gif" width="96" alt="wizard cat casting, drawn by Opus 4.8">
  <img src="docs/showcase/ball-fable-5.gif" width="96" alt="bouncing ball, drawn by Fable 5">
</p>
<p align="center"><em>Alien by Haiku 4.5 · potion by Sonnet 5 · wizard cat by Opus 4.8 ·
ball by Fable 5 — same briefs, four models, zero hand-editing.
Full benchmark with compare views: <a href="https://marmikshah.github.io/atelier/">marmikshah.github.io/atelier</a>.</em></p>

## What it is

Agents are good at *describing* art and bad at *seeing* it. atelier closes the
loop: every drawing op is a tool call, and `doc_look` hands back a PNG the
agent can actually look at, judge, and correct — the same look-and-fix loop a
human uses in an editor. One static Rust binary; no API keys, no network, fully
deterministic.

- **A real editor, headless** — layers, frames, tags, selections, locked
  palettes; generators for figures, walk/pose cycles, autotile terrain,
  9-slice panels and particle FX.
- **An eye, not just a hand** — critique, palette, silhouette, animation and
  colour-blindness audits turn "does it look right?" into numbers an agent
  acts on; `doc_set_audit` judges a whole asset set as one game.
- **Game-ready out of the box** — spritesheets with pivots/hitboxes/tags,
  GIF/APNG, texture atlases, Tiled tilesets, engine-standard JSON.

## Quickstart

```sh
curl -fsSL https://marmikshah.github.io/atelier/install.sh | sh
```

The installer registers atelier with your MCP client (stdio or a background
HTTP daemon). Restart your session,
then ask your agent for art — *"draw me a blinking cat sprite and export it
as a GIF"*:

```
doc_create → paint → doc_look (look!) → fix → doc_export op=anim
```

Prebuilt binaries: macOS (Apple Silicon), Linux x86_64, Windows
([releases](https://github.com/marmikshah/atelier/releases/latest));
anything else: `cargo install --path .`. Documents live under `~/.atelier`.

## Recipes

Every document is an ordered sequence of tool calls, so art is a **recipe** —
a JSON file that replays byte-identically and doubles as an integration test:

```sh
atelier replay docs/examples/invader-march.json --home /tmp/atelier-demo
```

## More

- [marmikshah.github.io/atelier](https://marmikshah.github.io/atelier/) —
  the benchmark: different models, identical briefs, the same studio in
  different hands.
- [docs/TOOLS.md](docs/TOOLS.md) — the complete tool reference (30-tool core
  profile by default; `ATELIER_PROFILE=full` for all 75).
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — crate layout.
- [CHANGELOG.md](CHANGELOG.md) — release notes.

## A personal note

atelier started as an experiment with one question behind it: can AI agents
build art that is genuinely good enough to ship in a game?

Every line of code here was written by AI — Claude Opus 4.8 and Fable 5 did
the heavy lifting, with Kimi 2.6 and Minimax 2.7 pitching in. I didn't write a
single line myself. My part was direction: holding the project to the same
practices and standards I use in the projects where I *do* still write the
code.

This is an ongoing experiment. As time allows, I'll keep running the
benchmark against other model families — GPT, Gemini, and whatever else looks
promising — and trying different designs to see how each of them holds a
brush.

If atelier helps you in any way — as a tool, a reference, or just a kick-start
on your own game-design journey — that makes me genuinely happy. The tokens
are already spent; the least they can do is be useful to you too.

### ⚠️ Notice — versions below 1.0.0

I intend to follow [SemVer](https://semver.org), but be realistic about what
this project is: AI-generated code. Diffs are large, and every 0.x release
will likely contain breaking changes despite my best intentions — which is
exactly what 0.x means.

Once I'm confident the tool has proven itself, I will cut a **1.0.0** release —
that is the point where I start reviewing the code in detail and contributing
to it directly. Until then, assume that anything below 1.0.0 has **not** been
fully reviewed by me and may contain bugs or security issues I haven't caught.

**Use at your own risk.**

## License

[MIT](LICENSE).
