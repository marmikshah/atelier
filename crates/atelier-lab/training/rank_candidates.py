#!/usr/bin/env python3
"""Rank best-of-N sprite PNGs with a trained pairwise critic adapter."""

import argparse
import json
import sys
from pathlib import Path

from vlm_data import critic_messages, load_jsonl
from vlm_runtime import attach_images, generate_json, load_model


def model_preference(model, processor, task, first, second, max_new_tokens):
    row = {"task": task}
    messages = attach_images(critic_messages(row, include_target=False), [first, second])
    result = generate_json(model, processor, messages, "preference", max_new_tokens)
    preference = result["preference"]
    if preference not in ("candidate_a", "candidate_b", "tie"):
        raise ValueError(f"critic returned invalid preference {preference!r}")
    return preference


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("images", nargs="+", type=Path)
    parser.add_argument("--tasks", type=Path, required=True)
    parser.add_argument("--task-id", required=True)
    parser.add_argument("--base-model", default="Qwen/Qwen3-VL-2B-Instruct")
    parser.add_argument("--adapter", required=True)
    parser.add_argument("--quantization", choices=("none", "4bit"), default="4bit")
    parser.add_argument("--image-scale", type=int, default=8)
    parser.add_argument("--max-new-tokens", type=int, default=64)
    args = parser.parse_args()
    if len(args.images) < 2:
        parser.error("best-of-N ranking needs at least two images")
    tasks = load_jsonl(args.tasks)
    task = next((task for task in tasks if task["id"] == args.task_id), None)
    if task is None:
        raise ValueError(f"task {args.task_id!r} not found in {args.tasks}")

    try:
        from PIL import Image
    except ImportError as error:
        raise ValueError("Pillow is required for critic ranking") from error
    images = []
    for path in args.images:
        image = Image.open(path).convert("RGB")
        if args.image_scale != 1:
            image = image.resize(
                (image.width * args.image_scale, image.height * args.image_scale),
                Image.Resampling.NEAREST,
            )
        images.append(image)
    model, processor = load_model(args.base_model, args.adapter, args.quantization)
    scores = [0.0] * len(images)
    comparisons = []
    for left in range(len(images)):
        for right in range(left + 1, len(images)):
            forward = model_preference(
                model, processor, task, images[left], images[right], args.max_new_tokens
            )
            reverse = model_preference(
                model, processor, task, images[right], images[left], args.max_new_tokens
            )
            votes = []
            votes.append(left if forward == "candidate_a" else right if forward == "candidate_b" else None)
            votes.append(right if reverse == "candidate_a" else left if reverse == "candidate_b" else None)
            for winner in votes:
                if winner is None:
                    scores[left] += 0.5
                    scores[right] += 0.5
                else:
                    scores[winner] += 1.0
            comparisons.append(
                {
                    "pair": [left, right],
                    "forward": forward,
                    "reverse": reverse,
                }
            )
    order = sorted(range(len(images)), key=lambda index: (-scores[index], index))
    print(
        json.dumps(
            {
                "task_id": args.task_id,
                "ranking": [
                    {"rank": rank + 1, "image": str(args.images[index]), "score": scores[index]}
                    for rank, index in enumerate(order)
                ],
                "comparisons": comparisons,
            },
            separators=(",", ":"),
        )
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
