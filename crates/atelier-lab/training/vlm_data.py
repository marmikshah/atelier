#!/usr/bin/env python3
"""Shared, dependency-light VLM dataset and prompt contracts."""

import json
from pathlib import Path


GENERATOR_SYSTEM = (
    "You are Atelier's vision-conditioned pixel-art action policy. "
    "Return exactly one compact JSON PolicyResponse and no prose. "
    "Use only palette indices and the typed Atelier action schema."
)

CRITIC_SYSTEM = (
    "You are Atelier's blinded pixel-art critic. Compare requirement adherence, "
    "native-size readability, silhouette, clusters, palette, lighting, and polish. "
    "Return only JSON with preference candidate_a, candidate_b, or tie."
)


def load_jsonl(path):
    rows = []
    for line_no, line in enumerate(Path(path).read_text().splitlines(), 1):
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError as error:
            raise ValueError(f"{path}:{line_no}: {error}") from error
    if not rows:
        raise ValueError(f"{path}: dataset is empty")
    return rows


def artifact_path(root, reference):
    digest = reference["sha256"]
    if len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
        raise ValueError(f"invalid artifact hash {digest!r}")
    path = Path(root) / "sha256" / digest[:2] / digest
    if not path.is_file():
        raise ValueError(f"missing artifact {path}")
    return path


def task_text(task):
    constraints = {
        "prompt": task["prompt"],
        "category": task["category"],
        "canvas": [task["width"], task["height"]],
        "max_colors": task["max_colors"],
        "must_include": task.get("must_include", []),
        "must_avoid": task.get("must_avoid", []),
        "style": task["style"],
    }
    return json.dumps(constraints, separators=(",", ":"), sort_keys=True)


def generator_messages(row, include_target=True):
    context = row["context"]
    user_text = (
        "TASK="
        + task_text(row["task"])
        + "\nSTATE="
        + json.dumps(context, separators=(",", ":"), sort_keys=True)
        + "\nChoose the single best next Atelier action."
    )
    messages = [
        {"role": "system", "content": GENERATOR_SYSTEM},
        {
            "role": "user",
            "content": [
                {"type": "image"},
                {"type": "text", "text": user_text},
            ],
        },
    ]
    if include_target:
        target = {"format_version": 1, "action": row["action"]}
        messages.append(
            {
                "role": "assistant",
                "content": json.dumps(target, separators=(",", ":")),
            }
        )
    return messages


def critic_messages(row, include_target=True):
    user_text = (
        "TASK="
        + task_text(row["task"])
        + "\nThe first image is candidate_a and the second is candidate_b. "
        "Choose the better pixel-art result without inferring model identity."
    )
    messages = [
        {"role": "system", "content": CRITIC_SYSTEM},
        {
            "role": "user",
            "content": [
                {"type": "image"},
                {"type": "image"},
                {"type": "text", "text": user_text},
            ],
        },
    ]
    if include_target:
        messages.append(
            {
                "role": "assistant",
                "content": json.dumps(
                    {"preference": row["overall"]}, separators=(",", ":")
                ),
            }
        )
    return messages


def validate_generator_row(row, artifacts):
    if row.get("format_version") != 1:
        raise ValueError(f"generator row {row.get('id')!r}: unsupported format")
    if row["task"]["split"] == "frozen_test":
        raise ValueError("refusing generator training data from frozen_test")
    path = artifact_path(artifacts, row["image"])
    messages = generator_messages(row)
    return [path], messages


def validate_critic_row(row, artifacts):
    if row.get("format_version") != 1:
        raise ValueError(f"critic row {row.get('id')!r}: unsupported format")
    if row["task"]["split"] == "frozen_test":
        raise ValueError("refusing critic training data from frozen_test")
    if row["overall"] not in ("candidate_a", "candidate_b", "tie"):
        raise ValueError(f"invalid critic preference {row['overall']!r}")
    paths = [
        artifact_path(artifacts, row["candidate_a"]["native"]),
        artifact_path(artifacts, row["candidate_b"]["native"]),
    ]
    return paths, critic_messages(row)


def extract_json_object(text, required_key):
    decoder = json.JSONDecoder()
    for index, char in enumerate(text):
        if char != "{":
            continue
        try:
            value, _ = decoder.raw_decode(text[index:])
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict) and required_key in value:
            return value
    raise ValueError(f"model output contains no JSON object with {required_key!r}")
