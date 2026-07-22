# Policy command protocol

`atelier-lab run-policy` and `atelier-lab run-batch` are provider-neutral. On
every turn they start the configured program without a shell, write one
`PolicyRequest` JSON object to stdin, close stdin, and read one
`PolicyResponse` JSON object from stdout.

The child inherits the operator's environment, allowing a wrapper to read its
own API key. Atelier never serializes environment variables or stderr into an
episode. Do not print diagnostics to stdout; stdout is exclusively the JSON
response channel.

## Request

```json
{
  "format_version": 1,
  "task": {"id": "item-001", "prompt": "A red potion", "...": "..."},
  "observation": {"doc_id": "...", "stage": "Specification", "...": "..."},
  "turn": 1,
  "max_turns": 40,
  "previous": null
}
```

`previous` is either compact transition feedback (accepted, compile error,
failed tool names) or the preceding provider/protocol error. This gives a
model enough information to repair invalid actions without exposing raw tool
payloads.

## Response

```json
{
  "format_version": 1,
  "action": {
    "action": {
      "PaintPatch": {
        "layer": 0,
        "x": 4,
        "y": 6,
        "width": 2,
        "height": 2,
        "grid": [1, 1, 1, 1]
      }
    },
    "intent": "Separate the focal shape from the torso"
  },
  "usage": {
    "input_tokens": 1200,
    "cached_input_tokens": 800,
    "output_tokens": 180,
    "reasoning_tokens": 90
  }
}
```

Usage is optional. The runner validates the protocol version and delegates all
action validation to the existing lab compiler.

## Local protocol check

The bundled example is deterministic and performs no network calls:

```sh
cargo run -p atelier-lab --bin atelier-lab -- \
  run-policy research/tasks.jsonl item-001 research/episodes \
  --policy python3 \
  --policy-arg crates/atelier-lab/policy/example_policy.py \
  --name deterministic-example
```

A real provider wrapper has the same stdin/stdout contract. It should translate
the request into its provider's prompt or structured-output call, report token
usage when available, and place credentials only in its environment.

## Resumable batches

`run-batch` accepts the same policy, budget, seed, and timeout options as
`run-policy`, plus task selection options:

```sh
cargo run -p atelier-lab --bin atelier-lab -- \
  run-batch crates/atelier-lab/tasks/development.jsonl research/episodes \
  --policy python3 \
  --policy-arg crates/atelier-lab/policy/example_policy.py \
  --name deterministic-example \
  --split development --limit 20 --repeats 1
```

Resume identity is `(task_id, policy name, seed)`. The batch manifest stores
the policy label and token usage, but never the child environment, stderr, or
policy arguments. Keep credentials in environment variables, not command-line
arguments. A completed key is skipped only after its reset, policy-call, and
finish provenance has been re-read from the referenced episode log.

## Trained checkpoint adapter

The custom generator adapter stays resident behind a loopback-only server;
`vlm_client.py` remains a small command-policy process and therefore needs no
special Rust integration:

```sh
python3 crates/atelier-lab/policy/vlm_server.py \
  --adapter research/checkpoints/generator-qwen3.5-4b-qlora

cargo run -p atelier-lab --bin atelier-lab -- \
  run-policy crates/atelier-lab/tasks/development.jsonl \
  development-character-001 research/vlm-episodes \
  --policy python3 \
  --policy-arg crates/atelier-lab/policy/vlm_client.py \
  --name generator-qwen3.5-4b-qlora
```

The server reconstructs the exact visual state from the indexed observation,
applies the same nearest-neighbour scale used for SFT, and requires the model
to return a versioned `PolicyResponse`. `ATELIER_VLM_URL` can point the client
at another trusted endpoint, but the supplied server has no authentication and
must not be exposed publicly.
