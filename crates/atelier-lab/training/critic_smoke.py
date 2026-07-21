#!/usr/bin/env python3
"""Dependency-free pairwise critic plumbing check.

This intentionally tiny linear ranker should memorize a small decisive export.
It is a data/label/image gate, not the architecture proposed in lab.md.
"""

import argparse
import json
import math
import random
import struct
import sys
import zlib
from pathlib import Path


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    return a if pa <= pb and pa <= pc else b if pb <= pc else c


def read_png(path):
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path}: not a PNG")
    pos, payload, width, height, color_type = 8, bytearray(), None, None, None
    while pos < len(data):
        size = struct.unpack(">I", data[pos : pos + 4])[0]
        kind = data[pos + 4 : pos + 8]
        body = data[pos + 8 : pos + 8 + size]
        pos += 12 + size
        if kind == b"IHDR":
            width, height, depth, color_type, compression, filtering, interlace = struct.unpack(
                ">IIBBBBB", body
            )
            if depth != 8 or compression or filtering or interlace:
                raise ValueError(f"{path}: smoke trainer needs non-interlaced 8-bit PNG")
        elif kind == b"IDAT":
            payload.extend(body)
        elif kind == b"IEND":
            break
    channels = {0: 1, 2: 3, 6: 4}.get(color_type)
    if channels is None:
        raise ValueError(f"{path}: unsupported PNG color type {color_type}")
    raw, stride = zlib.decompress(payload), width * channels
    rows, offset, previous = [], 0, bytearray(stride)
    for _ in range(height):
        filter_type = raw[offset]
        source = raw[offset + 1 : offset + 1 + stride]
        offset += stride + 1
        row = bytearray(stride)
        for x, value in enumerate(source):
            left = row[x - channels] if x >= channels else 0
            above = previous[x]
            upper_left = previous[x - channels] if x >= channels else 0
            if filter_type == 0:
                decoded = value
            elif filter_type == 1:
                decoded = value + left
            elif filter_type == 2:
                decoded = value + above
            elif filter_type == 3:
                decoded = value + ((left + above) // 2)
            elif filter_type == 4:
                decoded = value + paeth(left, above, upper_left)
            else:
                raise ValueError(f"{path}: unsupported PNG filter {filter_type}")
            row[x] = decoded & 255
        rows.append(row)
        previous = row
    return width, height, channels, rows


def raster_features(path):
    width, height, channels, rows = read_png(path)
    if (width, height) != (32, 32):
        raise ValueError(f"{path}: expected 32x32, got {width}x{height}")
    features = []
    for row in rows:
        for x in range(width):
            pixel = row[x * channels : (x + 1) * channels]
            if channels == 1:
                r = g = b = pixel[0]
                alpha = 255
            elif channels == 3:
                r, g, b = pixel
                alpha = 255
            else:
                r, g, b, alpha = pixel
            luminance = (77 * r + 150 * g + 29 * b) / (255.0 * 256.0)
            features.extend((luminance * (alpha / 255.0), alpha / 255.0))
    return features


def artifact_path(root, candidate):
    digest = candidate["native"]["sha256"]
    if len(digest) != 64:
        raise ValueError(f"invalid artifact hash {digest!r}")
    return root / "sha256" / digest[:2] / digest


def load_examples(dataset, artifacts):
    cache, examples, ties = {}, [], 0
    for line_no, line in enumerate(dataset.read_text().splitlines(), 1):
        if not line.strip():
            continue
        row = json.loads(line)
        if row.get("task", {}).get("split") == "frozen_test":
            raise ValueError(
                f"{dataset}:{line_no}: refusing to train on frozen_test; "
                "evaluate that split with a trained critic instead"
            )
        label = row["overall"]
        if label == "tie":
            ties += 1
            continue
        if label not in ("candidate_a", "candidate_b"):
            raise ValueError(f"{dataset}:{line_no}: invalid overall label {label!r}")
        vectors = []
        for side in ("candidate_a", "candidate_b"):
            path = artifact_path(artifacts, row[side])
            if path not in cache:
                cache[path] = raster_features(path)
            vectors.append(cache[path])
        difference = [a - b for a, b in zip(*vectors)]
        examples.append((difference, 1.0 if label == "candidate_a" else -1.0))
    return examples, ties


def score(weights, features):
    return sum(w * x for w, x in zip(weights, features))


def train(examples, epochs, learning_rate, seed):
    weights = [0.0] * len(examples[0][0])
    order = list(range(len(examples)))
    rng = random.Random(seed)
    for _ in range(epochs):
        rng.shuffle(order)
        for index in order:
            features, label = examples[index]
            margin = label * score(weights, features)
            factor = (
                label
                if margin < -40
                else 0.0
                if margin > 40
                else label / (1.0 + math.exp(margin))
            )
            for i, value in enumerate(features):
                weights[i] += learning_rate * factor * value
    return weights


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dataset", type=Path, help="critic JSONL from export-critic")
    parser.add_argument("artifacts", type=Path, help="bundle artifacts directory")
    parser.add_argument("--epochs", type=int, default=250)
    parser.add_argument("--learning-rate", type=float, default=0.1)
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument("--expect-accuracy", type=float, default=0.95)
    args = parser.parse_args()

    examples, ties = load_examples(args.dataset, args.artifacts)
    if not examples:
        raise ValueError("no decisive (non-tie) examples to train")
    weights = train(examples, args.epochs, args.learning_rate, args.seed)
    correct = sum(1 for features, label in examples if label * score(weights, features) > 0)
    accuracy = correct / len(examples)
    print(
        f"decisive={len(examples)} ties_skipped={ties} "
        f"train_accuracy={accuracy:.3f} epochs={args.epochs}"
    )
    if accuracy < args.expect_accuracy:
        print(
            f"FAILED: expected overfit accuracy >= {args.expect_accuracy:.3f}; "
            "check labels, duplicate images, and artifact plumbing",
            file=sys.stderr,
        )
        return 1
    print("PASS: the tiny raster ranker can overfit this export")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
