#!/usr/bin/env python3
"""Deterministic protocol example, not an artistic model.

Reads one PolicyRequest from stdin and writes one PolicyResponse to stdout.
It proves the command boundary end to end without network access or secrets.
"""

import json
import sys


request = json.load(sys.stdin)
observation = request["observation"]

if not observation["palette"]:
    action = {
        "action": {
            "SetPalette": {
                "colors": [
                    [35, 22, 28, 255],
                    [172, 42, 55, 255],
                    [238, 115, 104, 255],
                    [247, 218, 174, 255],
                ]
            }
        },
        "intent": "Set a compact red potion palette",
    }
elif observation["stage"] == "Specification":
    action = {"action": "AdvanceStage", "intent": "Begin the silhouette"}
elif observation["integrity"]["opaque_pixels"] == 0:
    # A deliberately crude 8x12 bottle block. A real wrapper replaces this
    # branch with one model call that emits the same typed Action structure.
    action = {
        "action": {
            "PaintPatch": {
                "layer": 0,
                "x": 12,
                "y": 10,
                "width": 8,
                "height": 12,
                "grid": [1] * 96,
            }
        },
        "intent": "Block the bottle silhouette",
    }
else:
    action = {"action": "Finish", "intent": "Close the protocol example"}

json.dump({"format_version": 1, "action": action}, sys.stdout, separators=(",", ":"))
sys.stdout.write("\n")
