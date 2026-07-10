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
  <img src="docs/showcase/campfire-haiku.gif" width="96" alt="campfire flicker, drawn by Haiku 4.5">
  <img src="docs/showcase/coin-sonnet.gif" width="96" alt="spinning coin, drawn by Sonnet 5">
  <img src="docs/showcase/invader-opus.gif" width="96" alt="marching invader, drawn by Opus 4.8">
  <img src="docs/showcase/bounce-fable.gif" width="96" alt="bouncing slime, drawn by Fable 5">
</p>
<p align="center"><em>Campfire by Haiku 4.5 · coin by Sonnet 5 · invader by Opus 4.8 ·
slime by Fable 5 — different models, one studio, zero hand-editing.
More in the <a href="docs/SHOWCASE.md">showcase</a>.</em></p>

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
HTTP daemon with a live `/gallery` + `/playground`). Restart your session,
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

- [docs/SHOWCASE.md](docs/SHOWCASE.md) — different models, identical briefs:
  the same studio in different hands.
- [docs/TOOLS.md](docs/TOOLS.md) — the complete tool reference (~30-tool core
  profile by default; `ATELIER_PROFILE=full` for all 75).
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — crate layout.
- [CHANGELOG.md](CHANGELOG.md) — release notes.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.
