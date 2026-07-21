# Policy command protocol

`atelier-lab run-policy` is provider-neutral. On every turn it starts the
configured program without a shell, writes one `PolicyRequest` JSON object to
stdin, closes stdin, and reads one `PolicyResponse` JSON object from stdout.

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
