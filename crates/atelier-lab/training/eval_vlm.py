#!/usr/bin/env python3
"""Exact-contract evaluation for trained Atelier generator or critic adapters."""

import argparse
import json
import sys
from pathlib import Path

from vlm_data import (
    critic_messages,
    generator_messages,
    load_jsonl,
    validate_critic_row,
    validate_generator_row,
)
from vlm_runtime import attach_images, generate_json, load_model


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("task", choices=("generator", "critic"))
    parser.add_argument("dataset", type=Path)
    parser.add_argument("artifacts", type=Path)
    parser.add_argument("--base-model", default="Qwen/Qwen3-VL-2B-Instruct")
    parser.add_argument("--adapter", required=True)
    parser.add_argument("--quantization", choices=("none", "4bit"), default="4bit")
    parser.add_argument("--image-scale", type=int, default=8)
    parser.add_argument("--max-new-tokens", type=int, default=768)
    parser.add_argument("--limit", type=int)
    parser.add_argument("--expect-accuracy", type=float, default=0.95)
    args = parser.parse_args()
    if not 0 <= args.expect_accuracy <= 1:
        parser.error("expected accuracy must be between 0 and 1")

    try:
        from PIL import Image
    except ImportError as error:
        raise ValueError("Pillow is required for VLM evaluation") from error
    rows = load_jsonl(args.dataset)
    if args.limit is not None:
        rows = rows[: args.limit]
    model, processor = load_model(args.base_model, args.adapter, args.quantization)
    correct = 0
    for row in rows:
        if args.task == "generator":
            paths, _ = validate_generator_row(row, args.artifacts)
            messages = generator_messages(row, include_target=False)
            expected = {"format_version": 1, "action": row["action"]}
            required = "action"
        else:
            paths, _ = validate_critic_row(row, args.artifacts)
            messages = critic_messages(row, include_target=False)
            expected = {"preference": row["overall"]}
            required = "preference"
        images = []
        for path in paths:
            image = Image.open(path).convert("RGB")
            if args.image_scale != 1:
                image = image.resize(
                    (image.width * args.image_scale, image.height * args.image_scale),
                    Image.Resampling.NEAREST,
                )
            images.append(image)
        predicted = generate_json(
            model,
            processor,
            attach_images(messages, images),
            required,
            args.max_new_tokens,
        )
        correct += int(predicted == expected)
    accuracy = correct / len(rows)
    print(f"task={args.task} examples={len(rows)} exact_accuracy={accuracy:.3f}")
    if accuracy < args.expect_accuracy:
        print(
            f"FAILED: expected exact accuracy >= {args.expect_accuracy:.3f}",
            file=sys.stderr,
        )
        return 1
    print("PASS: adapter meets the exact-contract accuracy gate")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
