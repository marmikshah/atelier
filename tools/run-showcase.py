#!/usr/bin/env python3
"""Run the frozen showcase briefs in fresh, isolated Codex sessions."""

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
WRAPPER = '''#!/usr/bin/env python3
import json
import os
import sys

args = sys.argv[1:]
if not args or args[0] not in ("call", "tools", "skills", "--version"):
    sys.exit("Use call, tools, or skills for benchmark artwork.")
if args[0] == "call":
    if "--home" in args:
        sys.exit("The benchmark wrapper supplies the isolated home.")
    with open(os.environ["ATELIER_BENCHMARK_LOG"], "a") as log:
        log.write(json.dumps({"tool": args[1] if len(args) > 1 else None,
                              "args": args[2:]}) + "\\n")
    args += ["--home", os.environ["ATELIER_BENCHMARK_HOME"]]
os.execv(os.environ["ATELIER_BENCHMARK_BINARY"], ["atelier", *args])
'''
SCHEMA = {
    "type": "object",
    "properties": {
        "doc_id": {"type": "string"},
        "summary": {"type": "string"},
    },
    "required": ["doc_id", "summary"],
    "additionalProperties": False,
}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    temporary = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def atelier_call(binary, home, tool, args):
    result = subprocess.run(
        [str(binary), "call", tool, json.dumps(args), "--home", str(home)],
        check=True, capture_output=True, text=True,
    )
    return json.loads(result.stdout)


def session_usage(thread_id):
    """Read cumulative usage for this session, including interrupted turns."""
    sessions = Path(os.environ.get("CODEX_HOME", Path.home() / ".codex")) / "sessions"
    usage = None
    settings = []
    for path in sessions.rglob(f"*{thread_id}*.jsonl"):
        for line in path.read_text().splitlines():
            event = json.loads(line)
            payload = event.get("payload", {})
            if event.get("type") == "turn_context":
                settings.append({"model": payload.get("model"), "effort": payload.get("effort")})
            if event.get("type") == "event_msg" and payload.get("type") == "token_count":
                info = payload.get("info") or {}
                usage = info.get("total_token_usage") or usage
    return usage, settings


def run_task(task, args, manifest, binary, wrapper, schema):
    directory = args.output / task
    complete = directory / "complete.json"
    if args.resume and complete.is_file():
        result = json.loads(complete.read_text())
        if result["provenance"] != manifest:
            raise RuntimeError(f"{task}: completed run has different provenance")
        if (digest(directory / f"{task}.gif") != result["gif_sha256"]
                or digest(directory / f"{task}.jsonl") != result["replay_sha256"]):
            raise RuntimeError(f"{task}: completed artifacts changed")
        print(f"{task}: retaining completed run", flush=True)
        return result
    continuing = directory.exists()
    if continuing and not args.resume:
        raise RuntimeError(f"{task}: partial run exists; use --resume to continue its session")
    directory.mkdir(exist_ok=True)
    home = directory / "home"
    home.mkdir(exist_ok=True)
    brief = ROOT / "showcase" / "tasks" / f"{task}.txt"
    prompt = f'''Create the following animated pixel-art sprite using Atelier.

{brief.read_text().strip()}

Load and follow the atelier-sprite skill by reading
{ROOT / "crates/atelier/skills/sprite.md"}.
Use {wrapper} for every Atelier invocation. It supplies this task's isolated
store and records calls. Inspect tool schemas with its tools command as needed.
Create all artwork through Atelier's editing tools. Shell/Python can construct
arguments and loops that call those tools. Use doc_look with --image-out and
your image viewer to inspect the actual images, and follow the skill's drawing
and animation workflow. Save previews and temporary argument files here.

This is an independent benchmark run. Work only on this brief. Do not inspect
other models' art or replay recipes. Do not edit repository source, the frozen
brief, or store files directly, and do not use git or GitHub commands. Do not
read credentials. Finish with exactly one document containing the requested
animation. Return its doc_id and a short summary. The harness exports the final
GIF and verifies its replay; you may export to inspect your own animation.
'''
    if not continuing:
        (directory / "prompt.txt").write_text(prompt)
    environment = dict(os.environ)
    environment.update({
        "ATELIER_BENCHMARK_HOME": str(home),
        "ATELIER_BENCHMARK_BINARY": str(binary),
        "ATELIER_BENCHMARK_LOG": str(directory / "calls.jsonl"),
    })
    command = [
        "codex", "exec", "--ignore-user-config", "--model", args.model,
        "-c", f'model_reasoning_effort="{args.effort}"',
        "-c", 'approval_policy="never"', "--sandbox", "workspace-write",
        "--json", "--output-schema", str(schema),
        "--output-last-message", str(directory / "result.json"),
        "--cd", str(directory), "-",
    ]
    if continuing:
        events = [json.loads(line) for line in (directory / "events.jsonl").read_text().splitlines()]
        thread_id = next(event["thread_id"] for event in events if event.get("type") == "thread.started")
        command = [
            "codex", "exec", "resume", "--ignore-user-config", "--model", args.model,
            "-c", f'model_reasoning_effort="{args.effort}"',
            "-c", 'approval_policy="never"', "-c", 'sandbox_mode="workspace-write"',
            "--json", "--output-schema", str(schema),
            "--output-last-message", str(directory / "result.json"), thread_id, "-",
        ]
        prompt = ("Continue your original showcase task in the same isolated store. "
                  "The previous CLI turn was interrupted; preserve its artwork and follow "
                  "the original brief and tool-only workflow. Complete any remaining checks "
                  "and return the final doc_id and summary in the required JSON format.")
    with (directory / "invocations.jsonl").open("a") as invocations:
        invocations.write(json.dumps({"command": command, **manifest}) + "\n")
    print(f"{task}: {'resuming' if continuing else 'starting'} {args.model} / {args.effort}", flush=True)
    with (directory / "events.jsonl").open("a") as events:
        with (directory / "stderr.log").open("a") as errors:
            process = subprocess.run(
                command, input=prompt, text=True, env=environment,
                stdout=events, stderr=errors, cwd=directory,
            )
    if process.returncode:
        raise RuntimeError(f"{task}: Codex exited {process.returncode}; see {directory}")
    result = json.loads((directory / "result.json").read_text())
    documents = atelier_call(binary, home, "list_docs", {})["documents"]
    if len(documents) != 1 or documents[0]["doc_id"] != result["doc_id"]:
        raise RuntimeError(f"{task}: expected exactly the reported document")
    info = atelier_call(binary, home, "doc_info", {"doc_id": result["doc_id"]})
    source = home / "documents" / result["doc_id"] / "recipe.jsonl"
    recipe = directory / f"{task}.jsonl"
    shutil.copyfile(source, recipe)
    gif = directory / f"{task}.gif"
    atelier_call(binary, home, "doc_export", {
        "doc_id": result["doc_id"], "op": "anim", "format": "gif",
        "scale": 4, "out_path": str(gif),
    })
    replay_home = directory / "replayed"
    with (directory / "replay.log").open("w") as log:
        subprocess.run(
            [str(binary), "replay", str(recipe), "--home", str(replay_home)],
            check=True, stdout=log, stderr=subprocess.STDOUT,
        )
    replay_id = atelier_call(binary, replay_home, "list_docs", {})["documents"][0]["doc_id"]
    replay_gif = directory / "replayed.gif"
    atelier_call(binary, replay_home, "doc_export", {
        "doc_id": replay_id, "op": "anim", "format": "gif",
        "scale": 4, "out_path": str(replay_gif),
    })
    if gif.read_bytes() != replay_gif.read_bytes():
        raise RuntimeError(f"{task}: replay changes the exported GIF")
    calls = [json.loads(line) for line in (directory / "calls.jsonl").read_text().splitlines()]
    usage = None
    thread_id = None
    for line in (directory / "events.jsonl").read_text().splitlines():
        event = json.loads(line)
        if event.get("type") == "thread.started":
            thread_id = event.get("thread_id")
        if event.get("type") == "turn.completed":
            usage = event.get("usage")
    cumulative_usage, settings = session_usage(thread_id)
    if cumulative_usage:
        usage = cumulative_usage
    if not settings or any(setting != {"model": args.model, "effort": args.effort} for setting in settings):
        raise RuntimeError(f"{task}: session did not confirm the requested model and reasoning effort")
    record = {
        "task": task, "model": args.label, "vendor": "OpenAI",
        "model_id": args.model, "reasoning_effort": args.effort,
        "doc_id": result["doc_id"], "summary": result["summary"],
        "tool_calls": len(calls),
        "looks": sum(call["tool"] == "doc_look" for call in calls),
        "tokens": usage["input_tokens"] + usage["output_tokens"] if usage else None,
        "usage": usage, "thread_id": thread_id, "settings": settings, "document": info,
        "gif_sha256": digest(gif), "replay_sha256": digest(recipe),
        "provenance": manifest,
    }
    write_json(complete, record)
    print(f"{task}: complete, {len(calls)} calls, replay matches", flush=True)
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", required=True, help="Codex model id")
    parser.add_argument("--effort", required=True,
                        choices=["low", "medium", "high", "xhigh", "max"])
    parser.add_argument("--label", help="Showcase model label; defaults to MODEL-EFFORT")
    parser.add_argument("--jobs", type=int, default=3, help="Concurrent sessions (default: 3)")
    parser.add_argument("--tasks", nargs="+", help="Subset of the frozen task names")
    parser.add_argument("--output", type=Path, help="Ignored artifact directory")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/atelier")
    parser.add_argument("--resume", action="store_true", help="Retain completed tasks and continue interrupted sessions")
    args = parser.parse_args()
    args.label = args.label or f"{args.model}-{args.effort}"
    if not args.label or any(c not in "abcdefghijklmnopqrstuvwxyz0123456789.-" for c in args.label):
        parser.error("label must contain only lowercase letters, digits, dots, and hyphens")
    if args.jobs < 1:
        parser.error("jobs must be positive")
    frozen = json.loads((ROOT / "showcase/runs.json").read_text())["tasks"]
    tasks = args.tasks or frozen
    if len(set(tasks)) != len(tasks) or set(tasks) - set(frozen):
        parser.error("tasks must be unique names from showcase/runs.json")
    args.output = (args.output or ROOT / "target/showcase" / args.label).resolve()
    args.binary = args.binary.resolve()
    if not args.binary.is_file() or not os.access(args.binary, os.X_OK):
        parser.error("build Atelier first: cargo build --locked -p atelier")
    args.output.mkdir(parents=True, exist_ok=True)
    manifest = {
        "model_id": args.model, "reasoning_effort": args.effort,
        "atelier_commit": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "atelier_binary_sha256": digest(args.binary),
        "atelier_version": subprocess.check_output([str(args.binary), "--version"], text=True).strip(),
        "codex_version": subprocess.check_output(["codex", "--version"], text=True).strip(),
        "skill_sha256": digest(ROOT / "crates/atelier/skills/sprite.md"),
        "brief_sha256": {task: digest(ROOT / "showcase/tasks" / f"{task}.txt") for task in frozen},
    }
    manifest_path = args.output / "manifest.json"
    if manifest_path.exists():
        if not args.resume or json.loads(manifest_path.read_text()) != manifest:
            parser.error("output already contains a run; use a fresh path or --resume with identical provenance")
    if not manifest_path.exists():
        write_json(manifest_path, manifest)
    binary = args.output / "atelier-binary"
    if not binary.exists():
        shutil.copy2(args.binary, binary)
    if digest(binary) != manifest["atelier_binary_sha256"]:
        parser.error("the pinned binary differs from the run manifest")
    wrapper = args.output / "atelier"
    if wrapper.exists():
        if wrapper.read_text() != WRAPPER:
            parser.error("the benchmark wrapper differs from this runner")
    else:
        wrapper.write_text(WRAPPER)
        wrapper.chmod(0o755)
    schema = args.output / "result-schema.json"
    if schema.exists():
        if json.loads(schema.read_text()) != SCHEMA:
            parser.error("the output schema differs from this runner")
    else:
        write_json(schema, SCHEMA)
    failures = []
    records = []
    with ThreadPoolExecutor(max_workers=args.jobs) as executor:
        futures = {executor.submit(run_task, task, args, manifest, binary, wrapper, schema): task for task in tasks}
        for future in as_completed(futures):
            try:
                records.append(future.result())
            except Exception as error:
                failures.append({"task": futures[future], "error": str(error)})
                print(str(error), file=sys.stderr, flush=True)
    records = [json.loads(path.read_text()) for task in frozen
               if (path := args.output / task / "complete.json").is_file()]
    write_json(args.output / "results.json", records)
    write_json(args.output / "failures.json", failures)
    return bool(failures)


if __name__ == "__main__":
    sys.exit(main())
