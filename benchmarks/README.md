# atelier benchmark

Run the same animation brief against different models and record what each does —
tool calls, tokens, wall-clock, and the exported GIF. Everything the model needs
is atelier's MCP tools; the only variable is the model.

## Tasks

Eight briefs, one per game-object category, each a **1-second loop at 10 FPS
(10 frames)** on a transparent background:

| brief | category |
|-------|----------|
| `person`  | character |
| `cat`     | animal |
| `car`     | vehicle |
| `alien`   | creature |
| `ball`    | prop / physics |
| `potion`  | item / pickup |
| `slash`   | effect / VFX |
| `torch`   | environment |

The brief text is the exact input; keep it fixed so runs stay comparable.

## Run

```sh
make release                                   # build the atelier binary once
pip install "mcp>=1.0" "openai>=1.40"

# any OpenAI-compatible endpoint (OpenAI by default)
export OPENAI_API_KEY=...
python benchmarks/run_task.py --model gpt-4o --task benchmarks/briefs/car.txt
```

Transcript is written to `benchmarks/runs/<model>/<task>.json` (gitignored).
Swap `--model` / `--base-url` to compare models on an identical brief.

## Reproducibility

Each run gets an isolated, freshly-wiped `ATELIER_HOME`; requests default to
temperature 0 with a fixed seed and retry with backoff on transient errors. The
transcript pins every input (model, endpoint, prompt, tools, temperature, seed)
plus tokens and duration. Endpoints don't guarantee bit-identical LLM output, but
the recorded tool-call sequence replays byte-identically via `atelier replay` —
the art is the deterministic part.

See `run_task.py --help` for all flags.
