#!/usr/bin/env python3
"""modelbench — run atelier's animation benchmark against any tool-capable model.

Every model is handed the SAME plain-text brief and drives atelier's MCP tools
through an OpenAI-compatible chat API (default: Poe). Each doc_look image is fed
back as vision input, so the model sees and corrects its own work. Each brief is
a ~1-second pixel-art animation loop.

The run records OBJECTIVE metrics only — wall-clock time, tool calls, look
iterations, API round-trips, and prompt/completion/total tokens — plus the full
tool-call trace. No quality judgement is made; the exported art and the numbers
are published for the reader to judge. Results are written to
docs/showcase/runs.json, which the benchmark page (index.html) reads.

  # one model, every brief, into the live benchmark
  POE_API_KEY=... tools/benchmark/modelbench.py run \\
      --model claude-opus-4.8 --label "Claude Opus 4.8" --vendor Anthropic

  # a single brief
  ... run --model gpt-5.4 --label "GPT-5.4" --vendor OpenAI --briefs ball

  # discover model ids the key can reach
  POE_API_KEY=... tools/benchmark/modelbench.py list-models | grep -i claude

Any OpenAI-compatible endpoint works: pass --base-url / --key-env. The atelier
release binary is auto-built if missing (or point at one with --binary).
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SHOWCASE = REPO / "docs" / "showcase"
TRACES = SHOWCASE / "traces"
RUNS_JSON = SHOWCASE / "runs.json"
DEFAULT_BINARY = REPO / "target" / "release" / "atelier"
MAX_STEPS = 120

# The tool allow-list every animation brief shares (parity across models).
ANIM_TOOLS = ["doc_create", "doc_draw", "doc_batch", "doc_paint_grid", "doc_look",
              "doc_fx", "doc_set_palette", "doc_frame", "doc_frame_diff",
              "doc_silhouette", "doc_add_tag", "doc_critique", "doc_export"]

# Shared framing appended to every brief: 8 frames at ~125ms = a ~1s loop, the
# see-and-fix discipline, and the two exports the page needs.
_LOOP = """\n\nRULES for all briefs:
- Build exactly 8 FRAMES and set each frame's duration to ~125ms (doc_frame op="duration") so the exported loop runs about ONE SECOND.
- Lock your palette with doc_set_palette FIRST and keep every pixel on it.
- doc_look after every drawing burst and study the image before continuing; do at least THREE look-and-fix passes across the animation.
- Every consecutive doc_frame_diff pair must report changed pixels (no dead frames), and the loop must close (frame 7 leads back into frame 0).
- doc_critique frame 0 and fix what it flags. doc_add_tag "{tag}" from 0 to 7 direction "forward".
- Export BOTH: doc_export op "anim" out_path "{out}.gif" scale {scale}, and op "sheet" out_path "{out}.png" scale {scale}.
Your final text must be raw data only: doc id, look iterations, and the per-pair frame_diff counts."""

BRIEFS = {
    "ball": {
        "canvas": (32, 32), "scale": 6, "tag": "bounce",
        "text": """TASK — animate a BOUNCING RUBBER BALL, a smooth ~1-second loop.
A single round ball bounces in place on a ground line at y=26. Over the loop it
FALLS from the top, SQUASHES wide-and-flat on ground contact, REBOUNDS tall-and-
narrow, rises to the top, and returns — classic squash-and-stretch with the
apparent VOLUME kept constant. Give it a simple highlight and a 1px outline, and
a soft CAST SHADOW on the ground that grows as the ball nears and shrinks as it
rises. Use exactly 4 colours (ball dark, ball light, highlight, shadow/outline).""",
    },
    "slash": {
        "canvas": (48, 48), "scale": 4, "tag": "slash",
        "text": """TASK — animate a small armored HERO doing a SWORD SLASH, side view, a ~1s loop.
The hero faces RIGHT with feet planted on a ground line at y=42, roughly 22px
tall (chunky, readable silhouette), holding a 12-14px sword clearly attached to
the leading hand. Over the loop: idle → WIND UP (blade raised back over the
shoulder, body coiled) → overhead → a downward diagonal SLASH to the front with a
2-3px motion-smear trailing the blade → follow-through low → recover to idle. The
body must stay ONE connected silhouette every frame (check with doc_silhouette —
one blob, no floating sword). Use exactly 6 colours (outline, skin, tunic,
boots/hilt brown, blade bright, blade shade).""",
    },
    "alien": {
        "canvas": (32, 32), "scale": 6, "tag": "idle",
        "text": """TASK — animate a little green SPACE ALIEN idling, a ~1s loop.
A big-headed alien centered on the canvas: rounded head, two large dark eyes, a
small body, and two thin antennae with round tips. It hovers just above the
ground. Over the loop: it BOBS gently up and down (1-2px), the ANTENNAE sway,
and it BLINKS once (eyes briefly close to a thin line). Keep it symmetric left-
to-right and centered. Use exactly 4 colours (body main, body shade, eye/dark,
outline).""",
    },
    "potion": {
        "canvas": (32, 32), "scale": 6, "tag": "bubble",
        "text": """TASK — animate a BUBBLING POTION BOTTLE, a ~1s loop.
A rounded glass flask with a narrower neck, a cork stopper, liquid filling about
two thirds with a lighter meniscus line and a glass highlight, light from the
top-left, and a 1px outline. The bottle itself stays still; ANIMATE the liquid:
2-3 small BUBBLES rise from the bottom through the liquid and pop at the surface
over the loop, and the liquid surface WOBBLES slightly. Only the bubbles and the
surface move frame to frame. Use exactly 6 colours (glass, liquid dark, liquid
light, bubble/highlight, cork dark, cork light).""",
    },
    "cat": {
        "canvas": (40, 40), "scale": 5, "tag": "cast",
        "text": """TASK — animate a WIZARD CAT casting a spell with a STAFF, a ~1s loop.
A seated cat wearing a tall pointed wizard hat, holding a staff topped with a
round glowing ORB. The cat's body stays put; ANIMATE the magic: the orb PULSES
brighter and a touch larger and back, casting a soft glow that grows and fades;
the cat's TAIL sways; and one or two SPARKLES drift upward from the orb and fade.
The cat should read clearly (ears, hat, eyes, staff). Use exactly 6 colours (cat
fur, fur shade, hat/robe, outline, orb glow bright, orb glow dim).""",
    },
}
# Stable art filename stem per brief.
STEM = {k: k for k in BRIEFS}


# --------------------------------------------------------------------------- #
# atelier MCP stdio client
# --------------------------------------------------------------------------- #
class Mcp:
    def __init__(self, binary, home):
        env = dict(os.environ, ATELIER_HOME=str(home))
        self.p = subprocess.Popen([str(binary)], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, env=env)
        self.next_id = 0
        self.rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                "clientInfo": {"name": "modelbench", "version": "2"}})
        self._notify("notifications/initialized", {})

    def _send(self, obj):
        self.p.stdin.write((json.dumps(obj) + "\n").encode())
        self.p.stdin.flush()

    def _notify(self, method, params):
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def rpc(self, method, params):
        self.next_id += 1
        rid = self.next_id
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("atelier MCP server closed unexpectedly")
            msg = json.loads(line)
            if msg.get("id") == rid:
                if "error" in msg:
                    raise RuntimeError(f"MCP error: {msg['error']}")
                return msg["result"]

    def tools(self):
        return self.rpc("tools/list", {})["tools"]

    def call(self, name, args):
        return self.rpc("tools/call", {"name": name, "arguments": args})

    def close(self):
        try:
            self.p.terminate()
        except Exception:
            pass


def content_of(result):
    texts, images = [], []
    for c in result.get("content", []):
        if c.get("type") == "text":
            texts.append(c.get("text", ""))
        elif c.get("type") == "image":
            images.append(c.get("data", ""))
    return "\n".join(texts) or "(ok)", images


# --------------------------------------------------------------------------- #
# schema sanitising (provider tool-parsers are pickier than the spec)
# --------------------------------------------------------------------------- #
BATCH_PARAMS = {
    "type": "object",
    "properties": {
        "doc_id": {"type": "string"},
        "layer": {"type": "integer"},
        "frame": {"type": "integer"},
        "ops": {"type": "array",
                "description": 'Drawing ops, each an object like {"op":"rect",...}.',
                "items": {"type": "object"}},
    },
    "required": ["doc_id", "layer", "frame", "ops"],
}
STRIP_KEYS = ("format", "$schema", "additionalProperties")


def sanitize(node):
    if isinstance(node, dict):
        return {k: sanitize(v) for k, v in node.items() if k not in STRIP_KEYS}
    if isinstance(node, list):
        return [sanitize(v) for v in node]
    return node


# --------------------------------------------------------------------------- #
# OpenAI-compatible chat
# --------------------------------------------------------------------------- #
def api_key(args):
    # An explicit --key-file wins over the environment (so a stale exported
    # key can't shadow the one you just passed).
    if args.key_file:
        return Path(args.key_file).read_text().strip()
    key = os.environ.get(args.key_env)
    if not key:
        sys.exit(f"no API key — set ${args.key_env} or pass --key-file")
    return key


def post(url, key, payload):
    req = urllib.request.Request(
        url, data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {key}"})
    for attempt in range(4):
        try:
            with urllib.request.urlopen(req, timeout=300) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            body = e.read().decode()[:400]
            if e.code in (429, 500, 502, 503) and attempt < 3:
                time.sleep(15 * (attempt + 1))
                continue
            raise RuntimeError(f"HTTP {e.code}: {body}")


def list_models(args):
    url = args.base_url.rstrip("/") + "/models"
    req = urllib.request.Request(url, headers={"Authorization": f"Bearer {api_key(args)}"})
    with urllib.request.urlopen(req, timeout=60) as r:
        for m in json.loads(r.read()).get("data", []):
            print(m.get("id"))


# --------------------------------------------------------------------------- #
# a single (model, brief) run
# --------------------------------------------------------------------------- #
def build_brief(kind, out):
    spec = BRIEFS[kind]
    tail = _LOOP.format(tag=spec["tag"], out=out, scale=spec["scale"])
    return spec["text"] + tail


def run_brief(args, key, binary, kind):
    slug = f"bench-{args.slug_base}-{kind}"
    out = SHOWCASE / f"tmp-{slug}"
    spec = BRIEFS[kind]
    home = Path("/tmp") / f"atelier-bench-{slug}"
    shutil.rmtree(home, ignore_errors=True)
    mcp = Mcp(binary, home)
    metrics = {"tool_calls": 0, "look_iterations": 0, "api_calls": 0,
               "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
    trace = []
    t0 = time.time()
    try:
        schemas = {t["name"]: t for t in mcp.tools() if t["name"] in ANIM_TOOLS}
        missing = [n for n in ANIM_TOOLS if n not in schemas]
        if missing:
            raise RuntimeError(f"atelier is missing tools {missing} — rebuild the binary")
        oa_tools = [{"type": "function", "function": {
            "name": n, "description": (s.get("description") or "")[:1024],
            "parameters": BATCH_PARAMS if n == "doc_batch"
            else sanitize(s.get("inputSchema", {"type": "object"}))}}
            for n, s in schemas.items()]

        task = build_brief(kind, out).replace("{slug}", slug)
        messages = [
            {"role": "system", "content":
             "You are a pixel artist driving the atelier editor through tools. Use "
             "ONLY the provided tools. Images returned by doc_look are shown to you "
             "as user messages — study them before continuing. Colours are [r,g,b] "
             "or [r,g,b,a] integer arrays 0-255."},
            {"role": "user", "content": f'Create the document with name "{slug}".\n\n{task}'},
        ]
        nudges = 0
        for step in range(MAX_STEPS):
            resp = post(args.base_url.rstrip("/") + "/chat/completions", key, {
                "model": args.model, "messages": messages,
                "tools": oa_tools, "tool_choice": "auto", "max_tokens": 4096})
            metrics["api_calls"] += 1
            u = resp.get("usage") or {}
            metrics["prompt_tokens"] += u.get("prompt_tokens", 0)
            metrics["completion_tokens"] += u.get("completion_tokens", 0)
            metrics["total_tokens"] += u.get("total_tokens", 0)
            msg = resp["choices"][0]["message"]
            messages.append(msg)
            tcs = msg.get("tool_calls") or []
            if not tcs:
                final = (msg.get("content") or "").strip()
                if (not final or metrics["tool_calls"] < 8) and nudges < 4:
                    nudges += 1
                    messages.append({"role": "user", "content":
                                     "You are not done — continue the TASK with tool "
                                     "calls, export both files at the end, then summarise."})
                    continue
                metrics["duration_ms"] = round((time.time() - t0) * 1000)
                return _record(args, kind, out, spec, metrics, final, trace)
            pending = []
            for tc in tcs:
                name = tc["function"]["name"]
                try:
                    a = json.loads(tc["function"]["arguments"] or "{}")
                except json.JSONDecodeError:
                    messages.append({"role": "tool", "tool_call_id": tc["id"],
                                     "content": "error: arguments were not valid JSON"})
                    continue
                metrics["tool_calls"] += 1
                metrics["look_iterations"] += name == "doc_look"
                try:
                    text, images = content_of(mcp.call(name, a))
                except Exception as e:
                    text, images = f"error: {e}", []
                messages.append({"role": "tool", "tool_call_id": tc["id"], "content": text[:6000]})
                trace.append({"n": metrics["tool_calls"], "tool": name, "args": a,
                              "ok": not text.startswith("error:")})
                pending += images
                print(f"  [{step}] {name}", file=sys.stderr)
            for b64 in pending[-2:]:
                messages.append({"role": "user", "content": [
                    {"type": "text", "text": "(image from your last doc_look)"},
                    {"type": "image_url", "image_url": {"url": f"data:image/png;base64,{b64}"}}]})
            imgs = [i for i, m in enumerate(messages) if isinstance(m.get("content"), list)]
            for i in imgs[:-2]:
                messages[i] = {"role": "user", "content": "(an earlier doc_look, superseded)"}
        raise RuntimeError(f"{args.model} hit the {MAX_STEPS}-step cap on {kind}")
    finally:
        mcp.close()


def _record(args, kind, out, spec, metrics, final, trace):
    stem = STEM[kind]
    png_src, gif_src = Path(f"{out}.png"), Path(f"{out}.gif")
    if not png_src.exists():
        raise RuntimeError(f"{kind}: the model never exported a sheet ({png_src} missing)")
    name = f"{stem}-{args.slug_base}"
    (SHOWCASE / f"{name}.png").write_bytes(png_src.read_bytes())
    anim = gif_src.exists()
    if anim:
        (SHOWCASE / f"{name}.gif").write_bytes(gif_src.read_bytes())
    (TRACES / f"{name}.trace.json").write_text(json.dumps(
        {"model": args.label, "brief": kind, **metrics, "final": final, "trace": trace}, indent=1))
    for f in (png_src, gif_src):
        f.unlink(missing_ok=True)
    print(f"== {args.label} / {kind}: {metrics['tool_calls']} calls, "
          f"{metrics['look_iterations']} looks, {metrics['total_tokens']} tokens, "
          f"{metrics['duration_ms']}ms", file=sys.stderr)
    return {"brief": kind, "model": args.label, "vendor": args.vendor,
            "base": f"docs/showcase/{name}", "anim": anim,
            "trace": f"docs/showcase/traces/{name}.trace.json", **metrics}


# --------------------------------------------------------------------------- #
# runs.json upsert
# --------------------------------------------------------------------------- #
def upsert(rows):
    data = json.loads(RUNS_JSON.read_text()) if RUNS_JSON.exists() else {"briefs": list(BRIEFS), "runs": []}
    data["briefs"] = list(BRIEFS)
    runs = data["runs"]
    for row in rows:
        for i, r in enumerate(runs):
            if r["model"] == row["model"] and r["brief"] == row["brief"]:
                runs[i] = row
                break
        else:
            runs.append(row)
    order = {k: i for i, k in enumerate(BRIEFS)}
    runs.sort(key=lambda r: (order.get(r["brief"], 9), r["model"]))
    RUNS_JSON.write_text(json.dumps(data, indent=2) + "\n")


def ensure_binary(path):
    if Path(path).exists():
        return path
    print("building the atelier release binary…", file=sys.stderr)
    subprocess.run(["cargo", "build", "--release"], cwd=REPO, check=True)
    return path


def cmd_run(args):
    key = api_key(args)
    binary = ensure_binary(args.binary)
    TRACES.mkdir(parents=True, exist_ok=True)
    briefs = args.briefs.split(",") if args.briefs else list(BRIEFS)
    for b in briefs:
        if b not in BRIEFS:
            sys.exit(f"unknown brief '{b}' — choose from {', '.join(BRIEFS)}")
    done = 0
    for b in briefs:
        print(f"\n=== {args.label} · {b} ===", file=sys.stderr)
        try:
            row = run_brief(args, key, binary, b)
        except Exception as e:
            print(f"!! {args.label} / {b} failed: {e}", file=sys.stderr)
            continue
        upsert([row])  # persist incrementally
        done += 1
    print(f"\n✓ {args.label}: {done}/{len(briefs)} brief(s) → docs/showcase/runs.json")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base-url", default="https://api.poe.com/v1")
    ap.add_argument("--key-env", default="POE_API_KEY")
    ap.add_argument("--key-file")
    sub = ap.add_subparsers(dest="cmd", required=True)
    lm = sub.add_parser("list-models", help="print model ids the key can reach")
    lm.set_defaults(fn=list_models)
    r = sub.add_parser("run", help="run one model across the animation briefs")
    r.add_argument("--model", required=True, help="provider model id (see list-models)")
    r.add_argument("--label", required=True, help='display name, e.g. "Claude Opus 4.8"')
    r.add_argument("--vendor", required=True, help="vendor label, e.g. Anthropic")
    r.add_argument("--briefs", help=f"comma list (default: all: {','.join(BRIEFS)})")
    r.add_argument("--binary", default=str(DEFAULT_BINARY))
    r.set_defaults(fn=cmd_run)
    args = ap.parse_args()
    if args.cmd == "run":
        args.slug_base = re.sub(r"[^a-z0-9]+", "-", args.label.lower()).strip("-")
    args.fn(args)


if __name__ == "__main__":
    main()
