#!/usr/bin/env python3
"""Run a bounded Atelier VLM train + validation cycle with one command."""

import argparse
import subprocess
import sys
from pathlib import Path

from train_vlm import load_config
from vlm_data import load_jsonl


SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parents[2]


def default_config(task):
    return SCRIPT_DIR / "configs" / f"{task}-qwen3-vl-2b-lora.json"


def resolve_input(path, label, directory=False):
    path = Path(path).expanduser()
    candidates = [path] if path.is_absolute() else [Path.cwd() / path, REPO_ROOT / path]
    predicate = Path.is_dir if directory else Path.is_file
    for candidate in candidates:
        if predicate(candidate):
            return candidate.resolve()
    tried = ", ".join(str(candidate) for candidate in candidates)
    kind = "directory" if directory else "file"
    raise ValueError(f"{label} {kind} not found; tried: {tried}")


def positive(value, name):
    if value <= 0:
        raise ValueError(f"{name} must be greater than zero")
    return value


def validation_count(dataset, split, limit):
    rows = load_jsonl(dataset)
    count = sum(row.get("task", {}).get("split") == split for row in rows)
    if count == 0:
        raise ValueError(f"dataset contains no rows for validation split {split!r}")
    return min(count, limit)


def run(command):
    print("+ " + " ".join(str(part) for part in command), flush=True)
    subprocess.run(command, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("task", choices=("generator", "critic"))
    parser.add_argument("dataset", type=Path)
    parser.add_argument("artifacts", type=Path)
    parser.add_argument(
        "config",
        nargs="?",
        type=Path,
        help="optional training config; defaults to the config beside this script",
    )
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--epochs", type=float, default=1)
    parser.add_argument("--train-limit", type=int, default=32)
    parser.add_argument("--val-limit", type=int, default=32)
    parser.add_argument("--expect-val-accuracy", type=float, default=0.0)
    parser.add_argument("--max-new-tokens", type=int, default=768)
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="validate bounded train/validation data without loading a model",
    )
    args = parser.parse_args()

    positive(args.epochs, "epochs")
    positive(args.train_limit, "train limit")
    positive(args.val_limit, "validation limit")
    positive(args.max_new_tokens, "max new tokens")
    if not 0 <= args.expect_val_accuracy <= 1:
        parser.error("expected validation accuracy must be between 0 and 1")

    dataset = resolve_input(args.dataset, "dataset")
    artifacts = resolve_input(args.artifacts, "artifacts", directory=True)
    config_path = resolve_input(args.config or default_config(args.task), "config")
    config = load_config(config_path)
    if config.get("task") not in (None, args.task):
        raise ValueError(
            f"config task {config['task']!r} does not match command task {args.task!r}"
        )
    eval_split = config.get("eval_split", "validation")
    val_rows = validation_count(dataset, eval_split, args.val_limit)
    output_dir = (
        args.output_dir.expanduser().resolve()
        if args.output_dir is not None
        else REPO_ROOT / "research" / "checkpoints" / f"{args.task}-quick"
    )

    train_command = [
        sys.executable,
        SCRIPT_DIR / "train_vlm.py",
        args.task,
        dataset,
        artifacts,
        config_path,
        "--output-dir",
        output_dir,
        "--epochs",
        str(args.epochs),
        "--train-limit",
        str(args.train_limit),
        "--eval-limit",
        str(args.val_limit),
    ]
    if args.dry_run:
        train_command.append("--dry-run")
    run(train_command)

    if args.dry_run:
        print(
            f"PASS: quick run is ready; validation split={eval_split} rows={val_rows}"
        )
        return 0

    eval_command = [
        sys.executable,
        SCRIPT_DIR / "eval_vlm.py",
        args.task,
        dataset,
        artifacts,
        "--base-model",
        config["model_id"],
        "--adapter",
        output_dir,
        "--quantization",
        config["quantization"],
        "--image-scale",
        str(config["image_scale"]),
        "--max-new-tokens",
        str(args.max_new_tokens),
        "--split",
        eval_split,
        "--limit",
        str(args.val_limit),
        "--expect-accuracy",
        str(args.expect_val_accuracy),
    ]
    run(eval_command)
    print(f"PASS: quick train + validation complete; adapter={output_dir}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.returncode)
    except (OSError, ValueError, KeyError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
