#!/usr/bin/env python3
"""Persistent local HTTP server for a trained Atelier generator adapter."""

import argparse
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

TRAINING = Path(__file__).resolve().parents[1] / "training"
sys.path.insert(0, str(TRAINING))

from vlm_data import generator_messages  # noqa: E402
from vlm_runtime import attach_images, generate_json, load_model  # noqa: E402


def observation_image(observation, scale):
    try:
        from PIL import Image
    except ImportError as error:
        raise ValueError("Pillow is required by the VLM policy server") from error

    width, height = observation["width"], observation["height"]
    cells = width * height
    palette = observation["palette"]
    canvas = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    for layer in observation["layers"]:
        if not layer.get("visible", True):
            continue
        indices = layer["indices"]
        if len(indices) != cells:
            raise ValueError(f"layer {layer['index']} has {len(indices)} cells, expected {cells}")
        opacity = layer.get("opacity", 255)
        pixels = []
        for index in indices:
            if index is None:
                pixels.append((0, 0, 0, 0))
                continue
            if not 0 <= index < len(palette):
                raise ValueError(f"layer {layer['index']} has invalid palette index {index}")
            red, green, blue, alpha = palette[index]
            pixels.append((red, green, blue, (alpha * opacity + 127) // 255))
        overlay = Image.new("RGBA", (width, height))
        overlay.putdata(pixels)
        canvas = Image.alpha_composite(canvas, overlay)
    if scale != 1:
        canvas = canvas.resize(
            (width * scale, height * scale), Image.Resampling.NEAREST
        )
    return canvas.convert("RGB")


def request_messages(request):
    observation = request["observation"]
    row = {
        "task": request["task"],
        "context": {
            "stage": observation["stage"],
            "palette": observation["palette"],
            "layer_count": len(observation["layers"]),
            "recent_actions": observation.get("recent_actions", []),
            "integrity": observation["integrity"],
        },
    }
    return generator_messages(row, include_target=False)


class GeneratorService:
    def __init__(self, args):
        self.model, self.processor = load_model(
            args.base_model, args.adapter, args.quantization
        )
        self.image_scale = args.image_scale
        self.max_new_tokens = args.max_new_tokens

    def propose(self, request):
        if request.get("format_version") != 1:
            raise ValueError("unsupported policy request format")
        image = observation_image(request["observation"], self.image_scale)
        messages = attach_images(request_messages(request), [image])
        response = generate_json(
            self.model,
            self.processor,
            messages,
            required_key="action",
            max_new_tokens=self.max_new_tokens,
        )
        if response.get("format_version") != 1:
            raise ValueError("model response must use format_version 1")
        return response


def handler(service):
    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            if self.path != "/generate":
                self.send_error(404)
                return
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 < length <= 8 * 1024 * 1024:
                    raise ValueError("request body size is invalid")
                request = json.loads(self.rfile.read(length))
                response = service.propose(request)
                body = json.dumps(response, separators=(",", ":")).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
                body = json.dumps({"error": str(error)}, separators=(",", ":")).encode()
                self.send_response(422)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        def log_message(self, _format, *_args):
            return

    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-model", default="Qwen/Qwen3-VL-2B-Instruct")
    parser.add_argument("--adapter", required=True)
    parser.add_argument("--quantization", choices=("none", "4bit"), default="4bit")
    parser.add_argument("--image-scale", type=int, default=8)
    parser.add_argument("--max-new-tokens", type=int, default=768)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8766)
    args = parser.parse_args()
    if args.image_scale < 1 or args.max_new_tokens < 1:
        parser.error("image scale and max new tokens must be positive")
    service = GeneratorService(args)
    server = HTTPServer((args.host, args.port), handler(service))
    print(f"atelier-lab VLM policy listening on http://{args.host}:{args.port}/generate")
    server.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
