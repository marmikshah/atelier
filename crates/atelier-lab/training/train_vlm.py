#!/usr/bin/env python3
"""LoRA/QLoRA trainer for Atelier generator and pairwise critic VLM rows."""

import argparse
import json
import sys
from pathlib import Path

from vlm_data import (
    load_jsonl,
    validate_critic_row,
    validate_generator_row,
)


REQUIRED_CONFIG = {
    "model_id",
    "model_class",
    "output_dir",
    "quantization",
    "image_scale",
    "epochs",
    "learning_rate",
    "batch_size",
    "gradient_accumulation",
    "lora_r",
    "lora_alpha",
    "lora_dropout",
}


def load_config(path):
    config = json.loads(Path(path).read_text())
    missing = sorted(REQUIRED_CONFIG - config.keys())
    if missing:
        raise ValueError(f"{path}: missing config keys {missing}")
    if config["quantization"] not in ("none", "4bit"):
        raise ValueError("quantization must be none or 4bit")
    if config["image_scale"] < 1:
        raise ValueError("image_scale must be at least 1")
    return config


def validate_rows(task, rows, artifacts):
    validator = validate_generator_row if task == "generator" else validate_critic_row
    checked = []
    for row in rows:
        paths, messages = validator(row, artifacts)
        for path in paths:
            if path.read_bytes()[:8] != b"\x89PNG\r\n\x1a\n":
                raise ValueError(f"{path}: expected PNG artifact")
        checked.append((row, paths, messages))
    return checked


def split_rows(checked, train_split, eval_split):
    train = [item for item in checked if item[0]["task"]["split"] == train_split]
    evaluate = [item for item in checked if item[0]["task"]["split"] == eval_split]
    if not train:
        raise ValueError(f"no rows use configured train split {train_split!r}")
    return train, evaluate


def positive(value, name):
    if value is not None and value <= 0:
        raise ValueError(f"{name} must be greater than zero")
    return value


def materialize(items, image_scale):
    from PIL import Image

    records = []
    for _, paths, messages in items:
        images = []
        for path in paths:
            image = Image.open(path).convert("RGB")
            if image_scale != 1:
                image = image.resize(
                    (image.width * image_scale, image.height * image_scale),
                    Image.Resampling.NEAREST,
                )
            images.append(image)
        records.append({"images": images, "messages": messages})
    return records


def train(config, train_items, eval_items):
    try:
        import torch
        import transformers
        from datasets import Dataset
        from peft import LoraConfig, prepare_model_for_kbit_training
        from transformers import AutoProcessor, BitsAndBytesConfig
        from trl import SFTConfig, SFTTrainer
    except ImportError as error:
        raise ValueError(
            "training dependencies missing; install requirements-vlm.txt"
        ) from error

    dtype = torch.bfloat16 if config.get("bf16", True) else torch.float16
    model_kwargs = {"dtype": dtype, "device_map": "auto"}
    if config["quantization"] == "4bit":
        if not torch.cuda.is_available():
            raise ValueError(
                "the checked-in 4bit config requires CUDA; use quantization=none "
                "for a non-CUDA training experiment"
            )
        model_kwargs["quantization_config"] = BitsAndBytesConfig(
            load_in_4bit=True,
            bnb_4bit_quant_type="nf4",
            bnb_4bit_compute_dtype=dtype,
            bnb_4bit_use_double_quant=True,
        )
    model_class = getattr(transformers, config["model_class"], None)
    if model_class is None:
        raise ValueError(f"unknown transformers model class {config['model_class']!r}")
    model = model_class.from_pretrained(config["model_id"], **model_kwargs)
    if config["quantization"] == "4bit":
        model = prepare_model_for_kbit_training(
            model, use_gradient_checkpointing=config.get("gradient_checkpointing", True)
        )
    model.config.use_cache = False
    processor = AutoProcessor.from_pretrained(config["model_id"])
    peft_config = LoraConfig(
        r=config["lora_r"],
        lora_alpha=config["lora_alpha"],
        lora_dropout=config["lora_dropout"],
        target_modules="all-linear",
        bias="none",
        task_type="CAUSAL_LM",
    )
    train_dataset = Dataset.from_list(materialize(train_items, config["image_scale"]))
    eval_dataset = (
        Dataset.from_list(materialize(eval_items, config["image_scale"]))
        if eval_items
        else None
    )
    args = SFTConfig(
        output_dir=config["output_dir"],
        num_train_epochs=config["epochs"],
        learning_rate=config["learning_rate"],
        per_device_train_batch_size=config["batch_size"],
        per_device_eval_batch_size=config["batch_size"],
        gradient_accumulation_steps=config["gradient_accumulation"],
        gradient_checkpointing=config.get("gradient_checkpointing", True),
        bf16=config.get("bf16", True),
        fp16=not config.get("bf16", True),
        logging_steps=config.get("logging_steps", 1),
        save_strategy="epoch",
        eval_strategy="epoch" if eval_dataset is not None else "no",
        report_to="none",
        max_length=None,
        assistant_only_loss=config.get("assistant_only_loss", True),
        remove_unused_columns=False,
    )
    trainer = SFTTrainer(
        model=model,
        args=args,
        train_dataset=train_dataset,
        eval_dataset=eval_dataset,
        processing_class=processor,
        peft_config=peft_config,
    )
    trainer.train()
    trainer.save_model(config["output_dir"])
    processor.save_pretrained(config["output_dir"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("task", choices=("generator", "critic"))
    parser.add_argument("dataset", type=Path)
    parser.add_argument("artifacts", type=Path)
    parser.add_argument("config", type=Path)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--epochs", type=float)
    parser.add_argument("--train-limit", type=int)
    parser.add_argument("--eval-limit", type=int)
    args = parser.parse_args()

    config = load_config(args.config)
    if config.get("task") not in (None, args.task):
        raise ValueError(
            f"config task {config['task']!r} does not match command task {args.task!r}"
        )
    positive(args.epochs, "epochs")
    positive(args.train_limit, "train limit")
    positive(args.eval_limit, "eval limit")
    if args.output_dir is not None:
        config["output_dir"] = str(args.output_dir)
    if args.epochs is not None:
        config["epochs"] = args.epochs
    rows = load_jsonl(args.dataset)
    checked = validate_rows(args.task, rows, args.artifacts)
    train_rows, eval_rows = split_rows(
        checked,
        config.get("train_split", "development"),
        config.get("eval_split", "validation"),
    )
    if args.train_limit is not None:
        train_rows = train_rows[: args.train_limit]
    if args.eval_limit is not None:
        eval_rows = eval_rows[: args.eval_limit]
    print(
        f"task={args.task} rows={len(rows)} train={len(train_rows)} "
        f"eval={len(eval_rows)} epochs={config['epochs']} "
        f"model={config['model_id']} output={config['output_dir']}"
    )
    if args.dry_run:
        print("PASS: dataset, artifacts, messages, targets, and config are valid")
        return 0
    train(config, train_rows, eval_rows)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
