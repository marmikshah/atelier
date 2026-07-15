#!/usr/bin/env python3
"""Run the same atelier task against different models over any OpenAI-compatible API.

atelier is a headless pixel-art editor exposed as an MCP server. This harness
spawns the stdio server, exposes its tools to a chat model over any
OpenAI-compatible `/chat/completions` endpoint, and runs an agentic tool-call
loop until the model stops (or the step budget runs out). Swap `--model` /
`--base-url` to compare models on an identical task; every run is a
self-contained transcript.

Robust + reproducible: each run gets an isolated, freshly-wiped ATELIER_HOME;
requests use temperature 0 + a fixed seed by default and retry with backoff on
429/5xx/timeouts. The record pins every input (model, endpoint, prompt, tools,
temperature, seed) plus tokens and wall-clock duration. Note: endpoints do not
guarantee bit-identical LLM outputs even at temp 0 + seed — the ART is what's
truly deterministic (the recorded tool-call sequence replays byte-identically
via `atelier replay`); the harness makes the *inputs* fixed and *audited*.

Deps: pip install "mcp>=1.0" "openai>=1.40"

Endpoint + key resolve from flags first, then env:
  --base-url  | OPENAI_BASE_URL  (default: https://api.openai.com/v1)
  --api-key   | OPENAI_API_KEY

Examples:
  # OpenAI (default endpoint)
  OPENAI_API_KEY=... python benchmarks/run_task.py \
      --model gpt-4o --task benchmarks/tasks/alien.txt
  # any other OpenAI-compatible endpoint
  OPENAI_BASE_URL=https://host/v1 OPENAI_API_KEY=... \
      python benchmarks/run_task.py --model <id> --task benchmarks/tasks/ball.txt
  # any local/self-hosted server (Ollama, vLLM, LM Studio, …)
  python benchmarks/run_task.py --base-url http://localhost:11434/v1 \
      --api-key dummy --model qwen2.5 --task benchmarks/tasks/cat.txt

Transcript auto-saved to benchmarks/runs/<model>/<task>.json — no --out needed.
Model names are whatever the chosen endpoint expects.
"""

from __future__ import annotations

import argparse
import asyncio
import copy
import json
import os
import re
import shutil
import sys
import time
from pathlib import Path
from typing import Any

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client
from openai import AsyncOpenAI

DEFAULT_BASE_URL = "https://api.openai.com/v1"
REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO_ROOT / "target" / "release" / "atelier"
RUNS_DIR = Path(__file__).resolve().parent / "runs"  # benchmarks/runs/

# The animation requirement, appended to whatever framing the server supplies so
# every run ends in a comparable artifact.
GIF_REQUIREMENT = (
    "This task is ANIMATED. You MUST finish by exporting an animated GIF: call "
    'doc_export with op="anim", format="gif", and an out_path ending in ".gif". '
    "Only stop once that GIF is written, then reply with a short final summary. "
    "Do not ask the user questions."
)

# Fallback framing if the server advertises no `instructions` (kept neutral and
# identical across models — the comparison stays fair).
FALLBACK_FRAMING = (
    "You are an expert pixel artist driving the atelier editor through its MCP "
    "tools. Create the requested art by calling tools, calling doc_look to see the "
    "frame and refining after each burst of edits."
)


def build_system_prompt(server_instructions: str | None) -> str:
    """Use atelier's own server `instructions` as the framing (how real MCP
    clients see it), plus the GIF requirement. Fall back to neutral wording."""
    framing = (server_instructions or "").strip() or FALLBACK_FRAMING
    return f"{framing}\n\n{GIF_REQUIREMENT}"


def _is_gif_export(name: str, call_args: dict[str, Any]) -> bool:
    """True when a tool call actually wrote an animated GIF (doc_export op=anim, gif)."""
    if name != "doc_export" or call_args.get("op") != "anim":
        return False
    fmt = str(call_args.get("format", "gif")).lower()
    out = str(call_args.get("out_path", "")).lower()
    return fmt == "gif" or out.endswith(".gif")


def mcp_tools_to_openai(tools: list[Any]) -> list[dict[str, Any]]:
    """Translate an MCP tool list into OpenAI function-tool schemas. atelier emits
    schemas that strict tool-call parsers accept as-is, so this is a pass-through."""
    return [
        {
            "type": "function",
            "function": {
                "name": t.name,
                "description": (t.description or "")[:1024],
                "parameters": t.inputSchema or {"type": "object", "properties": {}},
            },
        }
        for t in tools
    ]


def _slug(text: str) -> str:
    """Filesystem-safe slug for a model name, e.g. 'Claude-Opus-4.1' -> 'claude-opus-4.1'."""
    return re.sub(r"[^A-Za-z0-9._-]+", "-", text).strip("-").lower() or "model"


def _strip_images(messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Replace base64 image data-URIs with a light placeholder so the saved
    conversation stays replayable without carrying megabytes of PNG per frame."""
    out: list[dict[str, Any]] = []
    for m in messages:
        content = m.get("content")
        if isinstance(content, list):
            parts = []
            for part in content:
                if isinstance(part, dict) and part.get("type") == "image_url":
                    parts.append({"type": "image_url", "image_url": {"url": "[image omitted]"}})
                else:
                    parts.append(part)
            out.append({**m, "content": parts})
        else:
            out.append(m)
    return out


STALE_LOOK_NOTE = "[earlier doc_look frame — outdated after later edits; call doc_look to see the current art]"


def _demote_stale_images(messages: list[dict[str, Any]]) -> None:
    """Swap image parts already in the live context for TEXT parts, so only the
    newest doc_look frame is ever sent as a real image. Old frames are stale after
    edits and cost tens of thousands of tokens each on every resend — this bounds
    image cost to O(1) per request instead of O(looks). The part's `type` must
    change too: an image_url carrying a non-image URL is rejected by the API."""
    for m in messages:
        c = m.get("content")
        if not isinstance(c, list):
            continue
        m["content"] = [
            {"type": "text", "text": STALE_LOOK_NOTE}
            if isinstance(p, dict) and p.get("type") == "image_url"
            else p
            for p in c
        ]


def render_tool_result(result: Any) -> tuple[str, list[dict[str, Any]]]:
    """Split an MCP tool result into a text blob and any inline images.

    Images (e.g. doc_look frames) are returned separately so they can be fed
    back to a vision model as a follow-up user message.
    """
    texts: list[str] = []
    images: list[dict[str, Any]] = []
    for block in result.content or []:
        kind = getattr(block, "type", None)
        if kind == "text":
            texts.append(block.text)
        elif kind == "image":
            # Extracted here, but only forwarded to the model for doc_look (see
            # the loop). No text note — a dropped preview must not claim a frame.
            data_uri = f"data:{block.mimeType};base64,{block.data}"
            images.append({"type": "image_url", "image_url": {"url": data_uri}})
        else:
            texts.append(json.dumps(getattr(block, "__dict__", str(block)), default=str))
    text = "\n".join(texts) if texts else "(no content)"
    if getattr(result, "isError", False):
        text = f"ERROR: {text}"
    return text, images


async def run(args: argparse.Namespace) -> int:
    base_url = (
        args.base_url
        or os.environ.get("OPENAI_BASE_URL")
        or os.environ.get("OPENAI_API_URL")
        or DEFAULT_BASE_URL
    )
    api_key = args.api_key or os.environ.get("OPENAI_API_KEY")
    if not api_key:
        sys.exit(f"No API key. Pass --api-key or set OPENAI_API_KEY. Endpoint: {base_url}")

    binary = Path(args.binary)
    if not binary.exists():
        sys.exit(f"atelier binary missing: {binary}\nBuild it: make release")

    task_is_file = Path(args.task).exists()
    task = Path(args.task).read_text() if task_is_file else args.task
    task_name = Path(args.task).stem if task_is_file else "inline"
    out = Path(args.out) if args.out else RUNS_DIR / _slug(args.model) / f"{task_name}.json"

    # Isolated, freshly-wiped document store per run — no cross-run contamination,
    # so a run's inputs are exactly (model, endpoint, task, prompt, tools).
    home = Path(args.home) if args.home else out.parent / f"{task_name}-home"
    shutil.rmtree(home, ignore_errors=True)
    home.mkdir(parents=True, exist_ok=True)

    # Built-in retries with backoff on transient errors (429/5xx/timeouts); a
    # per-request timeout keeps a hung endpoint from stalling the whole run.
    client = AsyncOpenAI(
        api_key=api_key,
        base_url=base_url,
        timeout=args.timeout,
        max_retries=args.max_retries,
    )

    server = StdioServerParameters(
        command=str(binary),
        args=[],
        env={**os.environ, "ATELIER_PROFILE": args.profile, "ATELIER_HOME": str(home)},
    )

    # Determinism knobs sent on every completion; seed omitted when < 0 (endpoints
    # that don't support it would otherwise reject the request).
    sampling: dict[str, Any] = {"temperature": args.temperature}
    if args.seed >= 0:
        sampling["seed"] = args.seed

    # Every LLM turn and every tool call+result, in order — the replayable record.
    events: list[dict[str, Any]] = []
    exports: list[dict[str, Any]] = []
    tool_calls = look_iterations = step = nudges = 0
    # cumulative = raw sum across every request (inflated by resending the static
    # prefix each turn); peak_context = the largest single request (comparable to
    # what an MCP client like Claude Code reports); cached = prefix tokens the
    # endpoint served from cache (0 if it doesn't support prompt caching).
    tokens = {"prompt": 0, "completion": 0, "total": 0, "peak_context": 0, "cached": 0}
    gif_exported = False
    stop_reason = "unknown"
    system_prompt = ""

    async with stdio_client(server) as (read, write):
        async with ClientSession(read, write) as session:
            init = await session.initialize()
            system_prompt = build_system_prompt(getattr(init, "instructions", None))
            mcp_tools = (await session.list_tools()).tools
            tools = mcp_tools_to_openai(mcp_tools)
            seed_str = args.seed if args.seed >= 0 else "off"
            print(
                f"[atelier] {len(tools)} tools · profile={args.profile} · "
                f"{args.model} @ {base_url} · temp={args.temperature} seed={seed_str} · "
                f"home={home}",
                file=sys.stderr,
            )

            messages: list[dict[str, Any]] = [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": task},
            ]

            started = time.time()
            clock = time.monotonic()

            # Uncapped by default: run until the model stops AND a GIF exists.
            # --max-steps > 0 is an optional safety valve, off (0) by default.
            while True:
                step += 1
                if args.max_steps and step > args.max_steps:
                    stop_reason = "max_steps"
                    print(f"\n[stopped] hit --max-steps {args.max_steps}", file=sys.stderr)
                    break

                resp = await client.chat.completions.create(
                    model=args.model,
                    messages=messages,
                    tools=tools,
                    tool_choice="auto",
                    **sampling,
                )
                if resp.usage:
                    pt = resp.usage.prompt_tokens or 0
                    tokens["prompt"] += pt
                    tokens["completion"] += resp.usage.completion_tokens or 0
                    tokens["total"] += resp.usage.total_tokens or 0
                    tokens["peak_context"] = max(tokens["peak_context"], pt)
                    details = getattr(resp.usage, "prompt_tokens_details", None)
                    tokens["cached"] += getattr(details, "cached_tokens", 0) or 0

                msg = resp.choices[0].message
                messages.append(msg.model_dump(exclude_none=True))
                events.append(
                    {
                        "step": step,
                        "role": "assistant",
                        "content": msg.content,
                        "tool_calls": [
                            {"id": c.id, "name": c.function.name, "arguments": c.function.arguments}
                            for c in (msg.tool_calls or [])
                        ],
                    }
                )

                if not msg.tool_calls:
                    if gif_exported:
                        stop_reason = "done_with_gif"
                        print(f"\n[done @ step {step}] {msg.content or ''}", file=sys.stderr)
                        break
                    if nudges >= args.max_nudges:
                        stop_reason = "stopped_without_gif"
                        print(f"\n[stopped] model quit without a GIF after {nudges} nudges", file=sys.stderr)
                        break
                    nudges += 1
                    print(f"[nudge {nudges}] no GIF yet — asking model to export", file=sys.stderr)
                    messages.append(
                        {
                            "role": "user",
                            "content": (
                                "You have not exported an animated GIF yet. Finish the animation, "
                                'then call doc_export with op="anim", format="gif", and an out_path '
                                'ending in ".gif". Do that before you stop.'
                            ),
                        }
                    )
                    continue

                pending_images: list[dict[str, Any]] = []
                for call in msg.tool_calls:
                    name = call.function.name
                    tool_calls += 1
                    look_iterations += name == "doc_look"
                    try:
                        call_args = json.loads(call.function.arguments or "{}")
                    except json.JSONDecodeError as e:
                        text, imgs, is_err = f"ERROR: bad tool arguments: {e}", [], True
                    else:
                        print(f"[step {step}] {name}({', '.join(call_args)})", file=sys.stderr)
                        result = await session.call_tool(name, call_args)
                        text, imgs = render_tool_result(result)
                        is_err = bool(getattr(result, "isError", False))
                        if name in ("doc_export", "export_all", "export_atlas") and not is_err:
                            exports.append({"tool": name, "args": call_args})
                            gif_exported = gif_exported or _is_gif_export(name, call_args)
                    messages.append(
                        {"role": "tool", "tool_call_id": call.id, "content": text[: args.max_tool_chars]}
                    )
                    events.append(
                        {
                            "step": step,
                            "role": "tool",
                            "tool_call_id": call.id,
                            "name": name,
                            "arguments": call_args if not is_err else call.function.arguments,
                            "result": text[: args.max_tool_chars],
                            "is_error": is_err,
                            "images": len(imgs),
                        }
                    )
                    # The agent is BLIND unless it looks: only doc_look's frame is
                    # fed back as an image. Other tools (paint_grid, ref, snap…)
                    # return inline previews for convenience, but forwarding those
                    # defeats the see-and-fix loop and explodes image-token cost.
                    if name == "doc_look":
                        pending_images.extend(imgs)

                if pending_images:
                    # Only the newest doc_look frame stays a live image. Earlier
                    # looks are STALE — the art changed after each edit — so
                    # re-sending them on every request is pure token waste (an
                    # upscaled frame is tens of thousands of tokens). Demote them
                    # to a placeholder; the model looks again when it needs to see.
                    _demote_stale_images(messages)
                    messages.append({"role": "user", "content": pending_images})

    duration_ms = round((time.monotonic() - clock) * 1000)
    record = {
        "model": args.model,
        "base_url": base_url,
        "profile": args.profile,
        "task_name": task_name,
        "task": task,
        "system_prompt": system_prompt,
        # Everything needed to reproduce the run's inputs.
        "determinism": {
            "temperature": args.temperature,
            "seed": args.seed if args.seed >= 0 else None,
            "timeout_s": args.timeout,
            "max_retries": args.max_retries,
            "home": str(home),
            "started_unix": round(started),
        },
        "metrics": {
            "stop_reason": stop_reason,
            "steps": events[-1]["step"] if events else 0,
            "tool_calls": tool_calls,
            "look_iterations": look_iterations,
            "tokens": tokens,
            "exports": len(exports),
            "gif_exported": gif_exported,
            "nudges": nudges,
            "duration_ms": duration_ms,
        },
        "exports": exports,
        "events": events,
        # Full replayable conversation; image data-URIs stripped to keep it light.
        "messages": _strip_images(messages),
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(record, indent=2, default=str))
    print(
        f"[saved] {out}  ({tool_calls} tool calls · {look_iterations} looks · "
        f"peak {tokens['peak_context']} ctx · {tokens['total']} cumulative · "
        f"{tokens['cached']} cached · {stop_reason})",
        file=sys.stderr,
    )
    return 0


TASKS_DIR = Path(__file__).resolve().parent / "tasks"


async def run_full(args: argparse.Namespace) -> int:
    """Run EVERY task in benchmarks/tasks/ against one model, in parallel.
    Warns and asks for confirmation first — it is a lot of tokens."""
    tasks = sorted(TASKS_DIR.glob("*.txt"))
    if not tasks:
        sys.exit(f"no tasks found in {TASKS_DIR}")
    n = len(tasks)

    if not args.yes:
        sys.stderr.write(
            f"\n  ⚠️  --full runs ALL {n} tasks against '{args.model}' IN PARALLEL.\n"
            f"      Each is an uncapped agentic loop that can spend tens of thousands\n"
            f"      of tokens; {n} at once multiplies the cost and the rate-limit\n"
            f"      pressure. This can be expensive.\n\n"
            f"      Tasks: {', '.join(b.stem for b in tasks)}\n\n"
        )
        try:
            ans = input(f"  Run all {n} tasks in parallel? [y/N] ").strip().lower()
        except EOFError:
            ans = ""
        if ans not in ("y", "yes"):
            sys.exit("aborted.")

    async def one(task_path: Path) -> int:
        a = copy.copy(args)
        a.task = str(task_path)
        try:
            return await run(a)
        except Exception as e:  # one task failing must not sink the rest
            print(f"[{task_path.stem}] FAILED: {e}", file=sys.stderr)
            return 1

    results = await asyncio.gather(*(one(b) for b in tasks))
    ok = sum(r == 0 for r in results)
    print(f"\n[full] {ok}/{n} tasks completed for {args.model}", file=sys.stderr)
    return 0 if ok == n else 1


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--model", required=True, help="Model id/name the endpoint expects, e.g. gpt-4o")
    p.add_argument("--task", help="Task prompt string, or path to a .txt file (omit with --full)")
    p.add_argument(
        "--full",
        action="store_true",
        help="Run EVERY task in benchmarks/tasks/ in parallel (prompts for confirmation first)",
    )
    p.add_argument(
        "-y",
        "--yes",
        action="store_true",
        help="Skip the --full confirmation prompt (non-interactive)",
    )
    p.add_argument(
        "--base-url",
        default=None,
        help=f"OpenAI-compatible base URL (default: $OPENAI_BASE_URL or {DEFAULT_BASE_URL})",
    )
    p.add_argument(
        "--api-key",
        default=None,
        help="API key (default: $OPENAI_API_KEY)",
    )
    p.add_argument(
        "--out",
        default=None,
        help="Transcript path (default: benchmarks/runs/<model>/<task>.json, derived automatically)",
    )
    p.add_argument("--binary", default=str(DEFAULT_BINARY), help="Path to the atelier binary")
    p.add_argument(
        "--home",
        default=None,
        help="ATELIER_HOME for this run (default: an isolated, wiped dir next to --out)",
    )
    p.add_argument("--profile", default="full", choices=["core", "full"], help="ATELIER_PROFILE tool set")
    p.add_argument(
        "--max-steps",
        type=int,
        default=0,
        help="Safety cap on tool-call rounds; 0 = uncapped (run until GIF is exported)",
    )
    p.add_argument(
        "--max-nudges",
        type=int,
        default=3,
        help="Times to re-prompt the model to export a GIF if it stops without one",
    )
    p.add_argument("--max-tool-chars", type=int, default=6000, help="Truncate each tool result to N chars")
    # Determinism / robustness knobs.
    p.add_argument(
        "--temperature",
        type=float,
        default=0.0,
        help="Sampling temperature (default 0.0 for the most repeatable runs)",
    )
    p.add_argument(
        "--seed",
        type=int,
        default=0,
        help="Sampling seed sent to the endpoint (default 0); pass -1 to omit it",
    )
    p.add_argument(
        "--timeout",
        type=float,
        default=120.0,
        help="Per-request timeout in seconds (default 120)",
    )
    p.add_argument(
        "--max-retries",
        type=int,
        default=4,
        help="SDK retries with backoff on 429/5xx/timeout (default 4)",
    )
    args = p.parse_args()
    if args.full:
        sys.exit(asyncio.run(run_full(args)))
    if not args.task:
        p.error("--task is required (or pass --full to run every task)")
    sys.exit(asyncio.run(run(args)))


if __name__ == "__main__":
    main()
