#!/usr/bin/env python3
"""Dependency-free tests for the model-facing prompt and JSON contracts."""

import json
import unittest
from unittest.mock import patch

from eval_vlm import select_rows
from quick_vlm import default_config, resolve_input, validation_count
from vlm_data import critic_messages, extract_json_object, generator_messages
from vlm_runtime import attach_images


TASK = {
    "prompt": "A chipped red potion",
    "category": "item",
    "width": 32,
    "height": 32,
    "max_colors": 8,
    "must_include": ["chip"],
    "must_avoid": ["text"],
    "style": {"outline": "selective", "lighting": "upper-left", "detail": "medium"},
}


class ContractTests(unittest.TestCase):
    def test_generator_target_is_a_policy_response(self):
        row = {
            "task": TASK,
            "context": {
                "stage": "Silhouette",
                "palette": [[0, 0, 0, 255]],
                "layer_count": 1,
                "recent_actions": [],
                "integrity": {"on_palette": True, "palette_within_budget": True, "opaque_pixels": 0},
            },
            "action": {"action": "Finish"},
        }
        messages = generator_messages(row)
        self.assertEqual(json.loads(messages[-1]["content"])["format_version"], 1)
        self.assertEqual(json.loads(messages[-1]["content"])["action"], row["action"])

    def test_critic_uses_two_ordered_image_slots(self):
        messages = critic_messages({"task": TASK}, include_target=False)
        attached = attach_images(messages, ["first", "second"])
        slots = [
            item["image"]
            for message in attached
            if isinstance(message["content"], list)
            for item in message["content"]
            if item["type"] == "image"
        ]
        self.assertEqual(slots, ["first", "second"])

    def test_json_extraction_ignores_wrapping_text(self):
        value = extract_json_object(
            "```json\n{\"format_version\":1,\"action\":{\"action\":\"Finish\"}}\n```",
            "action",
        )
        self.assertEqual(value["action"]["action"], "Finish")

    def test_evaluation_selects_validation_before_limit(self):
        rows = [
            {"task": {"split": "development"}, "id": "train"},
            {"task": {"split": "validation"}, "id": "val-1"},
            {"task": {"split": "validation"}, "id": "val-2"},
        ]
        selected = select_rows(rows, "validation", 1)
        self.assertEqual([row["id"] for row in selected], ["val-1"])

    def test_evaluation_rejects_an_empty_split(self):
        with self.assertRaisesRegex(ValueError, "no evaluation rows"):
            select_rows([{"task": {"split": "development"}}], "validation", 1)

    def test_quick_run_requires_and_bounds_validation_rows(self):
        rows = [
            {"task": {"split": "development"}},
            {"task": {"split": "validation"}},
            {"task": {"split": "validation"}},
        ]
        with patch("quick_vlm.load_jsonl", return_value=rows):
            self.assertEqual(validation_count("unused.jsonl", "validation", 1), 1)
        with patch(
            "quick_vlm.load_jsonl",
            return_value=[{"task": {"split": "development"}}],
        ):
            with self.assertRaisesRegex(ValueError, "no rows for validation split"):
                validation_count("unused.jsonl", "validation", 1)

    def test_quick_run_has_a_bundled_default_config(self):
        config = default_config("generator")
        self.assertEqual(resolve_input(config, "config"), config.resolve())

    def test_quick_run_reports_every_attempted_input_path(self):
        with self.assertRaisesRegex(ValueError, "dataset file not found; tried:"):
            resolve_input("definitely-missing.jsonl", "dataset")


if __name__ == "__main__":
    unittest.main()
