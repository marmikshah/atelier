# Example recipes

Four authored JSON recipes of real tool calls that atelier replays
byte-identically. They double as the brand art and the integration tests
(`make test` replays them all). `atelier recipe compact` can turn any of them
into compact JSONL v2 without changing a call.

Replay one into a throwaway store:

```sh
atelier replay docs/examples/invader-march.json --home /tmp/demo
```

| recipe | what it demonstrates |
|---|---|
| `invader-march.json` | The cheapest loop possible: a 16×12 crab invader, one copied frame with a leg-swap edit, a pingpong tag → GIF. Two frames that differ only in the legs. |
| `pong-loop.json` | Animation as pure geometry: a static field on one layer, a bouncing ball on its own, timing that reads as eased motion. |
| `water-tile.json` | A seamless 16×16 water tile, then a toroidal-shift flow loop — the wrap seam is zero by construction, verified with the seam audit. |
| `kamehameha.json` | The big one: 14 frames of layered character FX — camera-shake background, a distinct body pose per frame (anticipation, smear, recoil), a charge orb that grows into a beam. |

Every step in every file carries its own narration (the `note` fields),
so reading a recipe top to bottom is a guided tour of the tool surface.
