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


def select_rows(rows, split=None, limit=None):
    if split is not None:
        rows = [row for row in rows if row["task"]["split"] == split]
    if limit is not None:
        rows = rows[:limit]
    if not rows:
        label = f" for split {split!r}" if split is not None else ""
        raise ValueError(f"dataset contains no evaluation rows{label}")
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("task", choices=("generator", "critic"))
    parser.add_argument("dataset", type=Path)
    parser.add_argument("artifacts", type=Path)
    parser.add_argument("--base-model", default="Qwen/Qwen3.5-4B")
    parser.add_argument("--adapter", required=True)
    parser.add_argument("--quantization", choices=("none", "4bit"), default="4bit")
    parser.add_argument("--image-scale", type=int, default=8)
    parser.add_argument("--max-new-tokens", type=int, default=768)
    parser.add_argument("--limit", type=int)
    parser.add_argument("--split", choices=("development", "validation"))
    parser.add_argument("--expect-accuracy", type=float, default=0.95)
    args = parser.parse_args()
    if not 0 <= args.expect_accuracy <= 1:
        parser.error("expected accuracy must be between 0 and 1")
    if args.limit is not None and args.limit <= 0:
        parser.error("limit must be greater than zero")

    try:
        from PIL import Image
    except ImportError as error:
        raise ValueError("Pillow is required for VLM evaluation") from error
    rows = select_rows(load_jsonl(args.dataset), args.split, args.limit)
    validator = validate_generator_row if args.task == "generator" else validate_critic_row
    checked = []
    for row in rows:
        paths, _ = validator(row, args.artifacts)
        checked.append((row, paths))
    model, processor = load_model(args.base_model, args.adapter, args.quantization)
    correct = 0
    invalid = 0
    for index, (row, paths) in enumerate(checked, 1):
        if args.task == "generator":
            messages = generator_messages(row, include_target=False)
            expected = {"format_version": 1, "action": row["action"]}
            required = "action"
        else:
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
        try:
            predicted = generate_json(
                model,
                processor,
                attach_images(messages, images),
                required,
                args.max_new_tokens,
            )
        except ValueError as error:
            invalid += 1
            print(f"example={index} invalid_output={error}", file=sys.stderr)
            continue
        correct += int(predicted == expected)
    accuracy = correct / len(rows)
    split = args.split or "all"
    print(
        f"task={args.task} split={split} examples={len(rows)} invalid={invalid} "
        f"exact_accuracy={accuracy:.3f}"
    )
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
