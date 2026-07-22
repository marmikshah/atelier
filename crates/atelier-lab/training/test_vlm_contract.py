#!/usr/bin/env python3
"""Dependency-free tests for the model-facing prompt and JSON contracts."""

import json
import unittest

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


if __name__ == "__main__":
    unittest.main()
